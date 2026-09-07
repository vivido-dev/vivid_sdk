//! The interactive lane and the desktop input records it carries.
//!
//! The lane is a separate authenticated connection precisely so a saturated media track cannot
//! delay a revocation, so this module never shares a writer with the control plane.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::{io, thread};

use vivid_protocol::cbor::Value;
use vivid_protocol::messages::{Envelope, LaneOpen, PayloadMap};
use vivid_protocol::revision::{GrantGeneration, InputEpoch, SurfaceGeneration};
use vivid_protocol::wire::{Connection, ConnectionReader, ConnectionWriter};
use vivid_protocol::{auth, messages};

use crate::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputBindingStatus {
    pub producer_epoch: u64,
    pub grant_generation: u64,
    pub context_id: u64,
    pub surface_id: u64,
    pub surface_generation: u64,
    pub effective_classes: u64,
    pub state: u64,
    pub reason: u64,
    pub watchdog_timeout_us: u64,
}

/// A validated watchdog renewal for the currently effective input grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputLeaseRenewal {
    pub binding: InputTuple,
    pub renewal_sequence: u64,
    pub watchdog_timeout_us: u64,
}

/// A validated revocation or reset of an effective input grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputGrantTermination {
    pub binding: InputTuple,
    pub reason: u64,
}

/// Actionable traffic from an authenticated interactive lane.
///
/// Ordinary input is kept as the exact strict payload until the caller supplies the authoritative
/// current surface dimensions to [`InputLaneEvent::decode_input`]. The resulting [`InputEvent`]
/// still has to pass an [`InputGate`] immediately before the OS injection API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputLaneEvent {
    Input {
        record_type: u16,
        surface_id: u64,
        payload: PayloadMap,
    },
    Renew(InputLeaseRenewal),
    Revoked(InputGrantTermination),
    Reset(InputGrantTermination),
    /// The lane became unusable. Atomically disable injection and release all held input state.
    LaneClosed {
        diagnostic: String,
    },
    Error(PresenterError),
}

impl InputLaneEvent {
    pub fn decode_input(&self, width: u64, height: u64) -> io::Result<InputEvent> {
        let Self::Input {
            record_type,
            surface_id,
            payload,
        } = self
        else {
            return Err(invalid_input("lane event is not an ordinary input event"));
        };
        let value = Value::Map(payload.clone());
        match *record_type {
            messages::KEY_INPUT => Ok(InputEvent::decode_key(*surface_id, &value)?),
            messages::POINTER_MOTION => Ok(InputEvent::decode_motion(
                *surface_id,
                &value,
                width,
                height,
            )?),
            messages::POINTER_BUTTON => Ok(InputEvent::decode_button(
                *surface_id,
                &value,
                width,
                height,
            )?),
            messages::POINTER_AXIS => {
                Ok(InputEvent::decode_axis(*surface_id, &value, width, height)?)
            }
            _ => Err(invalid_data("unknown ordinary input record type")),
        }
    }
}

/// One authenticated interactive-lane generation.
pub struct InputLane {
    pub(crate) writer: ConnectionWriter,
    pub(crate) shared: Arc<PendingInput>,
    pub(crate) lifecycle: Arc<SessionLifecycle>,
    pub(crate) lane_generation: u64,
    pub(crate) next_request_id: AtomicU64,
    pub(crate) offline: bool,
}

impl std::fmt::Debug for InputLane {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InputLane")
            .field("lane_generation", &self.lane_generation)
            .field("closed", &self.shared.closed.load(Ordering::Acquire))
            .finish()
    }
}

impl InputLane {
    pub const fn generation(&self) -> u64 {
        self.lane_generation
    }

    pub fn set_binding(&self, binding: &InputBinding) -> io::Result<InputBindingStatus> {
        self.lifecycle.ensure_active()?;
        binding.validate(binding.surface_id)?;
        if self.shared.closed.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "interactive lane is closed",
            ));
        }
        let request_id = self
            .next_request_id
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map_err(|_| invalid_data("interactive request ID space exhausted"))?;
        let body = Envelope::correlated(request_id, binding.payload())?.encode()?;
        if self.offline {
            self.writer
                .write_record(messages::SET_INPUT_BINDING, 0, binding.surface_id, &body)?;
            return Ok(InputBindingStatus {
                producer_epoch: binding.producer_epoch.get(),
                grant_generation: binding.producer_epoch.get(),
                context_id: binding.context_id,
                surface_id: binding.surface_id,
                surface_generation: binding.surface_generation.get(),
                effective_classes: binding.requested_classes,
                state: u64::from(!binding.disabled()),
                reason: binding.reason,
                watchdog_timeout_us: binding.requested_watchdog_us,
            });
        }
        let (send, receive) = mpsc::channel();
        {
            let mut requests = lock(&self.shared.requests, "input request table")?;
            if self.shared.closed.load(Ordering::Acquire) {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "interactive lane is closed",
                ));
            }
            if requests.insert(request_id, send).is_some() {
                return Err(invalid_data("duplicate interactive request ID"));
            }
        }
        if let Err(error) =
            self.writer
                .write_record(messages::SET_INPUT_BINDING, 0, binding.surface_id, &body)
        {
            let _ = lock(&self.shared.requests, "input request table")?.remove(&request_id);
            return Err(error);
        }
        let record = receive
            .recv()
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "interactive lane dispatcher stopped",
                )
            })?
            .map_err(|message| io::Error::new(io::ErrorKind::BrokenPipe, message))?;
        if record.record_type == messages::ERROR {
            return Err(presenter_error(&record.body)?);
        }
        expect_record(&record, messages::INPUT_BOUND, binding.surface_id)?;
        let payload = decoded_payload(&record)?;
        validate_exact_payload_keys("INPUT_BOUND", &payload, 0..=8)?;
        if required_u64(&payload, 0)? != binding.producer_epoch.get() {
            return Err(invalid_data(
                "INPUT_BOUND returned a different producer input epoch",
            ));
        }
        let status = InputBindingStatus {
            producer_epoch: required_u64(&payload, 0)?,
            grant_generation: required_u64(&payload, 1)?,
            context_id: required_u64(&payload, 2)?,
            surface_id: required_u64(&payload, 3)?,
            surface_generation: required_u64(&payload, 4)?,
            effective_classes: required_u64(&payload, 5)?,
            state: required_u64(&payload, 6)?,
            reason: required_u64(&payload, 7)?,
            watchdog_timeout_us: required_u64(&payload, 8)?,
        };
        if status.grant_generation == 0
            || status.state > 2
            || status.effective_classes & !binding.requested_classes != 0
            || (status.state == 1
                && (status.context_id != binding.context_id
                    || status.surface_id != binding.surface_id
                    || status.surface_generation != binding.surface_generation.get()
                    || status.effective_classes == 0
                    || !(vivid_protocol::input::MIN_WATCHDOG_US
                        ..=vivid_protocol::input::MAX_WATCHDOG_US)
                        .contains(&status.watchdog_timeout_us)))
        {
            return Err(invalid_data(
                "INPUT_BOUND contains an invalid effective grant",
            ));
        }
        Ok(status)
    }

    pub fn take_event(&self) -> io::Result<Option<InputLaneEvent>> {
        Ok(lock(&self.shared.events, "input event queue")?.pop_front())
    }

    pub fn close(&self) -> io::Result<()> {
        close_input_lane(&self.shared, "interactive lane closed");
        Ok(())
    }
}

impl Drop for InputLane {
    fn drop(&mut self) {
        let _ = self.writer.shutdown();
        close_input_lane(&self.shared, "interactive lane dropped");
    }
}

impl Session {
    /// Open one authenticated interactive-lane generation.
    ///
    /// `desktop-input-v1` must have been accepted. Lane loss never recreates an old grant; callers
    /// reconcile state and use a greater producer input epoch before enabling input again.
    pub fn open_input_lane(&self, lane_generation: u64) -> io::Result<InputLane> {
        self.lifecycle.ensure_active()?;
        if !self.supports(DESKTOP_INPUT) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "desktop-input-v1 was not accepted",
            ));
        }
        if lane_generation == 0 {
            return Err(invalid_input("interactive lane generation must be nonzero"));
        }
        let mut nonce = [0; 16];
        random_bytes(&mut nonce)?;
        let tag = auth::lane_tag(
            self.channel_key.expose(),
            self.info.session_id,
            LaneClass::Interactive as u32,
            lane_generation,
            &nonce,
        );
        let open = LaneOpen {
            session_id: self.info.session_id,
            lane_generation,
            client_nonce: nonce,
            authentication_tag: tag,
        };
        let body = zeroize::Zeroizing::new(messages::encode_payload(1, open.payload())?);
        let offline = matches!(&self.control, ControlPlane::Offline { .. });
        let mut connection = if let Some(directory) = &self.trace_dir {
            Connection::trace(
                &directory.join(format!("interactive-{lane_generation}.ndjson")),
                ConnectionKind::Lane,
            )?
        } else if offline {
            Connection::sink(ConnectionKind::Lane)?
        } else if let Some(factory) = &self.connection_factory {
            factory.open(ConnectionKind::Lane, Some(LaneClass::Interactive))?
        } else {
            Connection::open(
                self.endpoints
                    .interactive
                    .as_ref()
                    .ok_or_else(|| invalid_input("missing Vivid interactive endpoint"))?,
                ConnectionKind::Lane,
            )?
        };
        connection.write_record(messages::LANE_OPEN, 0, 0, &body)?;
        let maximum_body = if offline {
            64 * 1024
        } else {
            let reply = connection.read_record()?;
            if reply.record_type == messages::ERROR {
                return Err(presenter_error(&reply.body)?);
            }
            expect_record(&reply, messages::LANE_ACCEPTED, 0)?;
            let accepted = messages::decode_control(&reply.body)?;
            if accepted.request_id != 1 {
                return Err(invalid_data(
                    "LANE_ACCEPTED request ID does not match LANE_OPEN",
                ));
            }
            let payload = accepted.payload;
            validate_exact_payload_keys("LANE_ACCEPTED", &payload, 0..=3)?;
            if required_u64(&payload, 0)? != self.info.session_id
                || required_u64(&payload, 1)? != LaneClass::Interactive as u64
                || required_u64(&payload, 2)? != lane_generation
            {
                return Err(invalid_data(
                    "LANE_ACCEPTED contains the wrong session or generation",
                ));
            }
            required_u32(&payload, 3)?
        };
        if maximum_body == 0 || maximum_body > 64 * 1024 {
            return Err(invalid_data(
                "LANE_ACCEPTED maximum body is outside 1..=65536",
            ));
        }
        connection.set_send_body_limit(maximum_body)?;
        if !offline {
            connection.set_receive_body_limit(maximum_body)?;
        }
        let shared = Arc::new(PendingInput {
            requests: Mutex::new(HashMap::new()),
            events: Mutex::new(VecDeque::new()),
            closed: AtomicBool::new(false),
        });
        self.lifecycle.register_input_lane(&shared)?;
        let writer = if offline {
            connection.writer()
        } else {
            let (reader, writer) = connection.split()?;
            spawn_input_reader(reader, writer.clone(), shared.clone())?;
            writer
        };
        Ok(InputLane {
            writer,
            shared,
            lifecycle: self.lifecycle.clone(),
            lane_generation,
            next_request_id: AtomicU64::new(2),
            offline,
        })
    }
}

pub(crate) fn spawn_input_reader(
    mut reader: ConnectionReader,
    writer: ConnectionWriter,
    pending: Arc<PendingInput>,
) -> io::Result<()> {
    thread::Builder::new()
        .name("vivid-input-reader".into())
        .spawn(move || {
            let result = (|| -> io::Result<()> {
                loop {
                    let record = reader.read_record()?;
                    if !matches!(
                        record.record_type,
                        messages::PING
                            | messages::PONG
                            | messages::ERROR
                            | messages::INPUT_BOUND
                            | messages::INPUT_REVOKED
                            | messages::INPUT_RESET
                            | messages::INPUT_LEASE_RENEW
                            | messages::KEY_INPUT
                            | messages::POINTER_MOTION
                            | messages::POINTER_BUTTON
                            | messages::POINTER_AXIS
                    ) {
                        if record.flags & vivid_protocol::wire::RECORD_OPTIONAL != 0 {
                            continue;
                        }
                        return Err(invalid_data("unexpected required interactive record"));
                    }
                    let envelope = messages::decode_control(&record.body)?;
                    let fatal = record.record_type == messages::ERROR
                        && messages::parse_error_reply(&record.body)?.fatal;
                    if record.record_type == messages::PING {
                        if record.object_id != 0 {
                            return Err(invalid_data("interactive PING has a nonzero object ID"));
                        }
                        writer.write_record(
                            messages::PONG,
                            0,
                            0,
                            &Envelope::new(envelope.request_id, envelope.payload).encode()?,
                        )?;
                        continue;
                    }
                    if envelope.request_id != 0 {
                        let Some(sender) = lock(&pending.requests, "input request table")?
                            .remove(&envelope.request_id)
                        else {
                            return Err(invalid_data(
                                "interactive reply has no matching pending request",
                            ));
                        };
                        let _ = sender.send(Ok(record));
                        if fatal {
                            return Err(invalid_data("presenter sent a fatal interactive error"));
                        }
                        continue;
                    }
                    if fatal {
                        return Err(invalid_data("presenter sent a fatal interactive error"));
                    }
                    let event = match record.record_type {
                        messages::KEY_INPUT
                        | messages::POINTER_MOTION
                        | messages::POINTER_BUTTON
                        | messages::POINTER_AXIS => InputLaneEvent::Input {
                            record_type: record.record_type,
                            surface_id: record.object_id,
                            payload: envelope.payload,
                        },
                        messages::INPUT_LEASE_RENEW => InputLaneEvent::Renew(decode_input_renewal(
                            record.object_id,
                            &envelope.payload,
                        )?),
                        messages::INPUT_REVOKED => {
                            InputLaneEvent::Revoked(decode_input_termination(
                                "INPUT_REVOKED",
                                record.object_id,
                                &envelope.payload,
                            )?)
                        }
                        messages::INPUT_RESET => InputLaneEvent::Reset(decode_input_termination(
                            "INPUT_RESET",
                            record.object_id,
                            &envelope.payload,
                        )?),
                        messages::ERROR => {
                            InputLaneEvent::Error(messages::parse_error_reply(&record.body)?.into())
                        }
                        _ if record.flags & vivid_protocol::wire::RECORD_OPTIONAL != 0 => continue,
                        _ => {
                            return Err(invalid_data(
                                "unexpected required record on interactive lane",
                            ));
                        }
                    };
                    let mut events = lock(&pending.events, "input event queue")?;
                    if events.len() == MAX_INPUT_EVENTS {
                        return Err(invalid_data(
                            "interactive input queue exceeded its safety bound",
                        ));
                    }
                    events.push_back(event);
                }
            })();
            let message = result.err().map_or_else(
                || "interactive lane closed".into(),
                |error| error.to_string(),
            );
            let _ = writer.shutdown();
            close_input_lane(&pending, &message);
        })
        .map(|_| ())
}

pub(crate) fn close_input_lane(pending: &PendingInput, message: &str) {
    pending.closed.store(true, Ordering::Release);
    if let Ok(mut events) = pending.events.lock() {
        events.clear();
        events.push_back(InputLaneEvent::LaneClosed {
            diagnostic: message.to_owned(),
        });
    }
    fail_pending_input(pending, message);
}

pub(crate) fn fail_pending_input(pending: &PendingInput, message: &str) {
    if let Ok(mut requests) = pending.requests.lock() {
        for (_, sender) in requests.drain() {
            let _ = sender.send(Err(message.to_owned()));
        }
    }
}

pub(crate) fn decode_input_tuple(
    schema: &str,
    object_id: u64,
    payload: &PayloadMap,
) -> io::Result<InputTuple> {
    let tuple = InputTuple {
        producer_epoch: InputEpoch::new(required_u64(payload, 0)?),
        grant_generation: GrantGeneration::new(required_u64(payload, 1)?),
        context_id: required_u64(payload, 2)?,
        surface_id: required_u64(payload, 3)?,
        surface_generation: SurfaceGeneration::new(required_u64(payload, 4)?),
    };
    if tuple.producer_epoch == InputEpoch::ZERO
        || tuple.grant_generation == GrantGeneration::ZERO
        || tuple.context_id == 0
        || tuple.surface_id == 0
        || tuple.surface_generation == SurfaceGeneration::ZERO
        || tuple.surface_id != object_id
    {
        return Err(invalid_data(format!(
            "{schema} contains an invalid owner-qualified input tuple"
        )));
    }
    Ok(tuple)
}

pub(crate) fn decode_input_renewal(
    object_id: u64,
    payload: &PayloadMap,
) -> io::Result<InputLeaseRenewal> {
    validate_exact_payload_keys("INPUT_LEASE_RENEW", payload, 0..=6)?;
    let renewal_sequence = required_u64(payload, 5)?;
    let watchdog_timeout_us = required_u64(payload, 6)?;
    if renewal_sequence == 0
        || !(vivid_protocol::input::MIN_WATCHDOG_US..=vivid_protocol::input::MAX_WATCHDOG_US)
            .contains(&watchdog_timeout_us)
    {
        return Err(invalid_data(
            "INPUT_LEASE_RENEW has an invalid sequence or watchdog",
        ));
    }
    Ok(InputLeaseRenewal {
        binding: decode_input_tuple("INPUT_LEASE_RENEW", object_id, payload)?,
        renewal_sequence,
        watchdog_timeout_us,
    })
}

pub(crate) fn decode_input_termination(
    schema: &str,
    object_id: u64,
    payload: &PayloadMap,
) -> io::Result<InputGrantTermination> {
    validate_exact_payload_keys(schema, payload, 0..=5)?;
    let reason = required_u64(payload, 5)?;
    if reason == 0 || reason > 10 {
        return Err(invalid_data(format!("{schema} has an unknown reason")));
    }
    Ok(InputGrantTermination {
        binding: decode_input_tuple(schema, object_id, payload)?,
        reason,
    })
}

#[cfg(test)]
mod audit_tests {
    use super::*;
    #[test]
    fn interactive_optional_and_fatal_records_are_handled_before_dispatch() {
        use std::{io::Cursor, time::Duration};
        use vivid_protocol::wire::RecordHeader;
        let fatal = messages::ErrorReply {
            code: messages::ERROR_BAD_MESSAGE,
            request_id: 0,
            detail: messages::ErrorDetail::new(vec![]).unwrap(),
            fatal: true,
            diagnostic: String::new(),
        }
        .encode()
        .unwrap();
        for (kind, flags, body, accepted) in [
            (
                0x6fff,
                vivid_protocol::wire::RECORD_OPTIONAL,
                vec![0xff],
                true,
            ),
            (messages::ERROR, 0, fatal, false),
        ] {
            let mut input = Vec::new();
            for (index, (record_type, flags, body)) in [
                (messages::LANE_ACCEPTED, 0, vec![]),
                (kind, flags, body),
                (messages::PONG, 0, messages::empty(7)),
            ]
            .into_iter()
            .enumerate()
            {
                input.extend_from_slice(
                    &RecordHeader {
                        body_length: body.len() as u32,
                        record_type,
                        flags,
                        object_id: 0,
                        sequence: index as u64 + 1,
                    }
                    .encode(),
                );
                input.extend_from_slice(&body);
            }
            let mut connection = Connection::from_streams(
                Box::new(Cursor::new(input)),
                Box::new(std::io::sink()),
                ConnectionKind::Lane,
            )
            .unwrap();
            connection.read_record().unwrap();
            let (reader, writer) = connection.split().unwrap();
            let (send, receive) = mpsc::channel();
            let pending = Arc::new(PendingInput {
                requests: Mutex::new(HashMap::from([(7, send)])),
                events: Mutex::new(VecDeque::new()),
                closed: AtomicBool::new(false),
            });
            spawn_input_reader(reader, writer, pending).unwrap();
            assert_eq!(
                receive
                    .recv_timeout(Duration::from_secs(1))
                    .unwrap()
                    .is_ok(),
                accepted
            );
        }
    }
}

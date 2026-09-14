//! The interactive lane and the desktop input records it carries.
//!
//! The lane is a separate authenticated connection precisely so a saturated media track cannot
//! delay a revocation, so this module never shares a writer with the control plane.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};
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

    /// Take the next lane event, waiting up to `timeout` for one to arrive.
    ///
    /// Returns `None` on timeout, and `None` once [`InputLaneEvent::LaneClosed`] has been taken.
    /// Input is the one stream where polling is genuinely costly — a pointer-motion burst arrives
    /// far faster than any sensible poll interval, and a slow reader is what overruns
    /// `MAX_INPUT_EVENTS` — so a binding should drive its lane from here rather than
    /// [`InputLane::take_event`].
    pub fn wait_event(&self, timeout: Duration) -> io::Result<Option<InputLaneEvent>> {
        let mut events = lock(&self.shared.events, "input event queue")?;
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| crate::invalid_input("event timeout is out of range"))?;
        loop {
            if let Some(event) = events.pop_front() {
                return Ok(Some(event));
            }
            if self.shared.closed.load(Ordering::Acquire) {
                return Ok(None);
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Ok(None);
            };
            let (guard, _) = self
                .shared
                .events_ready
                .wait_timeout(events, remaining)
                .map_err(|_| invalid_data("input event queue lock was poisoned"))?;
            events = guard;
        }
    }

    pub fn close(&self) -> io::Result<()> {
        close_input_lane(&self.shared, "interactive lane closed");
        self.writer.shutdown()
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
        self.open_interactive_lane(lane_generation, false)
    }

    /// Open a separately authenticated lane carrying typed pane-overlay input.
    pub fn open_overlay_input_lane(&self, lane_generation: u64) -> io::Result<OverlayInputLane> {
        Ok(OverlayInputLane {
            inner: self.open_interactive_lane(lane_generation, true)?,
        })
    }

    fn open_interactive_lane(&self, lane_generation: u64, overlay: bool) -> io::Result<InputLane> {
        self.lifecycle.ensure_active()?;
        let profile = if overlay {
            OVERLAY_INPUT
        } else {
            DESKTOP_INPUT
        };
        if !self.supports(profile) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("{profile} was not accepted"),
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
            overlay_receipts: Mutex::new(HashMap::new()),
            requests: Mutex::new(HashMap::new()),
            events: Mutex::new(VecDeque::new()),
            events_ready: Condvar::new(),
            closed: AtomicBool::new(false),
        });
        self.lifecycle.register_input_lane(&shared)?;
        let writer = if offline {
            connection.writer()
        } else {
            let (reader, writer) = connection.split()?;
            spawn_interactive_reader(reader, writer.clone(), shared.clone(), overlay)?;
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

#[cfg(test)]
pub(crate) fn spawn_input_reader(
    reader: ConnectionReader,
    writer: ConnectionWriter,
    pending: Arc<PendingInput>,
) -> io::Result<()> {
    spawn_interactive_reader(reader, writer, pending, false)
}

fn spawn_interactive_reader(
    mut reader: ConnectionReader,
    writer: ConnectionWriter,
    pending: Arc<PendingInput>,
    overlay: bool,
) -> io::Result<()> {
    thread::Builder::new()
        .name("vivid-input-reader".into())
        .spawn(move || {
            let result = (|| -> io::Result<()> {
                let mut viewport_revision = 0;
                loop {
                    let record = reader.read_record()?;
                    if record.flags & !vivid_protocol::wire::RECORD_OPTIONAL != 0 {
                        return Err(invalid_data("unknown interactive record flags"));
                    }
                    let common = matches!(
                        record.record_type,
                        messages::PING | messages::PONG | messages::ERROR
                    );
                    let allowed = if overlay {
                        common
                            || matches!(
                                record.record_type,
                                messages::OK
                                    | messages::OVERLAY_INPUT_EVENT
                                    | messages::OVERLAY_SUBMISSION_OUTCOME
                                    | messages::OVERLAY_VIEWPORT_CHANGED
                            )
                    } else {
                        matches!(
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
                        )
                    };
                    if !allowed {
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
                        messages::OVERLAY_SUBMISSION_OUTCOME => {
                            let outcome = vivid_protocol::overlay::wire::SubmissionOutcome::decode(
                                record.object_id,
                                &Value::Map(envelope.payload.clone()),
                            )?;
                            if let Some(receipt) =
                                lock(&pending.overlay_receipts, "overlay receipts")?
                                    .remove(&outcome.submission)
                            {
                                receipt.finish(Ok(outcome.outcome));
                                continue;
                            }
                            InputLaneEvent::Input {
                                record_type: record.record_type,
                                surface_id: record.object_id,
                                payload: envelope.payload,
                            }
                        }
                        messages::OVERLAY_VIEWPORT_CHANGED => {
                            let update = vivid_protocol::overlay::wire::ViewportChanged::decode(
                                record.object_id,
                                &Value::Map(envelope.payload.clone()),
                            )?;
                            if update.revision <= viewport_revision {
                                return Err(invalid_data("viewport revision did not advance"));
                            }
                            viewport_revision = update.revision;
                            InputLaneEvent::Input {
                                record_type: record.record_type,
                                surface_id: record.object_id,
                                payload: envelope.payload,
                            }
                        }
                        messages::OVERLAY_INPUT_EVENT => {
                            vivid_protocol::overlay::wire::InputEvent::decode(
                                record.object_id,
                                &Value::Map(envelope.payload.clone()),
                            )?;
                            InputLaneEvent::Input {
                                record_type: record.record_type,
                                surface_id: record.object_id,
                                payload: envelope.payload,
                            }
                        }
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
                    if matches!(
                        event,
                        InputLaneEvent::Input {
                            record_type: messages::OVERLAY_VIEWPORT_CHANGED,
                            ..
                        }
                    ) {
                        events.retain(|e| {
                            !matches!(
                                e,
                                InputLaneEvent::Input {
                                    record_type: messages::OVERLAY_VIEWPORT_CHANGED,
                                    ..
                                }
                            )
                        });
                    }
                    if events.len()
                        >= if overlay {
                            vivid_protocol::overlay::MAX_PENDING_EVENTS
                        } else {
                            MAX_INPUT_EVENTS
                        }
                        || (overlay
                            && events.iter().map(overlay_event_bytes).sum::<usize>()
                                + overlay_event_bytes(&event)
                                > vivid_protocol::overlay::MAX_EVENT_QUEUE_BYTES)
                    {
                        return Err(invalid_data(
                            "interactive input queue exceeded its safety bound",
                        ));
                    }
                    events.push_back(event);
                    drop(events);
                    pending.events_ready.notify_all();
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

/// Actionable overlay input, with identities and revisions preserved at full width.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlayLaneEvent {
    Input(vivid_protocol::overlay::wire::InputEvent),
    Outcome(vivid_protocol::overlay::wire::SubmissionOutcome),
    Viewport(vivid_protocol::overlay::wire::ViewportChanged),
    ConnectionLost { diagnostic: String },
}

#[derive(Debug, Default)]
pub(crate) struct OverlayCompletion {
    result: Mutex<Option<Result<vivid_protocol::overlay::wire::PresentationOutcome, String>>>,
    ready: Condvar,
}
impl OverlayCompletion {
    pub(crate) fn finish(
        &self,
        result: Result<vivid_protocol::overlay::wire::PresentationOutcome, String>,
    ) {
        if let Ok(mut slot) = self.result.lock()
            && slot.is_none()
        {
            *slot = Some(result);
            self.ready.notify_all();
        }
    }
}
/// Opaque submission receipt. Waiting does not consume pointer, key, or viewport events.
#[derive(Debug, Clone)]
pub struct OverlaySubmission {
    pub(crate) identity: vivid_protocol::overlay::wire::Submission,
    completion: Arc<OverlayCompletion>,
}
impl OverlaySubmission {
    pub fn revision(&self) -> u64 {
        self.identity.revision
    }
    pub fn wait(
        &self,
        timeout: Duration,
    ) -> io::Result<Option<vivid_protocol::overlay::wire::PresentationOutcome>> {
        if timeout > Duration::from_secs(60) {
            return Err(invalid_input("submission wait exceeds 60 seconds"));
        }
        let deadline = Instant::now() + timeout;
        let mut result = lock(&self.completion.result, "overlay submission")?;
        loop {
            if let Some(result) = result.as_ref() {
                return result
                    .clone()
                    .map(Some)
                    .map_err(|message| io::Error::new(io::ErrorKind::BrokenPipe, message));
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Ok(None);
            };
            result = self
                .completion
                .ready
                .wait_timeout(result, remaining)
                .map_err(|_| invalid_data("submission lock poisoned"))?
                .0;
        }
    }
}

/// An overlay-only view of an authenticated interactive lane.
/// Capture and renewal never share a writer with scene or asset traffic.
#[derive(Debug)]
pub struct OverlayInputLane {
    inner: InputLane,
}

impl OverlayInputLane {
    pub(crate) fn cancel_submission(
        &self,
        identity: vivid_protocol::overlay::wire::Submission,
        message: &str,
    ) {
        if let Ok(mut receipts) = self.inner.shared.overlay_receipts.lock()
            && let Some(receipt) = receipts.remove(&identity)
        {
            receipt.finish(Err(message.to_owned()));
        }
    }
    pub(crate) fn register_submission(
        &self,
        identity: vivid_protocol::overlay::wire::Submission,
    ) -> io::Result<OverlaySubmission> {
        let mut receipts = lock(&self.inner.shared.overlay_receipts, "overlay receipts")?;
        if self.inner.shared.closed.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "overlay lane closed",
            ));
        }
        if receipts.len() >= 256 {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "too many pending overlay submissions",
            ));
        }
        if receipts.contains_key(&identity) {
            return Err(invalid_input("duplicate submission receipt"));
        }
        let completion = Arc::new(OverlayCompletion::default());
        receipts.insert(identity, completion.clone());
        Ok(OverlaySubmission {
            identity,
            completion,
        })
    }
    pub fn generation(&self) -> u64 {
        self.inner.generation()
    }

    pub fn wait_event(&self, timeout: Duration) -> io::Result<Option<OverlayLaneEvent>> {
        match self.inner.wait_event(timeout)? {
            None => Ok(None),
            Some(InputLaneEvent::Input {
                record_type: messages::OVERLAY_SUBMISSION_OUTCOME,
                surface_id,
                payload,
            }) => Ok(Some(OverlayLaneEvent::Outcome(
                vivid_protocol::overlay::wire::SubmissionOutcome::decode(
                    surface_id,
                    &Value::Map(payload),
                )?,
            ))),
            Some(InputLaneEvent::Input {
                record_type: messages::OVERLAY_VIEWPORT_CHANGED,
                surface_id,
                payload,
            }) => Ok(Some(OverlayLaneEvent::Viewport(
                vivid_protocol::overlay::wire::ViewportChanged::decode(
                    surface_id,
                    &Value::Map(payload),
                )?,
            ))),
            Some(InputLaneEvent::Input {
                record_type: messages::OVERLAY_INPUT_EVENT,
                surface_id,
                payload,
            }) => Ok(Some(OverlayLaneEvent::Input(
                vivid_protocol::overlay::wire::InputEvent::decode(
                    surface_id,
                    &Value::Map(payload),
                )?,
            ))),
            Some(InputLaneEvent::LaneClosed { diagnostic }) => {
                Ok(Some(OverlayLaneEvent::ConnectionLost { diagnostic }))
            }
            Some(InputLaneEvent::Error(error)) => Err(io::Error::other(error)),
            Some(_) => Err(invalid_data("unexpected desktop event on overlay lane")),
        }
    }

    pub fn capture(
        &self,
        capture: vivid_protocol::overlay::wire::Capture,
        timeout: Duration,
    ) -> io::Result<()> {
        self.request(
            messages::OVERLAY_INPUT_CAPTURE,
            capture.address.surface_id,
            capture.payload()?,
            timeout,
        )
    }

    pub fn renew(&self, watchdog_us: u64, timeout: Duration) -> io::Result<()> {
        let renewal = vivid_protocol::overlay::wire::Renew {
            lane_generation: self.generation(),
            watchdog_us,
        };
        self.request(
            messages::OVERLAY_INPUT_RENEW,
            0,
            renewal.payload()?,
            timeout,
        )
    }

    pub fn close(&self) -> io::Result<()> {
        self.inner.close()
    }

    fn request(
        &self,
        record_type: u16,
        object: u64,
        payload: PayloadMap,
        timeout: Duration,
    ) -> io::Result<()> {
        if timeout.is_zero() || timeout > Duration::from_secs(5) {
            return Err(invalid_input(
                "overlay input request timeout must be in (0, 5s]",
            ));
        }
        self.inner.lifecycle.ensure_active()?;
        let id = self
            .inner
            .next_request_id
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| invalid_data("interactive request ID space exhausted"))?;
        let body = Envelope::correlated(id, payload)?.encode()?;
        if self.inner.offline {
            if self.inner.shared.closed.load(Ordering::Acquire) {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "overlay lane is closed",
                ));
            }
            self.inner
                .writer
                .write_record(record_type, 0, object, &body)?;
            return Ok(());
        }
        let (sender, receiver) = mpsc::channel();
        {
            let mut requests = lock(&self.inner.shared.requests, "overlay request table")?;
            if self.inner.shared.closed.load(Ordering::Acquire) {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "overlay lane is closed",
                ));
            }
            if requests.len() >= 64 {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "overlay input request limit reached",
                ));
            }
            requests.insert(id, sender);
        }
        if let Err(error) = self
            .inner
            .writer
            .write_record(record_type, 0, object, &body)
        {
            let _ = self.close();
            return Err(error);
        }
        let record = match receiver.recv_timeout(timeout) {
            Ok(Ok(record)) => record,
            outcome => {
                // A timed-out mutation has an unknown outcome. Retire the whole lane so a late
                // capture reply cannot revive a gesture that the application already cancelled.
                let _ = self.close();
                return Err(match outcome {
                    Err(mpsc::RecvTimeoutError::Timeout) => io::Error::new(
                        io::ErrorKind::TimedOut,
                        "overlay input request timed out; lane retired",
                    ),
                    _ => io::Error::new(io::ErrorKind::BrokenPipe, "overlay input lane closed"),
                });
            }
        };
        if record.record_type == messages::ERROR {
            return Err(presenter_error(&record.body)?);
        }
        let result = (|| {
            expect_record(&record, messages::OK, object)?;
            if !decoded_payload(&record)?.is_empty() {
                return Err(invalid_data("overlay input OK reply must be empty"));
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = self.close();
        }
        result
    }
}

fn overlay_event_bytes(event: &InputLaneEvent) -> usize {
    // The strict codec bounds the fixed fields. Only committed/preedit text is variable-sized.
    let text_bytes = match event {
        InputLaneEvent::Input { payload, .. } => payload
            .iter()
            .find(|(key, _)| *key == 5)
            .and_then(|(_, value)| {
                value
                    .as_text()
                    .or_else(|| value.as_array()?.first()?.as_text())
            })
            .map_or(0, str::len),
        InputLaneEvent::Error(error) => error.diagnostic.len(),
        InputLaneEvent::LaneClosed { diagnostic } => diagnostic.len(),
        _ => 0,
    };
    128 + text_bytes
}

pub(crate) fn close_input_lane(pending: &PendingInput, message: &str) {
    if pending.closed.swap(true, Ordering::AcqRel) {
        return;
    }
    if let Ok(mut events) = pending.events.lock() {
        events.clear();
        events.push_back(InputLaneEvent::LaneClosed {
            diagnostic: message.to_owned(),
        });
    }
    pending.events_ready.notify_all();
    if let Ok(mut receipts) = pending.overlay_receipts.lock() {
        for (_, receipt) in receipts.drain() {
            receipt.finish(Err(message.to_owned()));
        }
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
    fn overlay_receipts_are_repeatable_bounded_and_resolved_on_lane_loss() {
        use vivid_protocol::overlay::wire::{PresentationOutcome, Submission, WindowAddress};
        let mut config = ProducerConfig::offline();
        config.required_profiles.extend([
            TERMINAL_OVERLAY.into(),
            VECTOR_SCENE.into(),
            OVERLAY_INPUT.into(),
        ]);
        config.required_profiles.sort();
        config.required_profiles.dedup();
        let session = Session::connect(config).unwrap();
        let lane = session.open_overlay_input_lane(1).unwrap();
        let identity = Submission {
            address: WindowAddress {
                context_id: 1,
                surface_id: 2,
                generation: 1,
            },
            track_id: 3,
            channel_generation: 1,
            epoch: 1,
            revision: u64::MAX,
        };
        let receipt = lane.register_submission(identity).unwrap();
        assert!(lane.register_submission(identity).is_err());
        assert_eq!(receipt.wait(Duration::ZERO).unwrap(), None);
        assert!(receipt.wait(Duration::from_secs(61)).is_err());
        receipt
            .completion
            .finish(Ok(PresentationOutcome::Presented));
        receipt
            .completion
            .finish(Ok(PresentationOutcome::Superseded));
        assert_eq!(
            receipt.wait(Duration::ZERO).unwrap(),
            Some(PresentationOutcome::Presented)
        );
        let other = lane
            .register_submission(Submission {
                track_id: 4,
                ..identity
            })
            .unwrap();
        lane.close().unwrap();
        assert_eq!(
            receipt.wait(Duration::ZERO).unwrap(),
            Some(PresentationOutcome::Presented)
        );
        assert_eq!(
            other.wait(Duration::ZERO).unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
    }
    #[test]
    fn overlay_lane_requires_its_profile_and_close_retires_the_transport() {
        let session = Session::connect(ProducerConfig::offline()).unwrap();
        assert_eq!(
            session.open_overlay_input_lane(1).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        let mut config = ProducerConfig::offline();
        config.required_profiles.extend([
            TERMINAL_OVERLAY.into(),
            VECTOR_SCENE.into(),
            OVERLAY_INPUT.into(),
        ]);
        config.required_profiles.sort();
        config.required_profiles.dedup();
        let session = Session::connect(config).unwrap();
        let lane = session.open_overlay_input_lane(u64::MAX).unwrap();
        lane.renew(1_000_000, Duration::from_millis(100)).unwrap();
        assert!(lane.renew(1_000_000, Duration::ZERO).is_err());
        lane.close().unwrap();
        assert!(lane.renew(1_000_000, Duration::from_millis(100)).is_err());
        assert!(matches!(
            lane.wait_event(Duration::ZERO).unwrap(),
            Some(OverlayLaneEvent::ConnectionLost { .. })
        ));
        assert!(lane.wait_event(Duration::ZERO).unwrap().is_none());
        lane.close().unwrap();
        assert!(lane.wait_event(Duration::ZERO).unwrap().is_none());
    }

    #[test]
    fn overlay_reader_validates_before_dispatch_and_revokes_on_byte_overflow() {
        use std::io::Cursor;
        use vivid_protocol::wire::RecordHeader;
        let event = vivid_protocol::overlay::wire::InputEvent {
            address: vivid_protocol::overlay::wire::WindowAddress {
                context_id: 1,
                surface_id: 2,
                generation: u64::MAX,
            },
            scene_revision: u64::MAX,
            event: vivid_protocol::overlay::Event::Text("x".repeat(4096)),
        };
        let body = Envelope::new(0, event.payload().unwrap()).encode().unwrap();
        for (overlay, count, kind, body, accepted) in [
            (true, 1, messages::OVERLAY_INPUT_EVENT, body.clone(), true),
            (false, 1, messages::OVERLAY_INPUT_EVENT, body.clone(), false),
            (true, 32, messages::OVERLAY_INPUT_EVENT, body, false),
            (
                true,
                1,
                messages::OVERLAY_INPUT_EVENT,
                messages::empty(0),
                false,
            ),
            (true, 1, messages::KEY_INPUT, messages::empty(0), false),
        ] {
            let mut records = vec![(messages::LANE_ACCEPTED, 0, vec![])];
            records.extend((0..count).map(|_| (kind, 2, body.clone())));
            records.push((messages::PONG, 0, messages::empty(7)));
            let mut bytes = Vec::new();
            for (index, (record_type, object_id, body)) in records.into_iter().enumerate() {
                bytes.extend_from_slice(
                    &RecordHeader {
                        body_length: body.len() as u32,
                        record_type,
                        flags: 0,
                        object_id,
                        sequence: index as u64 + 1,
                    }
                    .encode(),
                );
                bytes.extend_from_slice(&body);
            }
            let mut connection = Connection::from_streams(
                Box::new(Cursor::new(bytes)),
                Box::new(io::sink()),
                ConnectionKind::Lane,
            )
            .unwrap();
            connection.read_record().unwrap();
            let (reader, writer) = connection.split().unwrap();
            let (sender, receiver) = mpsc::channel();
            let pending = Arc::new(PendingInput {
                overlay_receipts: Mutex::new(HashMap::new()),
                requests: Mutex::new(HashMap::from([(7, sender)])),
                events: Mutex::new(VecDeque::new()),
                events_ready: Condvar::new(),
                closed: AtomicBool::new(false),
            });
            spawn_interactive_reader(reader, writer, pending, overlay).unwrap();
            assert_eq!(
                receiver
                    .recv_timeout(Duration::from_secs(1))
                    .unwrap()
                    .is_ok(),
                accepted
            );
        }
    }

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
                overlay_receipts: Mutex::new(HashMap::new()),
                requests: Mutex::new(HashMap::from([(7, send)])),
                events: Mutex::new(VecDeque::new()),
                events_ready: Condvar::new(),
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

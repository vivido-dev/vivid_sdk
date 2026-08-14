//! Producer-side file-drop binding and receiver connection APIs.

use std::io;

use vivid_protocol::cbor::Value;
use vivid_protocol::file_drop::{
    self, AcceptFileDrop, AdvanceFileTransfer, CancelFileDrop, FileDropAccepted, FileDropBinding,
    FileDropBindingState, FileDropDestination, FileDropGrant, FileDropStatus, FileFinish,
    FileResult, FileTransferAbort, FileTransferAccepted, FileTransferFlow, FileTransferOpen,
    MaximumFileData, QueryFileDrop,
};
use vivid_protocol::messages;
use vivid_protocol::revision::{
    FileDropEpoch, FileDropGrantGeneration, FileTransferGeneration, SurfaceGeneration,
};
use vivid_protocol::wire::{Connection, ConnectionKind, ConnectionReader, ConnectionWriter};
use vivid_protocol::{auth, registry::record as records};

use crate::*;

/// Producer-side lifecycle guard for one file-drop binding.
///
/// It advances the producer epoch on every enable/disable transition and rejects grants that do
/// not match the complete binding identity. It deliberately does not perform UI consent: consent
/// belongs to the presenter that owns the local source file.
#[derive(Debug, Default)]
pub struct FileDropBindingGuard {
    epoch: FileDropEpoch,
    requested: Option<FileDropBinding>,
    grant: Option<FileDropGrant>,
}

impl FileDropBindingGuard {
    pub fn new() -> Self {
        Self::default()
    }

    pub const fn epoch(&self) -> FileDropEpoch {
        self.epoch
    }

    pub const fn grant(&self) -> Option<FileDropGrant> {
        self.grant
    }

    #[allow(clippy::too_many_arguments)]
    pub fn enable(
        &mut self,
        context_id: u64,
        surface_id: u64,
        surface_generation: SurfaceGeneration,
        destination: FileDropDestination,
        maximum_file_bytes: u64,
        maximum_pending_offers: u64,
        maximum_active_transfers: u64,
        maximum_record_body: u32,
        acceptance_timeout_us: u64,
        idle_timeout_us: u64,
    ) -> io::Result<FileDropBinding> {
        self.epoch = self.epoch.advance()?;
        let binding = FileDropBinding {
            producer_epoch: self.epoch,
            context_id,
            surface_id,
            surface_generation,
            destination: Some(destination),
            maximum_file_bytes,
            maximum_pending_offers,
            maximum_active_transfers,
            maximum_record_body,
            acceptance_timeout_us,
            idle_timeout_us,
        };
        binding.validate(surface_id)?;
        self.requested = Some(binding.clone());
        self.grant = None;
        Ok(binding)
    }

    pub fn disable(&mut self) -> io::Result<FileDropBinding> {
        let current = self
            .requested
            .as_ref()
            .filter(|binding| !binding.disabled())
            .ok_or_else(|| invalid_data("file-drop binding is not enabled"))?;
        self.epoch = self.epoch.advance()?;
        let binding = FileDropBinding {
            producer_epoch: self.epoch,
            context_id: current.context_id,
            surface_id: current.surface_id,
            surface_generation: current.surface_generation,
            destination: None,
            maximum_file_bytes: 0,
            maximum_pending_offers: 0,
            maximum_active_transfers: 0,
            maximum_record_body: 0,
            acceptance_timeout_us: 0,
            idle_timeout_us: 0,
        };
        self.requested = Some(binding.clone());
        self.grant = None;
        Ok(binding)
    }

    pub fn handle_bound(&mut self, grant: FileDropGrant) -> io::Result<()> {
        let requested = self
            .requested
            .as_ref()
            .ok_or_else(|| invalid_data("FILE_DROP_BOUND arrived without a binding request"))?;
        if grant.producer_epoch != requested.producer_epoch
            || grant.context_id != requested.context_id
            || grant.surface_id != requested.surface_id
            || grant.surface_generation != requested.surface_generation
        {
            return Err(invalid_data(
                "FILE_DROP_BOUND returned a different complete binding identity",
            ));
        }
        if requested.disabled() {
            if grant.state != FileDropBindingState::Disabled
                || grant.grant_generation != FileDropGrantGeneration::ZERO
            {
                return Err(invalid_data(
                    "disabled file-drop binding returned a live grant",
                ));
            }
        } else if grant.state == FileDropBindingState::Enabled
            && (grant.grant_generation == FileDropGrantGeneration::ZERO
                || grant.destination != requested.destination
                || grant.maximum_file_bytes > requested.maximum_file_bytes
                || grant.maximum_pending_offers > requested.maximum_pending_offers
                || grant.maximum_active_transfers > requested.maximum_active_transfers
                || grant.maximum_record_body > requested.maximum_record_body
                || grant.acceptance_timeout_us > requested.acceptance_timeout_us
                || grant.idle_timeout_us > requested.idle_timeout_us)
        {
            return Err(invalid_data(
                "FILE_DROP_BOUND did not narrow requested limits",
            ));
        }
        self.grant = (grant.state == FileDropBindingState::Enabled).then_some(grant);
        Ok(())
    }

    pub fn release(&mut self) {
        self.requested = None;
        self.grant = None;
    }
}

/// Parameters needed to open one accepted presenter-to-producer transfer generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IncomingFileTransferRequest {
    pub context_id: u64,
    pub surface_id: u64,
    pub producer_epoch: FileDropEpoch,
    pub grant_generation: FileDropGrantGeneration,
    pub surface_generation: SurfaceGeneration,
    pub drop_id: u64,
    pub transfer_id: u64,
    pub transfer_generation: FileTransferGeneration,
    pub resume_offset: u64,
    pub declared_length: u64,
    pub maximum_record_body: u32,
    pub maximum_body_bytes: u64,
    pub maximum_records: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IncomingFileTransferEvent {
    Data { offset: u64, bytes: Vec<u8> },
    Finished(FileFinish),
    Aborted(FileTransferAbort),
}

/// One authenticated, independently flow-controlled incoming transfer generation.
pub struct IncomingFileTransfer {
    reader: Option<ConnectionReader>,
    writer: ConnectionWriter,
    request: IncomingFileTransferRequest,
    flow: FileTransferFlow,
}

impl std::fmt::Debug for IncomingFileTransfer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IncomingFileTransfer")
            .field("drop_id", &self.request.drop_id)
            .field("transfer_id", &self.request.transfer_id)
            .field("generation", &self.request.transfer_generation)
            .field("next_offset", &self.flow.next_offset())
            .finish_non_exhaustive()
    }
}

impl IncomingFileTransfer {
    pub const fn request(&self) -> IncomingFileTransferRequest {
        self.request
    }

    pub const fn next_offset(&self) -> u64 {
        self.flow.next_offset()
    }

    pub fn read_event(&mut self) -> io::Result<IncomingFileTransferEvent> {
        let reader = self.reader.as_mut().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "offline file-transfer connections have no incoming records",
            )
        })?;
        let record = reader.read_record()?;
        if record.object_id != self.request.transfer_id {
            return Err(invalid_data(
                "file-transfer record has the wrong transfer ID",
            ));
        }
        match record.record_type {
            records::FILE_DATA => {
                let data = file_drop::parse_file_data(&record.body)?;
                let payload_length = u32::try_from(data.data.len())
                    .map_err(|_| invalid_data("FILE_DATA payload exceeds u32"))?;
                self.flow.admit(data.offset, payload_length)?;
                if self.flow.next_offset() > self.request.declared_length {
                    return Err(invalid_data("FILE_DATA exceeds the declared file length"));
                }
                Ok(IncomingFileTransferEvent::Data {
                    offset: data.offset,
                    bytes: data.data.to_vec(),
                })
            }
            records::FILE_FINISH => {
                let finish = FileFinish::decode(&record.body)?;
                self.validate_generation(finish.transfer_id, finish.transfer_generation)?;
                if finish.final_length != self.request.declared_length
                    || finish.final_length != self.flow.next_offset()
                {
                    return Err(invalid_data(
                        "FILE_FINISH length does not match the offer and streamed bytes",
                    ));
                }
                Ok(IncomingFileTransferEvent::Finished(finish))
            }
            records::FILE_TRANSFER_ABORT => {
                let abort = FileTransferAbort::decode(&record.body)?;
                self.validate_generation(abort.transfer_id, abort.transfer_generation)?;
                if abort.final_offset != self.flow.next_offset() {
                    return Err(invalid_data("FILE_TRANSFER_ABORT has an impossible offset"));
                }
                Ok(IncomingFileTransferEvent::Aborted(abort))
            }
            messages::ERROR => Err(presenter_error(&record.body)?),
            _ => Err(invalid_data(
                "unexpected record on a file-transfer connection",
            )),
        }
    }

    pub fn grant(&mut self, maximum_body_bytes: u64, maximum_records: u64) -> io::Result<()> {
        let maximum = MaximumFileData {
            transfer_id: self.request.transfer_id,
            transfer_generation: self.request.transfer_generation,
            maximum_body_bytes,
            maximum_records,
        };
        let body = maximum.encode()?;
        self.writer
            .write_record(records::MAX_FILE_DATA, 0, self.request.transfer_id, &body)?;
        self.flow.raise_maxima(maximum_body_bytes, maximum_records);
        Ok(())
    }

    pub fn send_result(&self, result: &FileResult) -> io::Result<()> {
        self.validate_generation(result.transfer_id, result.transfer_generation)?;
        self.writer.write_record(
            records::FILE_RESULT,
            0,
            self.request.transfer_id,
            &result.encode()?,
        )?;
        Ok(())
    }

    pub fn abort(&self, reason: u64) -> io::Result<()> {
        let abort = FileTransferAbort {
            transfer_id: self.request.transfer_id,
            transfer_generation: self.request.transfer_generation,
            reason,
            final_offset: self.flow.next_offset(),
        };
        self.writer.write_record(
            records::FILE_TRANSFER_ABORT,
            0,
            self.request.transfer_id,
            &abort.encode()?,
        )?;
        Ok(())
    }

    fn validate_generation(
        &self,
        transfer_id: u64,
        generation: FileTransferGeneration,
    ) -> io::Result<()> {
        if transfer_id != self.request.transfer_id || generation != self.request.transfer_generation
        {
            Err(invalid_data(
                "file-transfer record has a stale complete identity",
            ))
        } else {
            Ok(())
        }
    }
}

impl Session {
    pub fn set_file_drop_binding(
        &self,
        binding: &FileDropBinding,
        metadata: &RequestMetadata,
    ) -> io::Result<FileDropGrant> {
        self.require_file_drop_profile()?;
        let reply = self.request(
            records::SET_FILE_DROP_BINDING,
            binding.surface_id,
            binding.payload()?,
            metadata,
            None,
            None,
        )?;
        if let Some(record) = reply {
            expect_record(&record, records::FILE_DROP_BOUND, binding.surface_id)?;
            return Ok(FileDropGrant::decode(
                binding.surface_id,
                &Value::Map(decoded_payload(&record)?),
            )?);
        }
        Ok(offline_grant(binding))
    }

    pub fn accept_file_drop(
        &self,
        acceptance: AcceptFileDrop,
        metadata: &RequestMetadata,
    ) -> io::Result<FileDropAccepted> {
        self.require_file_drop_profile()?;
        let reply = self.request(
            records::ACCEPT_FILE_DROP,
            acceptance.binding.drop_id,
            acceptance.payload()?,
            metadata,
            None,
            None,
        )?;
        if let Some(record) = reply {
            expect_record(
                &record,
                records::FILE_DROP_ACCEPTED,
                acceptance.binding.drop_id,
            )?;
            return Ok(FileDropAccepted::decode(
                acceptance.binding.drop_id,
                &Value::Map(decoded_payload(&record)?),
            )?);
        }
        Ok(FileDropAccepted {
            drop_id: acceptance.binding.drop_id,
            transfer_id: acceptance.transfer_id,
            transfer_generation: acceptance.transfer_generation,
            open_timeout_us: file_drop::DEFAULT_FILE_TRANSFER_IDLE_US,
        })
    }

    pub fn cancel_file_drop(
        &self,
        cancellation: CancelFileDrop,
        metadata: &RequestMetadata,
    ) -> io::Result<()> {
        self.require_file_drop_profile()?;
        let reply = self.request(
            records::CANCEL_FILE_DROP,
            cancellation.binding.drop_id,
            cancellation.payload()?,
            metadata,
            None,
            None,
        )?;
        if let Some(record) = reply {
            if record.record_type == messages::OK {
                expect_empty_ok(&record)?;
            } else {
                expect_record(
                    &record,
                    records::FILE_DROP_CANCELLED,
                    cancellation.binding.drop_id,
                )?;
                CancelFileDrop::decode(
                    "FILE_DROP_CANCELLED",
                    cancellation.binding.drop_id,
                    &Value::Map(decoded_payload(&record)?),
                )?;
            }
        }
        Ok(())
    }

    pub fn advance_file_transfer(
        &self,
        advance: AdvanceFileTransfer,
        metadata: &RequestMetadata,
    ) -> io::Result<file_drop::FileTransferAdvanced> {
        self.require_file_drop_profile()?;
        let reply = self.request(
            records::ADVANCE_FILE_TRANSFER,
            advance.transfer_id,
            advance.payload()?,
            metadata,
            None,
            None,
        )?;
        if let Some(record) = reply {
            expect_record(
                &record,
                records::FILE_TRANSFER_ADVANCED,
                advance.transfer_id,
            )?;
            return Ok(file_drop::FileTransferAdvanced::decode(
                advance.transfer_id,
                &Value::Map(decoded_payload(&record)?),
            )?);
        }
        Ok(file_drop::FileTransferAdvanced {
            transfer_id: advance.transfer_id,
            generation: advance.new_generation,
            committed_offset: advance.committed_offset,
            open_timeout_us: file_drop::DEFAULT_FILE_TRANSFER_IDLE_US,
        })
    }

    pub fn query_file_drop(
        &self,
        query: QueryFileDrop,
        metadata: &RequestMetadata,
    ) -> io::Result<FileDropStatus> {
        self.require_file_drop_profile()?;
        let reply = self
            .request(
                records::QUERY_FILE_DROP,
                query.drop_id,
                query.payload()?,
                metadata,
                None,
                None,
            )?
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::Unsupported,
                    "offline sessions have no file-drop status",
                )
            })?;
        expect_record(&reply, records::FILE_DROP_STATUS, query.drop_id)?;
        Ok(FileDropStatus::decode(
            query.drop_id,
            &Value::Map(decoded_payload(&reply)?),
        )?)
    }

    pub fn open_incoming_file_transfer(
        &self,
        request: IncomingFileTransferRequest,
    ) -> io::Result<IncomingFileTransfer> {
        self.require_file_drop_profile()?;
        if request.resume_offset > request.declared_length {
            return Err(invalid_input(
                "file-transfer resume offset exceeds file length",
            ));
        }
        let mut nonce = [0; file_drop::FILE_TRANSFER_NONCE_BYTES];
        random_bytes(&mut nonce)?;
        let tag = auth::file_transfer_tag(
            self.channel_key.expose(),
            self.info.session_id,
            request.context_id,
            request.surface_id,
            request.producer_epoch.get(),
            request.grant_generation.get(),
            request.surface_generation.get(),
            request.drop_id,
            request.transfer_id,
            request.transfer_generation.get(),
            request.resume_offset,
            request.maximum_record_body,
            request.maximum_body_bytes,
            request.maximum_records,
            &nonce,
        );
        let open = FileTransferOpen {
            session_id: self.info.session_id,
            context_id: request.context_id,
            surface_id: request.surface_id,
            producer_epoch: request.producer_epoch,
            grant_generation: request.grant_generation,
            surface_generation: request.surface_generation,
            drop_id: request.drop_id,
            transfer_id: request.transfer_id,
            transfer_generation: request.transfer_generation,
            resume_offset: request.resume_offset,
            maximum_record_body: request.maximum_record_body,
            maximum_body_bytes: request.maximum_body_bytes,
            maximum_records: request.maximum_records,
            client_nonce: nonce,
            authentication_tag: tag,
        };
        let mut connection = if let Some(directory) = &self.trace_dir {
            Connection::trace(
                &directory.join(format!(
                    "file-drop-{}-{}-{}.vivid",
                    request.drop_id,
                    request.transfer_id,
                    request.transfer_generation.get()
                )),
                ConnectionKind::FileTransfer,
            )?
        } else if matches!(&self.control, ControlPlane::Offline { .. }) {
            Connection::sink(ConnectionKind::FileTransfer)?
        } else if let Some(factory) = &self.connection_factory {
            factory.open(ConnectionKind::FileTransfer, Some(LaneClass::Bulk))?
        } else {
            Connection::open(
                self.endpoints
                    .bulk
                    .as_ref()
                    .ok_or_else(|| invalid_input("missing Vivid bulk endpoint"))?,
                ConnectionKind::FileTransfer,
            )?
        };
        connection.set_receive_body_limit(request.maximum_record_body)?;
        connection.write_record(
            records::FILE_TRANSFER_OPEN,
            0,
            request.transfer_id,
            &open.encode()?,
        )?;
        let offline = matches!(&self.control, ControlPlane::Offline { .. });
        let (reader, writer) = if offline {
            (None, connection.writer())
        } else {
            let reply = connection.read_record()?;
            if reply.record_type == messages::ERROR {
                return Err(presenter_error(&reply.body)?);
            }
            expect_record(&reply, records::FILE_TRANSFER_ACCEPTED, request.transfer_id)?;
            let accepted = FileTransferAccepted::decode(&reply.body)?;
            if accepted.transfer_id != request.transfer_id
                || accepted.transfer_generation != request.transfer_generation
                || accepted.resume_offset != request.resume_offset
            {
                return Err(invalid_data(
                    "FILE_TRANSFER_ACCEPTED returned a different complete identity",
                ));
            }
            let (reader, writer) = connection.split()?;
            (Some(reader), writer)
        };
        Ok(IncomingFileTransfer {
            reader,
            writer,
            request,
            flow: FileTransferFlow::new(
                request.transfer_generation,
                request.resume_offset,
                request.maximum_body_bytes,
                request.maximum_records,
            )?,
        })
    }

    fn require_file_drop_profile(&self) -> io::Result<()> {
        self.lifecycle.ensure_active()?;
        if self.supports(FILE_DROP) {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "file-drop-v1 was not negotiated",
            ))
        }
    }
}

fn offline_grant(binding: &FileDropBinding) -> FileDropGrant {
    FileDropGrant {
        producer_epoch: binding.producer_epoch,
        grant_generation: if binding.disabled() {
            FileDropGrantGeneration::ZERO
        } else {
            FileDropGrantGeneration::ONE
        },
        context_id: binding.context_id,
        surface_id: binding.surface_id,
        surface_generation: binding.surface_generation,
        state: if binding.disabled() {
            FileDropBindingState::Disabled
        } else {
            FileDropBindingState::Enabled
        },
        destination: binding.destination,
        maximum_file_bytes: binding.maximum_file_bytes,
        maximum_pending_offers: binding.maximum_pending_offers,
        maximum_active_transfers: binding.maximum_active_transfers,
        maximum_record_body: binding.maximum_record_body,
        acceptance_timeout_us: binding.acceptance_timeout_us,
        idle_timeout_us: binding.idle_timeout_us,
        reason: 0,
    }
}

fn expect_empty_ok(record: &vivid_protocol::wire::Record) -> io::Result<()> {
    let payload = decoded_payload(record)?;
    if !payload.is_empty() {
        return Err(invalid_data("malformed OK reply"));
    }
    Ok(())
}

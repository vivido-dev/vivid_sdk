//! File-drop bindings and incoming transfers.
//!
//! A drop is offered to a surface's effective binding, accepted by a policy or a person, and
//! carried on its own authenticated connection with independent flow control. File bytes travel
//! only there, never on the control or media lanes.

use mlua::prelude::*;
use vivid_protocol::file_drop::{
    AcceptFileDrop, AdvanceFileTransfer, CancelFileDrop, FileDropBinding, FileDropDestination,
    FileDropTuple, FileResult, FileResultCode, QueryFileDrop,
};
use vivid_protocol::revision::{
    FileDropEpoch, FileDropGrantGeneration, FileTransferGeneration, SurfaceGeneration,
};
use vivid_sdk::{
    IncomingFileTransfer, IncomingFileTransferEvent, IncomingFileTransferRequest, RequestMetadata,
};

use crate::convert::{arg, check_keys, get, need, timeout};
use crate::error::{IoResultExt, closed, invalid};
use crate::session::LuaSession;

const TUPLE_KEYS: &[&str] = &[
    "producer_epoch",
    "grant_generation",
    "context_id",
    "surface_id",
    "surface_generation",
    "drop_id",
];

/// The complete identity a drop carries. Accepting under anything less would let a recycled
/// identifier name somebody else's file.
pub fn drop_tuple(lua: &Lua, binding: &FileDropTuple) -> LuaResult<LuaTable> {
    let table = lua.create_table()?;
    table.set("producer_epoch", binding.producer_epoch.get())?;
    table.set("grant_generation", binding.grant_generation.get())?;
    table.set("context_id", binding.context_id)?;
    table.set("surface_id", binding.surface_id)?;
    table.set("surface_generation", binding.surface_generation.get())?;
    table.set("drop_id", binding.drop_id)?;
    Ok(table)
}

fn tuple_from(table: &LuaTable) -> LuaResult<FileDropTuple> {
    check_keys(table, TUPLE_KEYS, "drop")?;
    Ok(FileDropTuple {
        producer_epoch: FileDropEpoch::new(need(table, "producer_epoch")?),
        grant_generation: FileDropGrantGeneration::new(need(table, "grant_generation")?),
        context_id: need(table, "context_id")?,
        surface_id: need(table, "surface_id")?,
        surface_generation: SurfaceGeneration::new(need(table, "surface_generation")?),
        drop_id: need(table, "drop_id")?,
    })
}

fn binding_from(config: &LuaTable) -> LuaResult<FileDropBinding> {
    check_keys(
        config,
        &[
            "producer_epoch",
            "context_id",
            "surface_id",
            "surface_generation",
            "destination",
            "maximum_file_bytes",
            "maximum_pending_offers",
            "maximum_active_transfers",
            "maximum_record_body",
            "acceptance_timeout_us",
            "idle_timeout_us",
        ],
        "file-drop binding",
    )?;
    // An absent destination disables the binding rather than choosing a default: a host with
    // nowhere to put a file should say so, not accept one it will drop.
    let destination = get::<u64>(config, "destination")?
        .map(|value| {
            FileDropDestination::try_from(value).map_err(|error| invalid(error.to_string()))
        })
        .transpose()?;
    Ok(FileDropBinding {
        producer_epoch: FileDropEpoch::new(need(config, "producer_epoch")?),
        context_id: need(config, "context_id")?,
        surface_id: need(config, "surface_id")?,
        surface_generation: SurfaceGeneration::new(need(config, "surface_generation")?),
        destination,
        maximum_file_bytes: need(config, "maximum_file_bytes")?,
        maximum_pending_offers: get(config, "maximum_pending_offers")?.unwrap_or(8),
        maximum_active_transfers: get(config, "maximum_active_transfers")?.unwrap_or(4),
        maximum_record_body: need(config, "maximum_record_body")?,
        acceptance_timeout_us: get(config, "acceptance_timeout_us")?.unwrap_or(20_000_000),
        idle_timeout_us: get(config, "idle_timeout_us")?.unwrap_or(5_000_000),
    })
}

pub fn add_session_methods<M: LuaUserDataMethods<LuaSession>>(methods: &mut M) {
    methods.add_method("set_file_drop_binding", |lua, this, config: LuaValue| {
        let binding = binding_from(&arg::<LuaTable>(config, "file-drop binding")?)?;
        let grant = this
            .get()?
            .set_file_drop_binding(&binding, &RequestMetadata::default())
            .lua()?;
        let table = lua.create_table()?;
        table.set("producer_epoch", grant.producer_epoch.get())?;
        table.set("grant_generation", grant.grant_generation.get())?;
        table.set("context_id", grant.context_id)?;
        table.set("surface_id", grant.surface_id)?;
        table.set("surface_generation", grant.surface_generation.get())?;
        table.set("state", grant.state as u64)?;
        table.set(
            "destination",
            grant.destination.map(|destination| destination as u64),
        )?;
        table.set("maximum_file_bytes", grant.maximum_file_bytes)?;
        table.set("maximum_pending_offers", grant.maximum_pending_offers)?;
        table.set("maximum_active_transfers", grant.maximum_active_transfers)?;
        table.set("maximum_record_body", grant.maximum_record_body)?;
        table.set("acceptance_timeout_us", grant.acceptance_timeout_us)?;
        table.set("idle_timeout_us", grant.idle_timeout_us)?;
        table.set("reason", grant.reason)?;
        Ok(table)
    });
    methods.add_method("accept_file_drop", |lua, this, config: LuaValue| {
        let config = arg::<LuaTable>(config, "acceptance")?;
        check_keys(
            &config,
            &[
                "drop",
                "transfer_id",
                "transfer_generation",
                "maximum_record_body",
                "initial_maximum_body_bytes",
                "initial_maximum_records",
            ],
            "acceptance",
        )?;
        let acceptance = AcceptFileDrop {
            binding: tuple_from(&need::<LuaTable>(&config, "drop")?)?,
            transfer_id: need(&config, "transfer_id")?,
            transfer_generation: FileTransferGeneration::new(need(&config, "transfer_generation")?),
            maximum_record_body: need(&config, "maximum_record_body")?,
            initial_maximum_body_bytes: need(&config, "initial_maximum_body_bytes")?,
            initial_maximum_records: need(&config, "initial_maximum_records")?,
        };
        let accepted = this
            .get()?
            .accept_file_drop(acceptance, &RequestMetadata::default())
            .lua()?;
        let table = lua.create_table()?;
        table.set("drop_id", accepted.drop_id)?;
        table.set("transfer_id", accepted.transfer_id)?;
        table.set("transfer_generation", accepted.transfer_generation.get())?;
        table.set("open_timeout_us", accepted.open_timeout_us)?;
        Ok(table)
    });
    methods.add_method(
        "cancel_file_drop",
        |_, this, (drop, reason): (LuaValue, LuaValue)| {
            let cancellation = CancelFileDrop {
                binding: tuple_from(&arg::<LuaTable>(drop, "drop")?)?,
                reason: arg(reason, "reason")?,
            };
            this.get()?
                .cancel_file_drop(cancellation, &RequestMetadata::default())
                .lua()
        },
    );
    methods.add_method("advance_file_transfer", |lua, this, config: LuaValue| {
        let config = arg::<LuaTable>(config, "transfer advance")?;
        check_keys(
            &config,
            &[
                "context_id",
                "surface_id",
                "drop_id",
                "transfer_id",
                "expected_generation",
                "new_generation",
                "committed_offset",
                "maximum_body_bytes",
                "maximum_records",
            ],
            "transfer advance",
        )?;
        let advance = AdvanceFileTransfer {
            context_id: need(&config, "context_id")?,
            surface_id: need(&config, "surface_id")?,
            drop_id: need(&config, "drop_id")?,
            transfer_id: need(&config, "transfer_id")?,
            expected_generation: FileTransferGeneration::new(need(&config, "expected_generation")?),
            new_generation: FileTransferGeneration::new(need(&config, "new_generation")?),
            committed_offset: need(&config, "committed_offset")?,
            maximum_body_bytes: need(&config, "maximum_body_bytes")?,
            maximum_records: need(&config, "maximum_records")?,
        };
        let advanced = this
            .get()?
            .advance_file_transfer(advance, &RequestMetadata::default())
            .lua()?;
        let table = lua.create_table()?;
        table.set("transfer_id", advanced.transfer_id)?;
        table.set("generation", advanced.generation.get())?;
        table.set("committed_offset", advanced.committed_offset)?;
        table.set("open_timeout_us", advanced.open_timeout_us)?;
        Ok(table)
    });
    methods.add_method("query_file_drop", |lua, this, drop_id: LuaValue| {
        let drop_id = arg(drop_id, "drop_id")?;
        let status = this
            .get()?
            .query_file_drop(QueryFileDrop { drop_id }, &RequestMetadata::default())
            .lua()?;
        let table = lua.create_table()?;
        table.set("drop_id", status.drop_id)?;
        table.set("state", status.state as u64)?;
        table.set("transfer_id", status.transfer_id)?;
        table.set("generation", status.generation.get())?;
        table.set("committed_offset", status.committed_offset)?;
        table.set("result", status.result.map(|code| code as u64))?;
        table.set("final_name", status.final_name)?;
        Ok(table)
    });
    methods.add_method(
        "open_incoming_file_transfer",
        |_, this, config: LuaValue| {
            let config = arg::<LuaTable>(config, "transfer request")?;
            check_keys(
                &config,
                &[
                    "context_id",
                    "surface_id",
                    "producer_epoch",
                    "grant_generation",
                    "surface_generation",
                    "drop_id",
                    "transfer_id",
                    "transfer_generation",
                    "resume_offset",
                    "declared_length",
                    "maximum_record_body",
                    "maximum_body_bytes",
                    "maximum_records",
                ],
                "transfer request",
            )?;
            let request = IncomingFileTransferRequest {
                context_id: need(&config, "context_id")?,
                surface_id: need(&config, "surface_id")?,
                producer_epoch: FileDropEpoch::new(need(&config, "producer_epoch")?),
                grant_generation: FileDropGrantGeneration::new(need(&config, "grant_generation")?),
                surface_generation: SurfaceGeneration::new(need(&config, "surface_generation")?),
                drop_id: need(&config, "drop_id")?,
                transfer_id: need(&config, "transfer_id")?,
                transfer_generation: FileTransferGeneration::new(need(
                    &config,
                    "transfer_generation",
                )?),
                resume_offset: get(&config, "resume_offset")?.unwrap_or(0),
                declared_length: need(&config, "declared_length")?,
                maximum_record_body: need(&config, "maximum_record_body")?,
                maximum_body_bytes: need(&config, "maximum_body_bytes")?,
                maximum_records: need(&config, "maximum_records")?,
            };
            Ok(LuaIncomingFileTransfer {
                inner: Some(this.get()?.open_incoming_file_transfer(request).lua()?),
            })
        },
    );
}

pub struct LuaIncomingFileTransfer {
    inner: Option<IncomingFileTransfer>,
}

impl LuaIncomingFileTransfer {
    fn get(&self) -> LuaResult<&IncomingFileTransfer> {
        self.inner.as_ref().ok_or_else(|| closed("file transfer"))
    }

    fn get_mut(&mut self) -> LuaResult<&mut IncomingFileTransfer> {
        self.inner.as_mut().ok_or_else(|| closed("file transfer"))
    }
}

impl LuaUserData for LuaIncomingFileTransfer {
    fn add_fields<F: LuaUserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("closed", |_, this| Ok(this.inner.is_none()));
    }

    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(LuaMetaMethod::ToString, |_, this, ()| {
            Ok(match &this.inner {
                Some(transfer) => format!(
                    "vivid_sdk.IncomingFileTransfer(drop_id={}, transfer_id={})",
                    transfer.request().drop_id,
                    transfer.request().transfer_id
                ),
                None => "vivid_sdk.IncomingFileTransfer(closed)".into(),
            })
        });
        methods.add_method_mut("close", |_, this, ()| {
            this.inner
                .take()
                .map(drop)
                .ok_or_else(|| closed("file transfer"))
        });
        methods.add_method_mut("read_event", |lua, this, ()| {
            let event = this.get_mut()?.read_event().lua()?;
            let table = lua.create_table()?;
            match event {
                IncomingFileTransferEvent::Data { offset, bytes } => {
                    table.set("kind", "data")?;
                    table.set("offset", offset)?;
                    table.set("bytes", lua.create_string(&bytes)?)?;
                }
                IncomingFileTransferEvent::Finished(finish) => {
                    table.set("kind", "finished")?;
                    table.set("final_length", finish.final_length)?;
                }
                IncomingFileTransferEvent::Aborted(abort) => {
                    table.set("kind", "aborted")?;
                    table.set("reason", abort.reason)?;
                    table.set("final_offset", abort.final_offset)?;
                }
            }
            Ok(table)
        });
        methods.add_method_mut("set_read_deadline", |_, this, value: LuaValue| {
            // `nil` restores unbounded reads; a number bounds every later read, in seconds.
            let deadline = match value {
                LuaValue::Nil => None,
                value => Some(timeout(value, std::time::Duration::ZERO, "timeout")?),
            };
            this.get_mut()?.set_read_deadline(deadline).lua()
        });
        methods.add_method_mut(
            "grant",
            |_, this, (bytes, records): (LuaValue, LuaValue)| {
                let bytes = arg(bytes, "maximum_body_bytes")?;
                let records = arg(records, "maximum_records")?;
                this.get_mut()?.grant(bytes, records).lua()
            },
        );
        methods.add_method("send_result", |_, this, config: LuaValue| {
            let config = arg::<LuaTable>(config, "transfer result")?;
            check_keys(
                &config,
                &[
                    "transfer_id",
                    "transfer_generation",
                    "result",
                    "committed_length",
                    "final_name",
                    "committed_path",
                ],
                "transfer result",
            )?;
            let result = FileResult {
                transfer_id: need(&config, "transfer_id")?,
                transfer_generation: FileTransferGeneration::new(need(
                    &config,
                    "transfer_generation",
                )?),
                result: FileResultCode::try_from(need::<u64>(&config, "result")?)
                    .map_err(|error| invalid(error.to_string()))?,
                committed_length: get(&config, "committed_length")?.unwrap_or(0),
                final_name: get(&config, "final_name")?.unwrap_or_default(),
                committed_path: get(&config, "committed_path")?,
            };
            this.get()?.send_result(&result).lua()
        });
        methods.add_method("abort", |_, this, reason: LuaValue| {
            this.get()?.abort(arg(reason, "reason")?).lua()
        });
    }
}

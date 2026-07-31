//! Dry-run support: endpoints, contracts, and a synthetic target descriptor.
//!
//! A dry-run session exercises the full object model and every validation path without a
//! presenter, which is what makes producer logic testable in isolation.

use std::path::PathBuf;
use std::{env, io};

use vivid_protocol::cbor::Value;
use vivid_protocol::messages::PayloadMap;
use vivid_protocol::resource::{Resource, ResourceContract};
use vivid_protocol::wire::Endpoint;

use crate::*;

pub(crate) fn endpoint(explicit: Option<&str>, variable: &str) -> io::Result<Endpoint> {
    let value = explicit
        .map(ToOwned::to_owned)
        .or_else(|| env::var(variable).ok())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("required Vivid discovery variable {variable} is absent"),
            )
        })?;
    Endpoint::parse(&value)
}

pub(crate) fn optional_endpoint(
    explicit: Option<&str>,
    variable: &str,
) -> io::Result<Option<Endpoint>> {
    explicit
        .map(ToOwned::to_owned)
        .or_else(|| env::var(variable).ok())
        .map(|value| Endpoint::parse(&value))
        .transpose()
}

pub(crate) fn offline_endpoint() -> io::Result<Endpoint> {
    #[cfg(unix)]
    {
        Ok(Endpoint::Unix(PathBuf::from("/dev/null")))
    }
    #[cfg(not(unix))]
    {
        Endpoint::parse("tcp:127.0.0.1:1")
    }
}

pub(crate) fn offline_contract() -> ResourceContract {
    let mut contract = ResourceContract::new([1_000_000; 33]);
    contract.set(
        Resource::MediaRecordBody,
        u64::from(vivid_protocol::HARD_MAX_RECORD_BODY),
    );
    contract.set(
        Resource::ControlRecordBody,
        u64::from(vivid_protocol::CONTROL_MAX_RECORD_BODY),
    );
    contract
}

pub(crate) fn offline_target_descriptor() -> PayloadMap {
    vec![
        (0, Value::Unsigned(1920)),
        (1, Value::Unsigned(1080)),
        (2, Value::Unsigned(80)),
        (3, Value::Unsigned(24)),
        (4, Value::Unsigned(24)),
        (5, Value::Unsigned(45)),
        (6, Value::Bool(true)),
        (7, Value::Unsigned(3)),
        (8, Value::Unsigned(256)),
    ]
}

pub(crate) fn validate_terminal_target_descriptor(descriptor: &PayloadMap) -> io::Result<()> {
    validate_exact_payload_keys("terminal target descriptor", descriptor, 0..=8)?;
    for key in 0..=5 {
        if required_u64(descriptor, key)? == 0 {
            return Err(invalid_data(
                "terminal target descriptor contains a zero dimension",
            ));
        }
    }
    let _settled = required_bool(descriptor, 6)?;
    if required_u64(descriptor, 7)? != 3 || required_u64(descriptor, 8)? == 0 {
        return Err(invalid_data(
            "terminal target descriptor has unsupported anchor capabilities",
        ));
    }
    Ok(())
}

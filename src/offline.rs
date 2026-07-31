//! Dry-run support: endpoints, contracts, and a synthetic target descriptor.
//!
//! A dry-run session exercises the full object model and every validation path without a
//! presenter, which is what makes producer logic testable in isolation.

use std::{env, io};

#[cfg(unix)]
use std::path::PathBuf;

use vivid_protocol::resource::{Resource, ResourceContract};
use vivid_protocol::wire::Endpoint;

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

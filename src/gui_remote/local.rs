//! Bounded native framing shared by private GUI process services.

// Rust guideline compliant 2026-10-07
use crate::local_control::LocalStream;
use serde::{Serialize, de::DeserializeOwned};
use std::{
    io::{self, Read, Write},
    thread,
    time::{Duration, Instant},
};
const POLL: Duration = Duration::from_millis(25);
pub(super) fn validate_endpoint(endpoint: &std::path::Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if !endpoint.is_absolute() {
            return Err(invalid());
        }
        let metadata = std::fs::symlink_metadata(endpoint.parent().ok_or_else(invalid)?)?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err(invalid());
        }
        if endpoint.try_exists()? {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
    }
    #[cfg(not(unix))]
    let _ = endpoint;
    Ok(())
}
fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid local GUI service frame",
    )
}
pub(super) fn remaining(end: Instant) -> io::Result<Duration> {
    let time = end.saturating_duration_since(Instant::now());
    if time.is_zero() {
        Err(io::ErrorKind::TimedOut.into())
    } else {
        Ok(time)
    }
}
fn read_exact(stream: &mut LocalStream, mut bytes: &mut [u8], end: Instant) -> io::Result<()> {
    while !bytes.is_empty() {
        remaining(end)?;
        #[cfg(not(unix))]
        stream.set_read_timeout(Some(remaining(end)?))?;
        match stream.read(bytes) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => bytes = &mut bytes[n..],
            Err(e) if e.kind() == io::ErrorKind::Interrupted => (),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                thread::sleep(POLL.min(remaining(end)?))
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
pub(super) fn send<T: Serialize>(
    stream: &mut LocalStream,
    value: &T,
    max: usize,
    end: Instant,
) -> io::Result<()> {
    stream.set_nonblocking(true)?;
    let body = serde_json::to_vec(value).map_err(|_| invalid())?;
    if body.len() > max {
        return Err(invalid());
    }
    let header = u32::try_from(body.len())
        .map_err(|_| invalid())?
        .to_be_bytes();
    for mut bytes in [&header[..], &body[..]] {
        while !bytes.is_empty() {
            remaining(end)?;
            #[cfg(not(unix))]
            stream.set_write_timeout(Some(remaining(end)?))?;
            match stream.write(bytes) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(n) => bytes = &bytes[n..],
                Err(e) if e.kind() == io::ErrorKind::Interrupted => (),
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    thread::sleep(POLL.min(remaining(end)?))
                }
                Err(e) => return Err(e),
            }
        }
    }
    Ok(())
}
pub(super) fn receive<T: DeserializeOwned>(
    stream: &mut LocalStream,
    max: usize,
    end: Instant,
) -> io::Result<T> {
    // Unix reads are nonblocking under one absolute deadline. Reinstalling a
    // socket timeout after peer FIN can fail with EINVAL on macOS even when a
    // complete final reply remains buffered. Windows uses cancellable pipe I/O.
    stream.set_nonblocking(true)?;
    let mut header = [0; 4];
    read_exact(stream, &mut header, end)?;
    read_body(stream, header, max, end)
}
fn read_body<T: DeserializeOwned>(
    stream: &mut LocalStream,
    header: [u8; 4],
    max: usize,
    end: Instant,
) -> io::Result<T> {
    let len = usize::try_from(u32::from_be_bytes(header)).map_err(|_| invalid())?;
    if len == 0 || len > max {
        return Err(invalid());
    }
    let mut body = vec![0; len];
    read_exact(stream, &mut body, end)?;
    serde_json::from_slice(&body).map_err(|_| invalid())
}
pub(super) fn receive_idle<T: DeserializeOwned>(
    stream: &mut LocalStream,
    max: usize,
    budget: Duration,
) -> io::Result<Option<T>> {
    stream.set_nonblocking(true)?;
    let mut header = [0; 4];
    #[cfg(not(unix))]
    stream.set_read_timeout(Some(POLL))?;
    match stream.read(&mut header[..1]) {
        Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
        Ok(_) => (),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) =>
        {
            #[cfg(unix)]
            thread::sleep(POLL);
            return Ok(None);
        }
        Err(e) => return Err(e),
    }
    let end = Instant::now() + budget;
    read_exact(stream, &mut header[1..], end)?;
    read_body(stream, header, max, end).map(Some)
}

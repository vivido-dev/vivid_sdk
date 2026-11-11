//! Bounded external text selection reads dispatched outside the GUI thread.

// Rust guideline compliant 2026-10-07
use gpui::ClipboardReadError;
use std::time::Instant;

pub(crate) type ClipboardTextRead =
    Box<dyn FnOnce(Instant) -> Result<Option<String>, ClipboardReadError> + Send>;

#[cfg(feature = "wayland")]
pub(crate) fn read_pipe(
    mut fd: filedescriptor::FileDescriptor,
    deadline: Instant,
    max_bytes: usize,
) -> Result<Option<String>, ClipboardReadError> {
    use std::{io::Read as _, os::fd::AsRawFd as _};
    fd.set_non_blocking(true)
        .map_err(|_| ClipboardReadError::Unavailable)?;
    let mut bytes = Vec::new();
    let mut chunk = [0; 8192];
    loop {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return Err(ClipboardReadError::Unavailable);
        };
        let mut poll = [filedescriptor::pollfd {
            fd: fd.as_raw_fd(),
            events: filedescriptor::POLLIN,
            revents: 0,
        }];
        match filedescriptor::poll(&mut poll, Some(remaining)) {
            Ok(0) => return Err(ClipboardReadError::Unavailable),
            Ok(_) => (),
            Err(filedescriptor::Error::Poll(error))
                if error.kind() == std::io::ErrorKind::Interrupted =>
            {
                continue;
            }
            Err(_) => return Err(ClipboardReadError::Unavailable),
        }
        match fd.read(&mut chunk) {
            Ok(0) => {
                return String::from_utf8(bytes)
                    .map(Some)
                    .map_err(|_| ClipboardReadError::UnsupportedContent);
            }
            Ok(count) => {
                if bytes
                    .len()
                    .checked_add(count)
                    .is_none_or(|size| size > max_bytes)
                {
                    return Err(ClipboardReadError::UnsupportedContent);
                }
                bytes.extend_from_slice(&chunk[..count]);
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) =>
            {
                ()
            }
            Err(_) => return Err(ClipboardReadError::Unavailable),
        }
    }
}

#[cfg(all(test, feature = "wayland"))]
mod tests {
    use super::*;
    use std::{io::Write as _, time::Duration};
    #[test]
    fn pipe_text_is_exact_bounded_and_has_an_absolute_deadline() {
        for (bytes, limit, expected) in [
            ("λ\r\n👋".as_bytes(), 32, Some("λ\r\n👋")),
            (&b"long"[..], 3, None),
            (&b"\xff"[..], 10, None),
        ] {
            let mut pipe = filedescriptor::Pipe::new().unwrap();
            pipe.write.write_all(bytes).unwrap();
            drop(pipe.write);
            let result = read_pipe(pipe.read, Instant::now() + Duration::from_secs(1), limit);
            if let Some(expected) = expected {
                assert_eq!(result.unwrap().as_deref(), Some(expected));
            } else {
                assert_eq!(result.unwrap_err(), ClipboardReadError::UnsupportedContent);
            }
        }
        let mut pipe = filedescriptor::Pipe::new().unwrap();
        pipe.write.write_all(b"partial").unwrap();
        let _held = pipe.write;
        let start = Instant::now();
        assert!(read_pipe(pipe.read, start + Duration::from_millis(25), 32).is_err());
        assert!(start.elapsed() < Duration::from_secs(1));
    }
}

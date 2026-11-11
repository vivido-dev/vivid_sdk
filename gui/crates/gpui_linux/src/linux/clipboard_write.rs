//! Bounded asynchronous clipboard output without GUI-thread descriptor writes.

// Rust guideline compliant 2026-10-07
use smol::io::AsyncWriteExt as _;
use std::{
    cell::RefCell,
    fs::File,
    io,
    os::{fd::OwnedFd, unix::fs::FileTypeExt as _},
    time::{Duration, Instant},
};

// Match the hosted clipboard's transfer count, byte ceiling and deadline.
const MAX_WRITES: usize = 4;
const MAX_BYTES: usize = 1024 * 1024;
const WRITE_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Default)]
pub(crate) struct ClipboardWrites {
    tasks: RefCell<Vec<smol::Task<()>>>,
}
impl ClipboardWrites {
    pub(crate) fn send(&self, fd: OwnedFd, bytes: Vec<u8>) -> bool {
        if bytes.len() > MAX_BYTES {
            return false;
        }
        let mut tasks = self.tasks.borrow_mut();
        tasks.retain(|task| !task.is_finished());
        if tasks.len() >= MAX_WRITES {
            return false;
        }
        let deadline = Instant::now() + WRITE_TIMEOUT;
        // Retain task handles: dropping the clipboard cancels every owned write.
        tasks.push(smol::spawn(async move {
            let _ = write(fd, &bytes, deadline).await;
        }));
        true
    }
}

async fn write(fd: OwnedFd, bytes: &[u8], deadline: Instant) -> io::Result<()> {
    if Instant::now() >= deadline {
        return Err(io::ErrorKind::TimedOut.into());
    }
    let file = File::from(fd);
    let kind = file.metadata()?.file_type();
    if !kind.is_fifo() && !kind.is_socket() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "clipboard output must be a pipe or socket",
        ));
    }
    let mut output = smol::Async::new(file)?;
    smol::future::or(async { output.write_all(bytes).await }, async {
        smol::Timer::at(deadline).await;
        Err(io::ErrorKind::TimedOut.into())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Read as _, os::unix::net::UnixStream};

    #[test]
    fn output_preserves_bytes_rejects_regular_files_and_obeys_deadlines() {
        let queue = ClipboardWrites::default();
        let (mut reader, writer) = UnixStream::pair().unwrap();
        reader
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let bytes = b"file:///tmp/a%20b\r\n";
        assert!(queue.send(writer.into(), bytes.to_vec()));
        let mut actual = Vec::new();
        reader.read_to_end(&mut actual).unwrap();
        assert_eq!(actual, bytes);

        let file = File::open(std::env::current_exe().unwrap()).unwrap();
        assert_eq!(
            smol::block_on(write(
                file.into(),
                b"never write",
                Instant::now() + WRITE_TIMEOUT
            ))
            .unwrap_err()
            .kind(),
            io::ErrorKind::InvalidInput
        );
        let (_held_reader, writer) = UnixStream::pair().unwrap();
        let start = Instant::now();
        assert_eq!(
            smol::block_on(write(
                writer.into(),
                &vec![0; MAX_BYTES],
                start + Duration::from_millis(25)
            ))
            .unwrap_err()
            .kind(),
            io::ErrorKind::TimedOut
        );
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn saturated_output_is_bounded_and_does_not_consume_a_peer_queues_capacity() {
        let queue = ClipboardWrites::default();
        let peer = ClipboardWrites::default();
        let mut held = Vec::new();
        for _ in 0..MAX_WRITES {
            let (reader, writer) = UnixStream::pair().unwrap();
            held.push(reader);
            assert!(queue.send(writer.into(), vec![0; MAX_BYTES]));
        }
        let (_reader, writer) = UnixStream::pair().unwrap();
        assert!(!queue.send(writer.into(), Vec::new()));
        let (mut reader, writer) = UnixStream::pair().unwrap();
        reader
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        assert!(peer.send(writer.into(), b"peer".to_vec()));
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"peer");
        let deadline = Instant::now() + Duration::from_secs(2);
        while queue.tasks.borrow().iter().any(|task| !task.is_finished()) {
            assert!(
                Instant::now() < deadline,
                "stalled writes retained capacity"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        let (mut reader, writer) = UnixStream::pair().unwrap();
        reader
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        assert!(queue.send(writer.into(), b"fresh".to_vec()));
        bytes.clear();
        reader.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"fresh");
        drop(held);
    }
}

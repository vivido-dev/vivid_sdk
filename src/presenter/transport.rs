//! Accepted-side Vivid 1.5 framing for the per-session private presenter endpoint.

use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use vivid_protocol::wire::{
    ConnectionKind, HEADER_SIZE, PREFACE_SIZE, Preface, PrefaceClassification, RECORD_KNOWN_FLAGS,
    Record, RecordHeader,
};
use vivid_protocol::{CONTROL_MAX_RECORD_BODY, HARD_MAX_RECORD_BODY};

use super::listener::{ConnectionCancel, Transport};

pub struct Reader {
    reader: Box<dyn Read + Send>,
    writer: Arc<Writer>,
    timeout: Arc<dyn Fn(Option<std::time::Duration>) -> io::Result<()> + Send + Sync>,
    deadline: Arc<Mutex<Option<std::time::Instant>>>,
    _cancel: ConnectionCancel,
    negotiated_maximum: u32,
    maximum: u32,
    sequence: u64,
    first_record: bool,
}

impl Reader {
    pub fn new(mut stream: Transport) -> io::Result<(Self, Preface, [u8; PREFACE_SIZE])> {
        let cancel = stream.cancel();
        let mut bytes = [0_u8; PREFACE_SIZE];
        stream.reader.read_exact(&mut bytes)?;
        let preface = match Preface::classify(bytes)? {
            PrefaceClassification::Accepted(preface) => preface,
            PrefaceClassification::UnsupportedVersion(_) => {
                stream
                    .writer
                    .write_all(&vivid_protocol::wire::unsupported_version_record())?;
                stream.writer.flush()?;
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "unsupported Vivid protocol version",
                ));
            }
        };
        let maximum = preface.initiator_tx_body_limit.min(HARD_MAX_RECORD_BODY);
        let writer = Arc::new(Writer::new(
            stream.writer,
            cancel.clone(),
            if preface.kind == ConnectionKind::Control {
                CONTROL_MAX_RECORD_BODY
            } else {
                HARD_MAX_RECORD_BODY
            },
        )?);

        Ok((
            Self {
                reader: stream.reader,
                writer,
                timeout: stream.timeout,
                deadline: stream.deadline,
                _cancel: cancel,
                negotiated_maximum: maximum,
                maximum: if preface.kind == ConnectionKind::Control {
                    maximum.min(CONTROL_MAX_RECORD_BODY)
                } else {
                    maximum
                },
                sequence: 0,
                first_record: true,
            },
            preface,
            bytes,
        ))
    }

    pub fn read_record(&mut self, kind: ConnectionKind) -> io::Result<Record> {
        let mut body = Vec::new();
        let header = self.read_record_body_into(kind, &mut body)?;
        Ok(Record {
            record_type: header.record_type,
            flags: header.flags,
            object_id: header.object_id,
            sequence: header.sequence,
            body,
        })
    }

    fn read_record_body_into(
        &mut self,
        kind: ConnectionKind,
        body: &mut Vec<u8>,
    ) -> io::Result<RecordHeader> {
        let mut bytes = [0_u8; HEADER_SIZE];
        self.reader
            .read_exact(&mut bytes)
            .map_err(|error| contextual(error, "record header"))?;
        let header = RecordHeader::decode(bytes);
        if header.flags & !RECORD_KNOWN_FLAGS != 0 {
            return Err(invalid("Vivid record has nonzero reserved flags"));
        }
        if header.body_length > self.maximum || header.body_length > HARD_MAX_RECORD_BODY {
            return Err(invalid("Vivid record exceeds the accepted body limit"));
        }
        let expected = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| invalid("record sequence exhausted"))?;
        if header.sequence != expected {
            return Err(invalid("Vivid record sequence is not contiguous"));
        }
        if self.first_record {
            kind.validate_first_record(&header)?;
            self.first_record = false;
        }
        self.sequence = header.sequence;
        body.resize(header.body_length as usize, 0);
        self.reader
            .read_exact(body)
            .map_err(|error| contextual(error, "record body"))?;
        Ok(header)
    }

    pub fn writer(&self) -> Arc<Writer> {
        self.writer.clone()
    }

    pub fn cancel(&self) -> ConnectionCancel {
        self._cancel.clone()
    }

    pub fn set_maximum(&mut self, maximum: u32) -> io::Result<()> {
        if maximum == 0 || maximum > HARD_MAX_RECORD_BODY {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid incoming record limit",
            ));
        }
        self.maximum = self.negotiated_maximum.min(maximum);
        Ok(())
    }

    pub fn clear_read_deadline(&self) -> io::Result<()> {
        *self
            .deadline
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        (self.timeout)(None)
    }
}

/// Bounded, ordered outbound admission. Socket I/O runs on one independent worker.
/// A successful write means queued; use `flush` outside shared state locks when delivery is needed.
pub struct Writer {
    inner: Mutex<WriterInner>,
    closed: Arc<AtomicBool>,
    cancel: ConnectionCancel,
    bytes: Arc<AtomicUsize>,
}
const MAX_QUEUED_RECORDS: usize = 64;
const MAX_QUEUED_BYTES: usize = HARD_MAX_RECORD_BODY as usize + HEADER_SIZE;

struct WriterInner {
    sender: mpsc::SyncSender<Outbound>,
    maximum: u32,
    sequence: u64,
}
enum Outbound {
    Record(zeroize::Zeroizing<Vec<u8>>),
    Barrier(mpsc::SyncSender<()>),
}

impl Writer {
    fn new(
        mut stream: Box<dyn Write + Send>,
        cancel: ConnectionCancel,
        maximum: u32,
    ) -> io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(MAX_QUEUED_RECORDS);
        let closed = Arc::new(AtomicBool::new(false));
        let bytes = Arc::new(AtomicUsize::new(0));
        let worker_closed = closed.clone();
        let worker_bytes = bytes.clone();
        let worker_cancel = cancel.clone();
        std::thread::Builder::new()
            .name("vivid-presenter-writer".into())
            .spawn(move || {
                while !worker_closed.load(Ordering::Acquire) {
                    let item = match receiver.recv_timeout(Duration::from_millis(100)) {
                        Ok(item) => item,
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                    match item {
                        Outbound::Record(body) => {
                            let result =
                                write_complete(stream.as_mut(), &body).and_then(|_| stream.flush());
                            worker_bytes.fetch_sub(body.len(), Ordering::AcqRel);
                            if result.is_err() {
                                break;
                            }
                        }
                        Outbound::Barrier(done) => {
                            let _ = done.send(());
                        }
                    }
                }
                worker_closed.store(true, Ordering::Release);
                worker_cancel.cancel();
            })?;
        Ok(Self {
            inner: Mutex::new(WriterInner {
                sender,
                maximum,
                sequence: 0,
            }),
            closed,
            cancel,
            bytes,
        })
    }

    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.cancel.cancel();
    }

    /// Wait for queued records outside any presenter state lock. Silent peers are bounded.
    pub fn flush(&self) -> io::Result<()> {
        let (send, receive) = mpsc::sync_channel(1);
        {
            let inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            if self.closed.load(Ordering::Acquire)
                || inner.sender.try_send(Outbound::Barrier(send)).is_err()
            {
                self.close();
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "presenter writer closed or saturated",
                ));
            }
        }
        receive.recv_timeout(Duration::from_secs(3)).map_err(|_| {
            self.close();
            io::Error::new(io::ErrorKind::TimedOut, "presenter write deadline expired")
        })
    }

    pub fn set_maximum(&self, maximum: u32) -> io::Result<()> {
        if maximum == 0 || maximum > HARD_MAX_RECORD_BODY {
            return Err(invalid_input("invalid outgoing record limit"));
        }
        self.inner.lock().unwrap_or_else(|p| p.into_inner()).maximum = maximum;
        Ok(())
    }

    pub fn write_record(&self, record_type: u16, object_id: u64, body: &[u8]) -> io::Result<u64> {
        self.write_record_parts(record_type, object_id, &[body])
    }

    pub fn write_record_parts(
        &self,
        record_type: u16,
        object_id: u64,
        parts: &[&[u8]],
    ) -> io::Result<u64> {
        let body_length = parts.iter().try_fold(0usize, |total, part| {
            total
                .checked_add(part.len())
                .ok_or_else(|| invalid_input("record body length overflows"))
        })?;
        let body_length =
            u32::try_from(body_length).map_err(|_| invalid_input("record body exceeds u32"))?;
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if self.closed.load(Ordering::Acquire) {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        if body_length > inner.maximum || body_length > HARD_MAX_RECORD_BODY {
            return Err(invalid_input(
                "outgoing Vivid record exceeds the accepted body limit",
            ));
        }
        let sequence = inner
            .sequence
            .checked_add(1)
            .ok_or_else(|| invalid("outgoing sequence exhausted"))?;
        let size = HEADER_SIZE + body_length as usize;
        let byte_budget = (inner.maximum as usize + HEADER_SIZE)
            .saturating_mul(4)
            .clamp(1024 * 1024, MAX_QUEUED_BYTES);
        if self
            .bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(size).filter(|next| *next <= byte_budget)
            })
            .is_err()
        {
            self.close();
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "presenter output byte budget exhausted",
            ));
        }
        let mut encoded = zeroize::Zeroizing::new(Vec::with_capacity(size));
        encoded.extend_from_slice(
            &RecordHeader {
                body_length,
                record_type,
                flags: 0,
                object_id,
                sequence,
            }
            .encode(),
        );
        for part in parts {
            encoded.extend_from_slice(part);
        }
        if inner.sender.try_send(Outbound::Record(encoded)).is_err() {
            self.bytes.fetch_sub(size, Ordering::AcqRel);
            self.close();
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "presenter output queue exhausted",
            ));
        }
        inner.sequence = sequence;
        Ok(sequence)
    }
}

fn write_complete(stream: &mut dyn Write, mut bytes: &[u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        match stream.write(bytes) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(count) if count <= bytes.len() => bytes = &bytes[count..],
            Ok(_) => return Err(invalid("writer reported more bytes than offered")),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn contextual(error: io::Error, part: &'static str) -> io::Error {
    io::Error::new(
        error.kind(),
        format!("failed to read Vivid {part}: {error}"),
    )
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::net::Shutdown;
    use std::os::unix::net::UnixStream;
    use std::thread;

    fn transport(stream: UnixStream) -> Transport {
        let reader = stream.try_clone().unwrap();
        Transport::new(
            Box::new(reader),
            Box::new(stream),
            ConnectionCancel::inert(),
            Arc::new(|_| Ok(())),
        )
    }

    #[test]
    fn exact_1_5_preface_is_accepted() {
        let (mut client, server) = UnixStream::pair().unwrap();
        client
            .write_all(&vivid_protocol::wire::encode_preface(
                ConnectionKind::Control,
                CONTROL_MAX_RECORD_BODY,
            ))
            .unwrap();
        let (_, preface, _) = Reader::new(transport(server)).unwrap();
        assert_eq!((preface.major, preface.minor), (1, 5));
        assert_eq!(preface.kind, ConnectionKind::Control);
    }

    #[test]
    fn valid_1_1_preface_gets_one_typed_version_error_then_close() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let service = thread::spawn(move || {
            let error = match Reader::new(transport(server)) {
                Ok(_) => panic!("Vivid 1.1 preface was accepted"),
                Err(error) => error,
            };
            assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        });
        client
            .write_all(&vivid_protocol::wire::encode_preface_version(
                ConnectionKind::Control,
                CONTROL_MAX_RECORD_BODY,
                1,
                1,
            ))
            .unwrap();
        client.shutdown(Shutdown::Write).unwrap();
        let mut reply = Vec::new();
        client.read_to_end(&mut reply).unwrap();
        service.join().unwrap();
        assert_eq!(reply, vivid_protocol::wire::unsupported_version_record());
    }
}

#[cfg(test)]
mod writer_audit_tests {
    use super::*;
    struct BadWriter {
        calls: usize,
    }
    impl Write for BadWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.calls += 1;
            match self.calls {
                1 => Ok(1),
                _ => Ok(bytes.len() + 1),
            }
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn partial_write_error_poisons_connection() {
        let writer = Writer::new(
            Box::new(BadWriter { calls: 0 }),
            ConnectionCancel::inert(),
            1024,
        )
        .unwrap();
        writer.write_record(3, 0, &[1]).unwrap();
        assert!(writer.flush().is_err());
        assert!(writer.write_record(3, 0, &[2]).is_err());
    }
    struct Blocked {
        entered: mpsc::SyncSender<()>,
        release: mpsc::Receiver<()>,
    }
    impl Write for Blocked {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            self.entered.send(()).unwrap();
            self.release.recv_timeout(Duration::from_secs(2)).unwrap();
            Err(io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn saturation_is_bounded_and_cancels_without_waiting_for_writer() {
        for body_size in [1, 1024 * 1024] {
            let (entered_tx, entered_rx) = mpsc::sync_channel(1);
            let (release_tx, release_rx) = mpsc::channel();
            let cancel = ConnectionCancel::new(move || {
                let _ = release_tx.send(());
            });
            let writer = Writer::new(
                Box::new(Blocked {
                    entered: entered_tx,
                    release: release_rx,
                }),
                cancel,
                HARD_MAX_RECORD_BODY,
            )
            .unwrap();
            let body = vec![0; body_size];
            writer.write_record(3, 0, &body).unwrap();
            entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
            let mut refused = false;
            for _ in 0..=MAX_QUEUED_RECORDS {
                if writer.write_record(3, 0, &body).is_err() {
                    refused = true;
                    break;
                }
            }
            assert!(refused);
            assert!(writer.closed.load(Ordering::Acquire));
            assert!(writer.bytes.load(Ordering::Acquire) <= MAX_QUEUED_BYTES);
        }
    }
}

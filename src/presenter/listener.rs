use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct Transport {
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
    cancel: ConnectionCancel,
    pub(crate) timeout: Arc<dyn Fn(Option<Duration>) -> io::Result<()> + Send + Sync>,
    pub(crate) deadline: Arc<Mutex<Option<Instant>>>,
}

impl std::fmt::Debug for Transport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Transport")
            .field("cancel", &self.cancel)
            .finish_non_exhaustive()
    }
}

/// Product-provided accepted-connection listener used by the inner presenter.
pub trait PresenterListener: Send + 'static {
    fn endpoint(&self) -> String;
    fn accept(&self) -> io::Result<Transport>;
}

impl Transport {
    /// Wraps a TCP stream with platform-aware deadlines and cancellation.
    ///
    /// # Errors
    /// Returns an I/O error if configuring or cloning the socket fails.
    pub fn from_tcp(stream: std::net::TcpStream) -> io::Result<Self> {
        super::socket::tcp_transport(stream)
    }

    /// Wraps accepted I/O with framing deadlines and connection cancellation.
    ///
    /// The timeout callback must configure the actual reader's wait. On Windows,
    /// reads use bounded waits to observe cancellation even while the peer remains
    /// connected; timed-out partial records resume without discarding their bytes.
    pub fn new(
        reader: Box<dyn Read + Send>,
        writer: Box<dyn Write + Send>,
        cancel: ConnectionCancel,
        timeout: Arc<dyn Fn(Option<Duration>) -> io::Result<()> + Send + Sync>,
    ) -> Self {
        let deadline = Arc::new(Mutex::new(None));
        let reader = Box::new(DeadlineReader {
            inner: reader,
            timeout: timeout.clone(),
            deadline: deadline.clone(),
            #[cfg(windows)]
            cancel: cancel.clone(),
        });
        Self {
            reader,
            writer,
            cancel,
            timeout,
            deadline,
        }
    }

    pub fn cancel(&self) -> ConnectionCancel {
        self.cancel.clone()
    }

    pub fn set_read_deadline(&self, timeout: Duration) -> io::Result<()> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "deadline is too large"))?;
        *self
            .deadline
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(deadline);
        (self.timeout)(Some(timeout))
    }
}

struct DeadlineReader {
    inner: Box<dyn Read + Send>,
    timeout: Arc<dyn Fn(Option<Duration>) -> io::Result<()> + Send + Sync>,
    deadline: Arc<Mutex<Option<Instant>>>,
    #[cfg(windows)]
    cancel: ConnectionCancel,
}

impl Read for DeadlineReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        #[cfg(windows)]
        {
            self.read_cancellable(buffer)
        }

        #[cfg(not(windows))]
        {
            let deadline = *self
                .deadline
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(deadline) = deadline {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "Vivid handshake deadline expired",
                    ));
                }
                (self.timeout)(Some(remaining))?;
            }
            self.inner.read(buffer)
        }
    }
}

#[cfg(windows)]
impl DeadlineReader {
    fn read_cancellable(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        // Winsock shutdown does not wake an already blocked receive. The TCP
        // backend polls readiness to observe cancellation between bounded waits.
        // Retrying here preserves partial headers/bodies inside read_exact.
        loop {
            if self.cancel.inner.cancelled.load(Ordering::Acquire) {
                return Err(io::ErrorKind::ConnectionAborted.into());
            }
            let deadline = *self
                .deadline
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut timeout = Duration::from_millis(100);
            if let Some(deadline) = deadline {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "Vivid handshake deadline expired",
                    ));
                }
                timeout = timeout.min(remaining);
            }
            (self.timeout)(Some(timeout))?;
            match self.inner.read(buffer) {
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    ) => {}
                result => return result,
            }
        }
    }
}

#[derive(Clone)]
pub struct ConnectionCancel {
    inner: Arc<CancelInner>,
}

impl std::fmt::Debug for ConnectionCancel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConnectionCancel")
            .field("cancelled", &self.inner.cancelled.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

struct CancelInner {
    cancelled: AtomicBool,
    callback: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl ConnectionCancel {
    pub fn new(callback: impl FnOnce() + Send + 'static) -> Self {
        Self {
            inner: Arc::new(CancelInner {
                cancelled: AtomicBool::new(false),
                callback: Mutex::new(Some(Box::new(callback))),
            }),
        }
    }

    pub fn inert() -> Self {
        Self::new(|| {})
    }

    pub fn cancel(&self) {
        if self.inner.cancelled.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Some(callback) = self
            .inner
            .callback
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            callback();
        }
    }
}

impl Drop for CancelInner {
    fn drop(&mut self) {
        if !self.cancelled.swap(true, Ordering::AcqRel)
            && let Some(callback) = self
                .callback
                .get_mut()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take()
        {
            callback();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct SlowBytes(&'static [u8]);

    impl Read for SlowBytes {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.0.is_empty() || buffer.is_empty() {
                return Ok(0);
            }
            std::thread::sleep(Duration::from_millis(20));
            buffer[0] = self.0[0];
            self.0 = &self.0[1..];
            Ok(1)
        }
    }

    #[test]
    fn read_deadline_is_absolute_across_partial_reads() {
        let transport = Transport::new(
            Box::new(SlowBytes(b"abc")),
            Box::new(io::sink()),
            ConnectionCancel::inert(),
            Arc::new(|_| Ok(())),
        );
        transport
            .set_read_deadline(Duration::from_millis(30))
            .unwrap();
        let mut reader = transport.reader;
        let mut bytes = [0; 3];
        assert_eq!(
            reader.read_exact(&mut bytes).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_cancel_wakes_idle_tcp_reader_and_preserves_partial_reads() {
        use std::net::TcpStream;
        use std::sync::mpsc;

        let listener = super::super::SocketListener::bind("tcp:127.0.0.1:0").unwrap();
        let endpoint = listener.endpoint();
        let mut peer = TcpStream::connect(endpoint.strip_prefix("tcp:").unwrap()).unwrap();
        let transport = listener.accept().unwrap();
        let cancel = transport.cancel();
        let (send, receive) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut reader = transport.reader;
            let mut writer = transport.writer;
            let mut bytes = [0; 2];
            reader.read_exact(&mut bytes).unwrap();
            writer.write_all(b"ok").unwrap();
            send.send(Ok(bytes)).unwrap();
            let error = reader.read_exact(&mut bytes).unwrap_err();
            send.send(Err(error.kind())).unwrap();
        });
        peer.write_all(b"a").unwrap();
        // Cross a polling timeout with an incomplete record still in read_exact.
        std::thread::sleep(Duration::from_millis(600));
        peer.write_all(b"b").unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut reply = [0; 2];
        peer.read_exact(&mut reply).unwrap();
        assert_eq!(&reply, b"ok");
        assert_eq!(
            receive.recv_timeout(Duration::from_secs(2)).unwrap(),
            Ok(*b"ab")
        );
        cancel.cancel();
        assert!(matches!(
            receive.recv_timeout(Duration::from_secs(2)).unwrap(),
            Err(io::ErrorKind::ConnectionAborted | io::ErrorKind::UnexpectedEof)
        ));
        worker.join().unwrap();
        drop(peer);
    }
}

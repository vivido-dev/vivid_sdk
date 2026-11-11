//! Hold one media body unread without changing authenticated socket traffic.

// Rust guideline compliant 2026-10-07
use std::{
    io::{self, Read},
    sync::{Arc, Condvar, Mutex},
    time::Duration,
};
use vivid_protocol::wire::{HEADER_SIZE, PREFACE_SIZE, RecordHeader};
use vivid_sdk::presenter::{PresenterListener, SocketListener, Transport};

#[derive(Default)]
struct GateState {
    armed: Option<u16>,
    reached: bool,
    released: bool,
}

#[derive(Clone, Default)]
pub struct Gate(Arc<(Mutex<GateState>, Condvar)>);

impl Gate {
    pub fn arm(&self, record_type: u16) {
        *self.0.0.lock().unwrap() = GateState {
            armed: Some(record_type),
            ..Default::default()
        };
    }

    pub fn wait(&self) {
        let (state, timeout) = self
            .0
            .1
            .wait_timeout_while(self.0.0.lock().unwrap(), Duration::from_secs(3), |state| {
                !state.reached
            })
            .unwrap();
        assert!(
            !timeout.timed_out() && state.reached,
            "media body never parked"
        );
    }

    pub fn release(&self) {
        self.0.0.lock().unwrap().released = true;
        self.0.1.notify_all();
    }

    fn block(&self, record_type: u16) -> io::Result<()> {
        let mut state = self.0.0.lock().unwrap();
        if state.armed != Some(record_type) {
            return Ok(());
        }
        state.armed = None;
        state.reached = true;
        self.0.1.notify_all();
        // A failed test must not leave a presenter thread parked indefinitely.
        let (state, _) = self
            .0
            .1
            .wait_timeout_while(state, Duration::from_secs(5), |state| !state.released)
            .unwrap();
        if !state.released {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "test gate expired"));
        }
        Ok(())
    }
}

pub struct Listener {
    inner: SocketListener,
    gate: Gate,
}

impl Listener {
    pub fn new() -> (Self, Gate) {
        let gate = Gate::default();
        (
            Self {
                inner: SocketListener::bind("tcp:127.0.0.1:0").unwrap(),
                gate: gate.clone(),
            },
            gate,
        )
    }
}

impl PresenterListener for Listener {
    fn endpoint(&self) -> String {
        self.inner.endpoint()
    }

    fn accept(&self) -> io::Result<Transport> {
        let mut transport = self.inner.accept()?;
        transport.reader = Box::new(Reader {
            inner: transport.reader,
            gate: self.gate.clone(),
            preface: PREFACE_SIZE,
            header: [0; HEADER_SIZE],
            offset: 0,
            body: 0,
        });
        Ok(transport)
    }
}

struct Reader {
    inner: Box<dyn Read + Send>,
    gate: Gate,
    preface: usize,
    header: [u8; HEADER_SIZE],
    offset: usize,
    body: usize,
}

impl Read for Reader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let remaining = if self.preface != 0 {
            self.preface
        } else if self.body != 0 {
            self.body
        } else {
            HEADER_SIZE - self.offset
        };
        let length = buffer.len().min(remaining);
        let count = self.inner.read(&mut buffer[..length])?;
        if self.preface != 0 {
            self.preface -= count;
        } else if self.body != 0 {
            self.body -= count;
        } else {
            self.header[self.offset..self.offset + count].copy_from_slice(&buffer[..count]);
            self.offset += count;
            if self.offset == HEADER_SIZE {
                let header = RecordHeader::decode(self.header);
                self.body = header.body_length as usize;
                self.offset = 0;
                self.gate.block(header.record_type)?;
            }
        }
        Ok(count)
    }
}

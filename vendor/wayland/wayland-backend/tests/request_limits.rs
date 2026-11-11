//! Count, byte, fragmentation and reconnect admission through real sockets.

// Rust guideline compliant 2026-10-07
use std::{
    io::Write as _,
    num::NonZeroU32,
    os::unix::net::UnixStream,
    sync::{Arc, Mutex},
};
use wayland_backend::rs::server::{
    Backend, ClientData, ClientId, DisconnectReason, RequestBudget, RequestLimits,
};

struct Status {
    limits: Option<RequestLimits>,
    shared: Option<Arc<RequestBudget>>,
    error: Mutex<Option<u32>>,
}
impl ClientData for Status {
    fn request_limits(&self) -> Option<RequestLimits> {
        self.limits
    }
    fn shared_request_budget(&self) -> Option<Arc<RequestBudget>> {
        self.shared.clone()
    }
    fn disconnected(&self, _: ClientId, reason: DisconnectReason) {
        if let DisconnectReason::ProtocolError(error) = reason {
            *self.error.lock().unwrap() = Some(error.code);
        }
    }
}
fn limits(requests: u32, bytes: u32) -> RequestLimits {
    RequestLimits {
        request_burst: NonZeroU32::new(requests).unwrap(),
        requests_per_second: NonZeroU32::new(1).unwrap(),
        byte_burst: NonZeroU32::new(bytes).unwrap(),
        bytes_per_second: NonZeroU32::new(1).unwrap(),
    }
}
fn connect(
    backend: &Backend<()>,
    limits: Option<RequestLimits>,
    shared: Option<Arc<RequestBudget>>,
) -> (UnixStream, Arc<Status>) {
    let (server, peer) = UnixStream::pair().unwrap();
    let state = Arc::new(Status { limits, shared, error: Mutex::new(None) });
    backend.handle().insert_client(server, state.clone()).unwrap();
    (peer, state)
}
fn sync() -> Vec<u8> {
    [1u32, 12u32 << 16, 2].into_iter().flat_map(u32::to_ne_bytes).collect()
}
fn dispatch(backend: &mut Backend<()>) {
    backend.dispatch_all_clients(&mut ()).unwrap();
}

#[test]
fn core_request_count_is_charged_without_application_callbacks() {
    let mut backend = Backend::<()>::new().unwrap();
    let (mut peer, state) = connect(&backend, Some(limits(2, 1024)), None);
    peer.write_all(&sync()).unwrap();
    dispatch(&mut backend);
    peer.write_all(&sync()).unwrap();
    dispatch(&mut backend);
    assert_eq!(*state.error.lock().unwrap(), None);
    peer.write_all(&sync()).unwrap();
    dispatch(&mut backend);
    assert_eq!(*state.error.lock().unwrap(), Some(2));
}

#[test]
fn declared_wire_bytes_are_charged_once_across_body_read_splits() {
    let mut backend = Backend::<()>::new().unwrap();
    let (mut peer, state) = connect(&backend, Some(limits(100, 24)), None);
    for _ in 0..2 {
        let bytes = sync();
        peer.write_all(&bytes[..8]).unwrap();
        dispatch(&mut backend);
        assert_eq!(*state.error.lock().unwrap(), None);
        peer.write_all(&bytes[8..]).unwrap();
        dispatch(&mut backend);
        assert_eq!(*state.error.lock().unwrap(), None);
    }
    // A header alone is enough for pre-parse admission to reject the next body.
    peer.write_all(&sync()[..8]).unwrap();
    dispatch(&mut backend);
    assert_eq!(*state.error.lock().unwrap(), Some(2));
}

#[test]
fn a_header_waiting_for_a_descriptor_keeps_its_original_admission() {
    use rustix::net::{sendmsg, SendAncillaryBuffer, SendAncillaryMessage, SendFlags};
    use std::{
        io::IoSlice,
        mem::MaybeUninit,
        os::fd::{AsFd as _, OwnedFd},
        sync::atomic::{AtomicUsize, Ordering},
    };
    use wayland_backend::{
        protocol::{Argument, ArgumentType, Interface, Message, MessageDesc},
        rs::server::{Handle, ObjectData, ObjectId},
    };
    struct Receiver(Arc<AtomicUsize>);
    impl ObjectData<()> for Receiver {
        fn request(
            self: Arc<Self>,
            _: &Handle,
            _: &mut (),
            _: ClientId,
            request: Message<ObjectId, OwnedFd>,
        ) -> Option<Arc<dyn ObjectData<()>>> {
            assert!(matches!(request.args[0], Argument::Fd(_)));
            self.0.fetch_add(1, Ordering::Relaxed);
            None
        }
        fn destroyed(self: Arc<Self>, _: &Handle, _: &mut (), _: ClientId, _: ObjectId) {}
    }
    static INTERFACE: Interface = Interface {
        name: "vivid_rate_fd",
        version: 1,
        c_ptr: None,
        events: &[],
        requests: &[MessageDesc {
            name: "receive",
            signature: &[ArgumentType::Fd],
            since: 1,
            is_destructor: false,
            child_interface: None,
            arg_interfaces: &[],
        }],
    };
    let mut backend = Backend::<()>::new().unwrap();
    let (server, mut peer) = UnixStream::pair().unwrap();
    let state =
        Arc::new(Status { limits: Some(limits(1, 8)), shared: None, error: Mutex::new(None) });
    let owner = backend.handle().insert_client(server, state.clone()).unwrap();
    let received = Arc::new(AtomicUsize::new(0));
    let object = backend
        .handle()
        .create_object::<()>(owner, &INTERFACE, 1, Arc::new(Receiver(received.clone())))
        .unwrap();
    let bytes = [object.protocol_id(), 8u32 << 16]
        .into_iter()
        .flat_map(u32::to_ne_bytes)
        .collect::<Vec<_>>();
    peer.write_all(&bytes).unwrap();
    dispatch(&mut backend);
    assert_eq!(received.load(Ordering::Relaxed), 0);
    assert_eq!(*state.error.lock().unwrap(), None);
    let file = std::fs::File::open("/dev/null").unwrap();
    let fds = [file.as_fd()];
    let mut storage = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
    let mut ancillary = SendAncillaryBuffer::new(&mut storage);
    assert!(ancillary.push(SendAncillaryMessage::ScmRights(&fds)));
    assert_eq!(
        sendmsg(&peer, &[IoSlice::new(&bytes[..1])], &mut ancillary, SendFlags::NOSIGNAL).unwrap(),
        1
    );
    dispatch(&mut backend);
    assert_eq!(received.load(Ordering::Relaxed), 1);
    assert_eq!(*state.error.lock().unwrap(), None);
    peer.write_all(&bytes[1..]).unwrap();
    dispatch(&mut backend);
    assert_eq!(*state.error.lock().unwrap(), Some(2));
    assert_eq!(received.load(Ordering::Relaxed), 1);
}

#[test]
fn reconnects_share_group_debt_while_another_group_reuses_the_same_callback_id() {
    let mut backend = Backend::<()>::new().unwrap();
    let budget = Arc::new(RequestBudget::new(limits(2, 1024)));
    let (mut first, a) = connect(&backend, None, Some(budget.clone()));
    for _ in 0..2 {
        first.write_all(&sync()).unwrap();
        dispatch(&mut backend);
    }
    assert_eq!(*a.error.lock().unwrap(), None);
    for _ in 0..2 {
        let (mut next, b) = connect(&backend, None, Some(budget.clone()));
        next.write_all(&sync()).unwrap();
        dispatch(&mut backend);
        assert_eq!(*b.error.lock().unwrap(), Some(2));
        assert_eq!(*a.error.lock().unwrap(), None);
    }
    let other_budget = Arc::new(RequestBudget::new(limits(2, 1024)));
    let (mut other, state) = connect(&backend, None, Some(other_budget));
    other.write_all(&sync()).unwrap();
    dispatch(&mut backend);
    assert_eq!(*state.error.lock().unwrap(), None);
}

#[test]
fn role_transitions_select_existing_group_debt_without_resetting_it() {
    use std::sync::atomic::{AtomicBool, Ordering};
    struct Role {
        protected: AtomicBool,
        root: Arc<RequestBudget>,
        helpers: Arc<RequestBudget>,
        error: Mutex<Option<u32>>,
    }
    impl ClientData for Role {
        fn shared_request_budget(&self) -> Option<Arc<RequestBudget>> {
            Some(if self.protected.load(Ordering::Acquire) {
                self.root.clone()
            } else {
                self.helpers.clone()
            })
        }
        fn disconnected(&self, _: ClientId, reason: DisconnectReason) {
            if let DisconnectReason::ProtocolError(error) = reason {
                *self.error.lock().unwrap() = Some(error.code);
            }
        }
    }
    let mut backend = Backend::<()>::new().unwrap();
    let (server, mut peer) = UnixStream::pair().unwrap();
    let role = Arc::new(Role {
        protected: AtomicBool::new(false),
        root: Arc::new(RequestBudget::new(limits(2, 1024))),
        helpers: Arc::new(RequestBudget::new(limits(2, 1024))),
        error: Mutex::new(None),
    });
    backend.handle().insert_client(server, role.clone()).unwrap();
    for _ in 0..2 {
        peer.write_all(&sync()).unwrap();
        dispatch(&mut backend);
    }
    role.protected.store(true, Ordering::Release);
    for _ in 0..2 {
        peer.write_all(&sync()).unwrap();
        dispatch(&mut backend);
    }
    assert_eq!(*role.error.lock().unwrap(), None);
    role.protected.store(false, Ordering::Release);
    peer.write_all(&sync()).unwrap();
    dispatch(&mut backend);
    assert_eq!(*role.error.lock().unwrap(), Some(2));
}

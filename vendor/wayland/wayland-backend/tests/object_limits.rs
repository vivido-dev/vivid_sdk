//! Scoped object admission through the Rust backend's public transport API.

// Rust guideline compliant 2026-10-07
use std::{
    io::{Read as _, Write as _},
    num::NonZeroUsize,
    os::{fd::OwnedFd, unix::net::UnixStream},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use wayland_backend::{
    protocol::{Interface, Message, ANONYMOUS_INTERFACE},
    rs::server::{
        Backend, ClientData, ClientId, DisconnectReason, GlobalHandler, GlobalId, Handle,
        ObjectData, ObjectId,
    },
};

#[derive(Default)]
struct Status(Mutex<Option<u32>>);
impl ClientData for Status {
    fn max_object_slots(&self) -> Option<NonZeroUsize> {
        NonZeroUsize::new(4)
    }
    fn disconnected(&self, _: ClientId, reason: DisconnectReason) {
        if let DisconnectReason::ProtocolError(error) = reason {
            *self.0.lock().unwrap() = Some(error.code);
        }
    }
}

struct Object;
impl ObjectData<()> for Object {
    fn request(
        self: Arc<Self>,
        _: &Handle,
        _: &mut (),
        _: ClientId,
        _: Message<ObjectId, OwnedFd>,
    ) -> Option<Arc<dyn ObjectData<()>>> {
        unreachable!("test objects have no requests")
    }
    fn destroyed(self: Arc<Self>, _: &Handle, _: &mut (), _: ClientId, _: ObjectId) {}
}

fn sync(socket: &mut UnixStream, id: u32) {
    let request =
        [1u32, 12u32 << 16, id].into_iter().flat_map(u32::to_ne_bytes).collect::<Vec<_>>();
    socket.write_all(&request).unwrap();
}

#[test]
fn server_object_exhaustion_preserves_another_owner_with_reused_numeric_ids() {
    let mut backend = Backend::<()>::new().unwrap();
    let mut handle = backend.handle();
    let (first, _first_peer) = UnixStream::pair().unwrap();
    let status = Arc::new(Status::default());
    let owner = handle.insert_client(first, status.clone()).unwrap();
    let (other, mut peer) = UnixStream::pair().unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
    let other_status = Arc::new(Status::default());
    let other_owner = handle.insert_client(other, other_status.clone()).unwrap();
    let first_id = handle
        .create_object::<()>(owner.clone(), &ANONYMOUS_INTERFACE, 1, Arc::new(Object))
        .unwrap();
    let other_id =
        handle.create_object::<()>(other_owner, &ANONYMOUS_INTERFACE, 1, Arc::new(Object)).unwrap();
    assert_eq!(first_id.protocol_id(), other_id.protocol_id());
    for _ in 0..2 {
        handle
            .create_object::<()>(owner.clone(), &ANONYMOUS_INTERFACE, 1, Arc::new(Object))
            .unwrap();
    }
    assert!(handle.create_object::<()>(owner, &ANONYMOUS_INTERFACE, 1, Arc::new(Object)).is_err());
    assert_eq!(*status.0.lock().unwrap(), Some(2));
    assert_eq!(*other_status.0.lock().unwrap(), None);
    assert!(handle.object_info(other_id).is_ok());
    sync(&mut peer, 2);
    backend.dispatch_all_clients(&mut ()).unwrap();
    backend.flush(None).unwrap();
    assert!(peer.read(&mut [0; 64]).unwrap() > 0);
}

#[test]
fn malformed_client_ids_cannot_claim_the_server_namespace_or_skip_table_slots() {
    for id in [0xff00_0000, 0, 1000] {
        let mut backend = Backend::<()>::new().unwrap();
        let mut handle = backend.handle();
        let (socket, mut peer) = UnixStream::pair().unwrap();
        let status = Arc::new(Status::default());
        handle.insert_client(socket, status.clone()).unwrap();
        sync(&mut peer, id);
        backend.dispatch_all_clients(&mut ()).unwrap();
        assert_eq!(*status.0.lock().unwrap(), Some(0));
    }
}

#[test]
fn completed_callbacks_reuse_slots_but_increasing_ids_remain_charged() {
    let mut backend = Backend::<()>::new().unwrap();
    let mut handle = backend.handle();
    let (socket, mut peer) = UnixStream::pair().unwrap();
    let status = Arc::new(Status::default());
    handle.insert_client(socket, status.clone()).unwrap();
    for _ in 0..20 {
        sync(&mut peer, 2);
        backend.dispatch_all_clients(&mut ()).unwrap();
    }
    assert_eq!(*status.0.lock().unwrap(), None);
    for id in 3..=5 {
        sync(&mut peer, id);
        backend.dispatch_all_clients(&mut ()).unwrap();
    }
    assert_eq!(*status.0.lock().unwrap(), Some(2));
}

#[test]
fn registry_allocation_is_bounded_without_application_callbacks() {
    let mut backend = Backend::<()>::new().unwrap();
    let mut handle = backend.handle();
    let (socket, mut peer) = UnixStream::pair().unwrap();
    let status = Arc::new(Status::default());
    handle.insert_client(socket, status.clone()).unwrap();
    for id in 2..=5 {
        let bytes = [1u32, (12u32 << 16) | 1, id]
            .into_iter()
            .flat_map(u32::to_ne_bytes)
            .collect::<Vec<_>>();
        peer.write_all(&bytes).unwrap();
        backend.dispatch_all_clients(&mut ()).unwrap();
    }
    assert_eq!(*status.0.lock().unwrap(), Some(2));
}

struct Global(AtomicUsize);
static TEST_INTERFACE: Interface =
    Interface { name: "vivid_test", version: 1, requests: &[], events: &[], c_ptr: None };
impl GlobalHandler<()> for Global {
    fn bind(
        self: Arc<Self>,
        _: &Handle,
        _: &mut (),
        _: ClientId,
        _: GlobalId,
        _: ObjectId,
    ) -> Arc<dyn ObjectData<()>> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Arc::new(Object)
    }
}

#[test]
fn registry_binding_checks_admission_before_initializing_user_data() {
    let mut backend = Backend::<()>::new().unwrap();
    let mut handle = backend.handle();
    let global = Arc::new(Global(AtomicUsize::new(0)));
    handle.create_global::<()>(&TEST_INTERFACE, 1, global.clone());
    let (socket, mut peer) = UnixStream::pair().unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
    let status = Arc::new(Status::default());
    handle.insert_client(socket, status.clone()).unwrap();
    peer.write_all(
        &[1u32, (12u32 << 16) | 1, 2].into_iter().flat_map(u32::to_ne_bytes).collect::<Vec<_>>(),
    )
    .unwrap();
    backend.dispatch_all_clients(&mut ()).unwrap();
    backend.flush(None).unwrap();
    let mut event = [0; 128];
    assert!(peer.read(&mut event).unwrap() >= 12);
    let name = u32::from_ne_bytes(event[8..12].try_into().unwrap());
    for id in 3..=5 {
        let mut interface = TEST_INTERFACE.name.as_bytes().to_vec();
        interface.push(0);
        let len = interface.len() as u32;
        while interface.len() % 4 != 0 {
            interface.push(0);
        }
        let size = (24 + interface.len()) as u32;
        let mut request = [2u32, size << 16, name, len]
            .into_iter()
            .flat_map(u32::to_ne_bytes)
            .collect::<Vec<_>>();
        request.extend(interface);
        request.extend([1u32, id].into_iter().flat_map(u32::to_ne_bytes));
        peer.write_all(&request).unwrap();
        backend.dispatch_all_clients(&mut ()).unwrap();
    }
    assert_eq!(global.0.load(Ordering::Relaxed), 2);
    assert_eq!(*status.0.lock().unwrap(), Some(2));
}

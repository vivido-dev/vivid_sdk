//! Outbound saturation and exact-owner descriptor cleanup through the Rust API.

// Rust guideline compliant 2026-10-07
use rustix::net::{
    recvmsg, send, sockopt, RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, SendFlags,
};
use std::{
    io::{IoSliceMut, Read as _, Write as _},
    mem::MaybeUninit,
    num::NonZeroUsize,
    os::{
        fd::{AsRawFd as _, OwnedFd},
        unix::net::UnixStream,
    },
    sync::{Arc, Mutex},
};
use wayland_backend::{
    protocol::{Argument, ArgumentType, Interface, Message, MessageDesc},
    rs::server::{Backend, ClientData, ClientId, DisconnectReason, Handle, ObjectData, ObjectId},
    smallvec::smallvec,
};

#[derive(Default)]
struct Status(Mutex<Option<u32>>);
impl ClientData for Status {
    fn max_buffered_fds(&self) -> Option<NonZeroUsize> {
        NonZeroUsize::new(2)
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
        unreachable!("fixture has no requests")
    }
    fn destroyed(self: Arc<Self>, _: &Handle, _: &mut (), _: ClientId, _: ObjectId) {}
}
static INTERFACE: Interface = Interface {
    name: "vivid_fd_fixture",
    version: 1,
    requests: &[],
    c_ptr: None,
    events: &[MessageDesc {
        name: "descriptor",
        signature: &[ArgumentType::Fd],
        since: 1,
        is_destructor: false,
        child_interface: None,
        arg_interfaces: &[],
    }],
};

#[test]
fn blocked_outbound_queue_releases_only_its_owner_and_preserves_reused_ids() {
    let mut backend = Backend::<()>::new().unwrap();
    let mut handle = backend.handle();
    let (socket, _blocked_peer) = UnixStream::pair().unwrap();
    sockopt::set_socket_send_buffer_size(&socket, 1024).unwrap();
    loop {
        match send(&socket, &[0; 4096], SendFlags::DONTWAIT | SendFlags::NOSIGNAL) {
            Ok(count) => assert!(count > 0),
            Err(rustix::io::Errno::AGAIN) => break,
            Err(error) => panic!("fill fixture socket: {error}"),
        }
    }
    let status = Arc::new(Status::default());
    let owner = handle.insert_client(socket, status.clone()).unwrap();
    let object = handle.create_object::<()>(owner, &INTERFACE, 1, Arc::new(Object)).unwrap();
    let (socket, peer) = UnixStream::pair().unwrap();
    let other_status = Arc::new(Status::default());
    let other = handle.insert_client(socket, other_status.clone()).unwrap();
    let other_object = handle.create_object::<()>(other, &INTERFACE, 1, Arc::new(Object)).unwrap();
    assert_eq!(object.protocol_id(), other_object.protocol_id());
    let (mut probe, writer) = UnixStream::pair().unwrap();
    probe.set_nonblocking(true).unwrap();
    let (mut other_probe, other_writer) = UnixStream::pair().unwrap();
    handle
        .send_event(Message {
            sender_id: other_object,
            opcode: 0,
            args: smallvec![Argument::Fd(other_writer.as_raw_fd())],
        })
        .unwrap();
    for _ in 0..2 {
        handle
            .send_event(Message {
                sender_id: object.clone(),
                opcode: 0,
                args: smallvec![Argument::Fd(writer.as_raw_fd())],
            })
            .unwrap();
    }
    assert!(handle
        .send_event(Message {
            sender_id: object,
            opcode: 0,
            args: smallvec![Argument::Fd(writer.as_raw_fd())]
        })
        .is_err());
    assert_eq!(*status.0.lock().unwrap(), Some(2));
    assert_eq!(*other_status.0.lock().unwrap(), None);
    drop(writer);
    assert_eq!(
        probe.read(&mut [0]).unwrap(),
        0,
        "rejected owner's outbound descriptors survived failure"
    );
    backend.flush(None).unwrap();
    let mut bytes = [0; 64];
    let mut storage = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(2))];
    let mut ancillary = RecvAncillaryBuffer::new(&mut storage);
    assert_eq!(
        recvmsg(&peer, &mut [IoSliceMut::new(&mut bytes)], &mut ancillary, RecvFlags::DONTWAIT)
            .unwrap()
            .bytes,
        8
    );
    let fds = ancillary
        .drain()
        .filter_map(|item| match item {
            RecvAncillaryMessage::ScmRights(fds) => Some(fds),
            _ => None,
        })
        .flatten()
        .collect::<Vec<_>>();
    assert_eq!(fds.len(), 1);
    UnixStream::from(fds.into_iter().next().unwrap()).write_all(b"x").unwrap();
    let mut byte = [0];
    other_probe.read_exact(&mut byte).unwrap();
    assert_eq!(&byte, b"x");
}

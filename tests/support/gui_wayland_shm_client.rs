//! A synchronous test driver backed by a real native SHM connection.
#![cfg(all(feature = "gui-wayland", target_os = "linux"))]

// Rust guideline compliant 2026-10-07
use std::{
    fs::File,
    net::Shutdown,
    os::{fd::AsFd as _, unix::net::UnixStream},
    sync::mpsc,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use vivid_sdk::gui_wayland::WaylandHost;
use wayland_client::{
    Connection, Dispatch, DispatchError, QueueHandle,
    backend::WaylandError,
    delegate_noop,
    protocol::{
        wl_buffer, wl_callback, wl_compositor, wl_region, wl_registry, wl_shm, wl_shm_pool,
        wl_subcompositor, wl_subsurface, wl_surface,
    },
};

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Failure {
    Protocol(u32, String),
    Io(std::io::ErrorKind),
    BadMessage,
}
pub(super) type Reply = Result<(), Failure>;
#[derive(Clone, Copy)]
pub(super) enum Operation {
    RegionOperations(usize, bool),
    Damage(usize, bool),
    Frames(usize),
    CachedCommits(usize, bool),
    CommitSurface,
    AssignRegion,
    Regions(usize),
    Pool(usize),
    BadPool,
    Resize(usize, i32),
    Buffer(usize),
    DropPool(usize),
    DropBuffer(usize),
}
struct Pool {
    handle: Option<wl_shm_pool::WlShmPool>,
    file: File,
}
#[derive(Default)]
struct Client {
    region: Option<wl_region::WlRegion>,
    surface: Option<wl_surface::WlSurface>,
    tree: Option<(wl_surface::WlSurface, wl_surface::WlSurface)>,
    subcompositor: Option<wl_subcompositor::WlSubcompositor>,
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    pools: Vec<Pool>,
    buffers: Vec<Option<wl_buffer::WlBuffer>>,
}
impl Dispatch<wl_registry::WlRegistry, ()> for Client {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_subcompositor" => state.subcompositor = Some(registry.bind(name, 1, qh, ())),
                "wl_shm" => state.shm = Some(registry.bind(name, version.min(2), qh, ())),
                "wl_compositor" => {
                    state.compositor = Some(registry.bind(name, version.min(4), qh, ()))
                }
                _ => (),
            }
        }
    }
}
delegate_noop!(Client: ignore wl_shm::WlShm);
delegate_noop!(Client: ignore wl_shm_pool::WlShmPool);
delegate_noop!(Client: ignore wl_buffer::WlBuffer);
delegate_noop!(Client: ignore wl_compositor::WlCompositor);
delegate_noop!(Client: ignore wl_region::WlRegion);
delegate_noop!(Client: ignore wl_surface::WlSurface);
delegate_noop!(Client: ignore wl_subcompositor::WlSubcompositor);
delegate_noop!(Client: ignore wl_subsurface::WlSubsurface);
delegate_noop!(Client: ignore wl_callback::WlCallback);

impl Client {
    fn apply(&mut self, operation: Operation, qh: &QueueHandle<Self>) {
        match operation {
            Operation::CommitSurface => self.surface.as_ref().unwrap().commit(),
            Operation::AssignRegion => {
                let surface = self.surface.as_ref().unwrap();
                surface.set_input_region(self.region.as_ref());
                surface.set_opaque_region(self.region.as_ref());
            }
            Operation::RegionOperations(count, subtract) => {
                let region = self
                    .region
                    .get_or_insert_with(|| self.compositor.as_ref().unwrap().create_region(qh, ()));
                for _ in 0..count {
                    if subtract {
                        region.subtract(0, 0, 1, 1);
                    } else {
                        region.add(0, 0, 1, 1);
                    }
                }
            }
            Operation::Damage(count, buffer) => {
                let surface = self.surface.get_or_insert_with(|| {
                    self.compositor.as_ref().unwrap().create_surface(qh, ())
                });
                for _ in 0..count {
                    if buffer {
                        surface.damage_buffer(0, 0, 1, 1);
                    } else {
                        surface.damage(0, 0, 1, 1);
                    }
                }
            }
            Operation::Frames(count) => {
                let surface = self.surface.get_or_insert_with(|| {
                    self.compositor.as_ref().unwrap().create_surface(qh, ())
                });
                for _ in 0..count {
                    surface.frame(qh, ());
                }
            }
            Operation::CachedCommits(count, publish) => {
                let (parent, child) = self.tree.get_or_insert_with(|| {
                    let compositor = self.compositor.as_ref().unwrap();
                    let parent = compositor.create_surface(qh, ());
                    let child = compositor.create_surface(qh, ());
                    self.subcompositor
                        .as_ref()
                        .unwrap()
                        .get_subsurface(&child, &parent, qh, ());
                    (parent, child)
                });
                self.surface = Some(child.clone());
                for _ in 0..count {
                    child.commit();
                }
                if publish {
                    parent.commit();
                }
            }
            Operation::Regions(count) => {
                for _ in 0..count {
                    self.compositor.as_ref().unwrap().create_region(qh, ());
                }
            }
            Operation::Pool(bytes) => {
                let file = tempfile::tempfile().unwrap();
                file.set_len(bytes as u64).unwrap();
                let handle = self.shm.as_ref().unwrap().create_pool(
                    file.as_fd(),
                    i32::try_from(bytes).unwrap(),
                    qh,
                    (),
                );
                self.pools.push(Pool {
                    file,
                    handle: Some(handle),
                });
            }
            Operation::BadPool => {
                let (socket, _peer) = UnixStream::pair().unwrap();
                self.shm
                    .as_ref()
                    .unwrap()
                    .create_pool(socket.as_fd(), 4, qh, ());
            }
            Operation::Resize(index, bytes) => {
                let pool = &self.pools[index];
                if bytes > 0 {
                    pool.file.set_len(bytes as u64).unwrap();
                }
                pool.handle.as_ref().unwrap().resize(bytes);
            }
            Operation::Buffer(pool) => {
                let buffer = self.pools[pool].handle.as_ref().unwrap().create_buffer(
                    0,
                    1,
                    1,
                    4,
                    wl_shm::Format::Argb8888,
                    qh,
                    (),
                );
                self.buffers.push(Some(buffer));
            }
            Operation::DropPool(index) => self.pools[index].handle.take().unwrap().destroy(),
            Operation::DropBuffer(index) => self.buffers[index].take().unwrap().destroy(),
        }
    }
}

pub(super) struct Peer {
    socket: UnixStream,
    commands: Option<mpsc::SyncSender<Operation>>,
    replies: mpsc::Receiver<Reply>,
    worker: Option<JoinHandle<()>>,
}
impl Peer {
    pub(super) fn new(host: &WaylandHost) -> Self {
        let socket = UnixStream::connect(host.socket_path()).unwrap();
        let client_socket = socket.try_clone().unwrap();
        let (commands, incoming) = mpsc::sync_channel(1);
        let (outgoing, replies) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            let connection = Connection::from_socket(client_socket).unwrap();
            let mut queue = connection.new_event_queue();
            let qh = queue.handle();
            let mut client = Client::default();
            connection.display().get_registry(&qh, ());
            if queue.roundtrip(&mut client).is_err() {
                return;
            }
            while let Ok(operation) = incoming.recv() {
                client.apply(operation, &qh);
                let reply = queue
                    .roundtrip(&mut client)
                    .map(|_| ())
                    .map_err(|error| match error {
                        DispatchError::Backend(WaylandError::Protocol(error)) => {
                            Failure::Protocol(error.code, error.object_interface)
                        }
                        DispatchError::Backend(WaylandError::Io(error)) => {
                            Failure::Io(error.kind())
                        }
                        DispatchError::BadMessage { .. } => Failure::BadMessage,
                    });
                let failed = reply.is_err();
                if outgoing.send(reply).is_err() || failed {
                    break;
                }
            }
        });
        Self {
            socket,
            commands: Some(commands),
            replies,
            worker: Some(worker),
        }
    }
    pub(super) fn apply(&self, host: &mut WaylandHost, operation: Operation) -> Reply {
        self.commands.as_ref().unwrap().send(operation).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match self.replies.try_recv() {
                Ok(reply) => return reply,
                Err(mpsc::TryRecvError::Empty) => (),
                Err(error) => panic!("native SHM client ended unexpectedly: {error}"),
            }
            assert!(Instant::now() < deadline, "native SHM operation timed out");
            if let Some(frame) = host.dispatch().unwrap() {
                assert!(host.presented(frame.id, frame.target));
            }
            thread::sleep(Duration::from_millis(1));
        }
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.socket.shutdown(Shutdown::Both);
        self.commands.take();
        self.worker.take().unwrap().join().unwrap();
    }
}

//! Real native clients, independent displays, and presentation-qualified input.
#![cfg(all(feature = "gui-wayland", target_os = "linux"))]

// Rust guideline compliant 2026-10-07
use std::{
    io::Write as _,
    os::fd::AsFd as _,
    os::unix::net::UnixStream,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use vivid_sdk::gui_wayland::{Frame, Input, WaylandHost};
use wayland_client::{
    Connection, Dispatch, Proxy as _, QueueHandle, WEnum, delegate_noop,
    protocol::{
        wl_buffer, wl_callback, wl_compositor, wl_keyboard, wl_pointer, wl_registry, wl_seat,
        wl_shm, wl_shm_pool, wl_surface,
    },
};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

struct Client(Child);
impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn client(host: &WaylandHost) -> Client {
    Client(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "wayland_child_fixture",
                "--nocapture",
            ])
            .env("VIVID_TEST_WAYLAND_SOCKET", host.socket_path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    )
}
fn next(host: &mut WaylandHost) -> Frame {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(frame) = host.dispatch().unwrap() {
            return frame;
        }
        assert!(
            Instant::now() < deadline,
            "Wayland client did not supply pixels"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn present(host: &mut WaylandHost, frame: &Frame) {
    assert!(host.presented(frame.id, frame.target));
}
fn color(frame: &Frame) -> [u8; 4] {
    frame.rgba[..4].try_into().unwrap()
}

#[test]
fn private_displays_reject_foreign_and_stale_input_and_keep_peers_alive() {
    let mut first = WaylandHost::new(64, 48).unwrap();
    let mut second = WaylandHost::new(64, 48).unwrap();
    assert_ne!(first.socket_path(), second.socket_path());
    use std::os::unix::fs::PermissionsExt as _;
    assert_eq!(
        std::fs::metadata(first.socket_path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(first.runtime_directory())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    let _first_client = client(&first);
    let _second_client = client(&second);
    let original = next(&mut first);
    let peer = next(&mut second);
    assert_eq!(original.id, peer.id);
    assert_ne!(original.target, peer.target);
    assert_eq!(original.logical_size, [64, 48]);
    assert_eq!(color(&original), [10, 20, 30, 255]);
    assert_eq!(
        &original.rgba[4..8],
        &peer.rgba[4..8],
        "fixture roots did not reuse protocol object IDs"
    );
    assert_ne!(original.rgba[4], 0);
    first.set_active(true);
    second.set_active(true);
    assert!(first.input_target().is_none());
    assert!(
        !first
            .input(
                original.target,
                Input::Key {
                    code: 30,
                    pressed: true
                }
            )
            .unwrap()
    );
    assert!(!second.presented(original.id, original.target));
    present(&mut first, &original);
    present(&mut second, &peer);
    let after_callback = next(&mut first);
    let peer_callback = next(&mut second);
    assert_eq!(color(&after_callback), [11, 20, 30, 255]);
    assert_eq!(color(&peer_callback), [11, 20, 30, 255]);
    present(&mut first, &after_callback);
    present(&mut second, &peer_callback);
    assert!(
        !first
            .input(
                peer.target,
                Input::Key {
                    code: 30,
                    pressed: true
                }
            )
            .unwrap()
    );
    assert!(
        first
            .input(
                original.target,
                Input::Key {
                    code: 30,
                    pressed: true
                }
            )
            .unwrap()
    );
    assert!(
        !first
            .input(
                original.target,
                Input::Key {
                    code: 30,
                    pressed: true
                }
            )
            .unwrap(),
        "repeats must not become presses"
    );
    let pressed = next(&mut first);
    assert_eq!(color(&pressed), [11, 21, 30, 255]);
    present(&mut first, &pressed);
    first.set_active(false);
    let released = next(&mut first);
    assert_eq!(color(&released), [11, 21, 31, 255]);
    present(&mut first, &released);
    assert!(first.input_target().is_none());
    first.set_active(true);
    assert!(
        first
            .input(original.target, Input::Motion { x: 1., y: 2. })
            .unwrap()
    );
    assert!(
        first
            .input(
                original.target,
                Input::Button {
                    code: 0x110,
                    pressed: true
                }
            )
            .unwrap()
    );
    let clicked = next(&mut first);
    assert_eq!(color(&clicked), [200, 21, 31, 255]);
    present(&mut first, &clicked);
    first.set_active(false);
    let cancelled = next(&mut first);
    assert_eq!(
        color(&cancelled),
        [200, 21, 31, 255],
        "focus loss activated a release-based widget"
    );
    present(&mut first, &cancelled);
    first.set_active(true);
    first.resize(100, 80).unwrap();
    assert!(
        !first
            .input(
                original.target,
                Input::Key {
                    code: 30,
                    pressed: false
                }
            )
            .unwrap()
    );
    assert!(!first.presented(clicked.id, clicked.target));
    let resized = next(&mut first);
    assert_eq!(resized.logical_size, [100, 80]);
    assert_ne!(resized.target, original.target);
    assert!(first.input_target().is_none());
    present(&mut first, &resized);
    let resize_callback = next(&mut first);
    present(&mut first, &resize_callback);
    assert!(
        first
            .input(resized.target, Input::Motion { x: f64::NAN, y: 1. })
            .is_err()
    );
    assert!(
        first
            .input(resized.target, Input::Motion { x: 100., y: 1. })
            .is_err()
    );
    assert!(
        !first
            .input(
                original.target,
                Input::Key {
                    code: 30,
                    pressed: true
                }
            )
            .unwrap()
    );
    for _ in 0..20 {
        assert!(
            second.dispatch().unwrap().is_none(),
            "another owner's input changed the peer"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        first
            .input(
                resized.target,
                Input::Key {
                    code: 31,
                    pressed: true
                }
            )
            .unwrap()
    );
    let remapped = next(&mut first);
    assert_eq!(remapped.logical_size, resized.logical_size);
    assert_ne!(
        remapped.target, resized.target,
        "unmap/remap reused old input authority"
    );
    assert!(first.input_target().is_none());
    assert!(
        !first
            .input(
                resized.target,
                Input::Key {
                    code: 30,
                    pressed: true
                }
            )
            .unwrap()
    );
    present(&mut first, &remapped);
    let directory = first.runtime_directory().to_owned();
    drop(first);
    assert!(
        !directory.exists(),
        "private socket directory survived teardown"
    );
    assert!(
        second
            .input(
                peer.target,
                Input::Key {
                    code: 30,
                    pressed: true
                }
            )
            .unwrap()
    );
    let survivor = next(&mut second);
    assert_eq!(color(&survivor), [11, 21, 30, 255]);
}

#[test]
fn invalid_extents_fail_before_opening_a_display() {
    for (width, height) in [(0, 1), (1, 0), (2049, 1), (u32::MAX, u32::MAX)] {
        assert!(WaylandHost::new(width, height).is_err());
    }
}

#[test]
fn configure_ack_with_unchanged_buffer_requires_new_presentation_and_target() {
    let mut host = WaylandHost::new(64, 48).unwrap();
    let _client = Client(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "wayland_child_fixture",
                "--nocapture",
            ])
            .env("VIVID_TEST_WAYLAND_SOCKET", host.socket_path())
            .env("VIVID_TEST_WAYLAND_KEEP_BUFFER", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let initial = next(&mut host);
    host.set_active(true);
    present(&mut host, &initial);
    let original = next(&mut host);
    present(&mut host, &original);
    host.resize(100, 80).unwrap();
    assert!(host.input_target().is_none());
    let fresh = next(&mut host);
    assert_eq!(fresh.rgba, original.rgba);
    assert_eq!(
        fresh.logical_size,
        [64, 48],
        "the client may ignore an advisory size"
    );
    assert_ne!(fresh.target, original.target);
    assert!(host.input_target().is_none());
    assert!(
        !host
            .input(
                original.target,
                Input::Key {
                    code: 30,
                    pressed: true
                }
            )
            .unwrap()
    );
    present(&mut host, &fresh);
    assert_eq!(host.input_target().unwrap().0, fresh.target);
}

#[test]
fn resize_releases_presented_old_frame_callbacks_without_rearming_old_input() {
    let mut host = WaylandHost::new(64, 48).unwrap();
    let _client = Client(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "wayland_child_fixture",
                "--nocapture",
            ])
            .env("VIVID_TEST_WAYLAND_SOCKET", host.socket_path())
            .env("VIVID_TEST_WAYLAND_WAIT_FRAME", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let original = next(&mut host);
    host.set_active(true);
    host.resize(100, 80).unwrap();
    assert!(
        !host.presented(original.id, original.target),
        "old presentation cannot rearm geometry"
    );
    assert!(
        !host
            .input(
                original.target,
                Input::Key {
                    code: 30,
                    pressed: true
                }
            )
            .unwrap()
    );
    // This client acknowledges configure but waits for the old frame callback
    // before drawing. Dropping that callback deadlocks ordinary resize.
    let resized = next(&mut host);
    assert_eq!(resized.logical_size, [100, 80]);
    assert_ne!(original.target, resized.target);
    assert_eq!(color(&resized)[0], 11, "old callback was not delivered");
    assert!(host.input_target().is_none());
    present(&mut host, &resized);
    assert!(host.input_target().is_some());
}

#[test]
fn oversized_native_client_is_disconnected_without_retiring_its_peer() {
    let mut first = WaylandHost::new(64, 48).unwrap();
    let mut second = WaylandHost::new(64, 48).unwrap();
    let mut bad = Client(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "wayland_child_fixture",
                "--nocapture",
            ])
            .env("VIVID_TEST_WAYLAND_SOCKET", first.socket_path())
            .env("VIVID_TEST_WAYLAND_OVERSIZE", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let _peer = client(&second);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(first.dispatch().unwrap().is_none());
        if bad.0.try_wait().unwrap().is_some() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "oversized client stayed connected"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    let frame = next(&mut second);
    second.set_active(true);
    present(&mut second, &frame);
    assert!(
        second
            .input(
                frame.target,
                Input::Key {
                    code: 30,
                    pressed: true
                }
            )
            .unwrap()
    );
}

#[test]
#[ignore = "child process fixture invoked by the native display regression"]
fn wayland_child_fixture() {
    let socket = std::env::var_os("VIVID_TEST_WAYLAND_SOCKET").expect("child socket");
    let connection = Connection::from_socket(UnixStream::connect(socket).unwrap()).unwrap();
    let mut queue = connection.new_event_queue();
    let qh = queue.handle();
    connection.display().get_registry(&qh, ());
    let mut state = Fixture {
        surface: None,
        shm: None,
        shell: None,
        xdg: None,
        size: [64, 48],
        color: [10, 20, 30, 255],
        configured_once: false,
        pointer: (-1., -1.),
    };
    loop {
        if queue.blocking_dispatch(&mut state).is_err() {
            break;
        }
    }
}

struct Fixture {
    surface: Option<wl_surface::WlSurface>,
    shm: Option<wl_shm::WlShm>,
    shell: Option<xdg_wm_base::XdgWmBase>,
    xdg: Option<xdg_surface::XdgSurface>,
    size: [u32; 2],
    color: [u8; 4],
    configured_once: bool,
    pointer: (f64, f64),
}
impl Fixture {
    fn setup(&mut self, qh: &QueueHandle<Self>) {
        if self.xdg.is_none()
            && let (Some(surface), Some(shell)) = (&self.surface, &self.shell)
        {
            let xdg = shell.get_xdg_surface(surface, qh, ());
            xdg.get_toplevel(qh, ());
            self.xdg = Some(xdg);
            surface.commit();
        }
    }
    fn draw(&mut self, qh: &QueueHandle<Self>, callback: bool) {
        let (width, height) = if std::env::var_os("VIVID_TEST_WAYLAND_OVERSIZE").is_some() {
            (4096, 1)
        } else {
            (self.size[0], self.size[1])
        };
        let bytes = width as usize * height as usize * 4;
        let mut file = tempfile::tempfile().unwrap();
        let pixel = [self.color[2], self.color[1], self.color[0], self.color[3]];
        let mut pixels = pixel.repeat(bytes / 4);
        let protocol_id = self
            .surface
            .as_ref()
            .unwrap()
            .id()
            .protocol_id()
            .to_le_bytes();
        // Distinguish full server identity from a client's reused object IDs.
        pixels[4..8].copy_from_slice(&[protocol_id[2], protocol_id[1], protocol_id[0], 255]);
        file.write_all(&pixels).unwrap();
        let pool = self
            .shm
            .as_ref()
            .unwrap()
            .create_pool(file.as_fd(), bytes as i32, qh, ());
        let buffer = pool.create_buffer(
            0,
            width as i32,
            height as i32,
            width as i32 * 4,
            wl_shm::Format::Argb8888,
            qh,
            (),
        );
        pool.destroy();
        let surface = self.surface.as_ref().unwrap();
        surface.attach(Some(&buffer), 0, 0);
        surface.damage(0, 0, width as i32, height as i32);
        if callback {
            surface.frame(qh, ());
        }
        surface.commit();
    }
}
impl Dispatch<wl_registry::WlRegistry, ()> for Fixture {
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
                "wl_compositor" => {
                    let compositor = registry.bind::<wl_compositor::WlCompositor, _, _>(
                        name,
                        version.min(4),
                        qh,
                        (),
                    );
                    state.surface = Some(compositor.create_surface(qh, ()));
                }
                "wl_shm" => {
                    state.shm = Some(registry.bind(name, 1, qh, ()));
                }
                "xdg_wm_base" => {
                    state.shell = Some(registry.bind(name, 1, qh, ()));
                }
                "wl_seat" => {
                    registry.bind::<wl_seat::WlSeat, _, _>(name, version.min(5), qh, ());
                }
                _ => (),
            }
            state.setup(qh);
        }
    }
}
impl Dispatch<xdg_wm_base::XdgWmBase, ()> for Fixture {
    fn event(
        _: &mut Self,
        shell: &xdg_wm_base::XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            shell.pong(serial);
        }
    }
}
impl Dispatch<xdg_surface::XdgSurface, ()> for Fixture {
    fn event(
        state: &mut Self,
        surface: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
            if state.configured_once && std::env::var_os("VIVID_TEST_WAYLAND_KEEP_BUFFER").is_some()
            {
                state.surface.as_ref().unwrap().commit();
                return;
            }
            if !state.configured_once || std::env::var_os("VIVID_TEST_WAYLAND_WAIT_FRAME").is_none()
            {
                state.draw(qh, true);
            }
            state.configured_once = true;
        }
    }
}
impl Dispatch<xdg_toplevel::XdgToplevel, ()> for Fixture {
    fn event(
        state: &mut Self,
        _: &xdg_toplevel::XdgToplevel,
        event: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_toplevel::Event::Configure { width, height, .. } = event
            && width > 0
            && height > 0
        {
            state.size = [width as u32, height as u32];
        }
    }
}
impl Dispatch<wl_seat::WlSeat, ()> for Fixture {
    fn event(
        _: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
        {
            if capabilities.contains(wl_seat::Capability::Keyboard) {
                seat.get_keyboard(qh, ());
            }
            if capabilities.contains(wl_seat::Capability::Pointer) {
                seat.get_pointer(qh, ());
            }
        }
    }
}
impl Dispatch<wl_keyboard::WlKeyboard, ()> for Fixture {
    fn event(
        state: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::Key {
            key: 31,
            state: WEnum::Value(wl_keyboard::KeyState::Pressed),
            ..
        } = event
        {
            let surface = state.surface.as_ref().unwrap();
            surface.attach(None, 0, 0);
            surface.commit();
        }
        if let wl_keyboard::Event::Key {
            key: 30,
            state: WEnum::Value(key_state),
            ..
        } = event
        {
            match key_state {
                wl_keyboard::KeyState::Pressed => state.color[1] += 1,
                wl_keyboard::KeyState::Released => state.color[2] += 1,
                _ => (),
            }
            state.draw(qh, false);
        }
    }
}
impl Dispatch<wl_pointer::WlPointer, ()> for Fixture {
    fn event(
        state: &mut Self,
        _: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter {
                surface_x,
                surface_y,
                ..
            }
            | wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => state.pointer = (surface_x, surface_y),
            wl_pointer::Event::Leave { .. } => state.pointer = (-1., -1.),
            _ => (),
        }
        if let wl_pointer::Event::Button {
            state: WEnum::Value(button),
            ..
        } = event
        {
            if button == wl_pointer::ButtonState::Pressed {
                state.color[0] = 200;
            } else if state.pointer.0 >= 0.
                && state.pointer.1 >= 0.
                && state.pointer.0 < f64::from(state.size[0])
                && state.pointer.1 < f64::from(state.size[1])
            {
                state.color[0] = 201;
            }
            state.draw(qh, false);
        }
    }
}
impl Dispatch<wl_buffer::WlBuffer, ()> for Fixture {
    fn event(
        _: &mut Self,
        buffer: &wl_buffer::WlBuffer,
        event: wl_buffer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event {
            buffer.destroy();
        }
    }
}
impl Dispatch<wl_callback::WlCallback, ()> for Fixture {
    fn event(
        state: &mut Self,
        _: &wl_callback::WlCallback,
        event: wl_callback::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            state.color[0] += 1;
            state.draw(
                qh,
                std::env::var_os("VIVID_TEST_WAYLAND_WAIT_FRAME").is_some(),
            );
        }
    }
}
delegate_noop!(Fixture: ignore wl_compositor::WlCompositor);
delegate_noop!(Fixture: ignore wl_surface::WlSurface);
delegate_noop!(Fixture: ignore wl_shm::WlShm);
delegate_noop!(Fixture: ignore wl_shm_pool::WlShmPool);

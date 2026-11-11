//! Native state framing and exact channel identities, independent of any GPU.

// Rust guideline compliant 2026-10-07
use super::*;
use crate::{
    gui as gpui,
    gui_host::{AppConfig, AppDescriptor, AppFactory, AppHost, HostedApp, Lifecycle},
};
use gpui::{App, AppContext as _, Context, Entity, IntoElement, Render, TestAppContext, Window};

#[derive(Debug)]
struct View;
impl Render for View {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui::div()
    }
}
#[derive(Clone, Debug)]
struct Proxy(Host);
#[derive(Debug)]
struct Instance(Entity<View>, Proxy);
impl AppFactory for Proxy {
    fn descriptor(&self) -> AppDescriptor {
        AppDescriptor::new("process-state", "Process state")
    }
    fn create(
        &self,
        _: &AppConfig,
        _: &mut Window,
        cx: &mut App,
    ) -> io::Result<Box<dyn HostedApp>> {
        Ok(Box::new(Instance(cx.new(|_| View), self.clone())))
    }
}
impl HostedApp for Instance {
    fn view(&self) -> gpui::AnyView {
        self.0.clone().into()
    }
    fn lifecycle(&mut self, _: Lifecycle, _: &mut Window, _: &mut App) {}
    fn capture_state(&mut self, response: StateResponder, _: &mut App) {
        self.1.0.capture(response);
    }
}
fn endpoint(directory: &tempfile::TempDir, name: &str) -> PathBuf {
    #[cfg(unix)]
    {
        directory.path().join(name)
    }
    #[cfg(windows)]
    {
        PathBuf::from(format!(
            r"\\.\pipe\vivid-state-{}-{name}",
            directory.path().file_name().unwrap().to_string_lossy()
        ))
    }
}
fn directory() -> tempfile::TempDir {
    let directory = tempfile::Builder::new().prefix("vvst-").tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    directory
}

#[test]
fn captured_state_roundtrips_its_validated_envelope() {
    let bytes = serde_json::to_vec(&Envelope {
        version: 1,
        message: Message::Captured {
            owner: [1; 16],
            id: 2,
            state: Some(AppState::new(serde_json::json!({"count":9})).unwrap()),
        },
    })
    .unwrap();
    let decoded = serde_json::from_slice::<Envelope>(&bytes);
    assert!(decoded.is_ok(), "{:?}", decoded.err());
}

#[test]
#[cfg(unix)]
fn final_reply_remains_readable_after_peer_eof() {
    let (mut writer, mut reader) = LocalStream::pair().unwrap();
    write_message(
        &mut writer,
        Message::Captured {
            owner: [1; 16],
            id: 1,
            state: Some(AppState::new(serde_json::json!({"count":9})).unwrap()),
        },
        Instant::now() + BUDGET,
    )
    .unwrap();
    drop(writer);
    assert!(matches!(
        read_message(&mut reader, Instant::now() + BUDGET).unwrap(),
        Message::Captured { id: 1, .. }
    ));
}
fn connect(host: &Host) -> LocalStream {
    let mut stream = LocalStream::connect(&host.endpoint).unwrap();
    write_message(
        &mut stream,
        Message::Hello { owner: host.owner },
        Instant::now() + BUDGET,
    )
    .unwrap();
    assert!(
        matches!(read_message(&mut stream, Instant::now() + BUDGET).unwrap(), Message::Initial { owner, .. } if owner == host.owner)
    );
    write_message(
        &mut stream,
        Message::Ready { owner: host.owner },
        Instant::now() + BUDGET,
    )
    .unwrap();
    stream
}

#[test]
#[cfg(unix)]
fn oversized_and_slow_fragmented_frames_fail_with_bounded_work() {
    let (mut writer, mut reader) = LocalStream::pair().unwrap();
    writer
        .write_all(&u32::try_from(MAX_FRAME + 1).unwrap().to_be_bytes())
        .unwrap();
    assert_eq!(
        read_message(&mut reader, Instant::now() + BUDGET)
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::InvalidData
    );
    let (mut writer, mut reader) = LocalStream::pair().unwrap();
    let delayed = thread::spawn(move || {
        writer.write_all(&[0]).unwrap();
        thread::sleep(Duration::from_millis(80));
        let _ = writer.write_all(&[0, 0, 2, b'{', b'}']);
    });
    let began = Instant::now();
    let error = read_message(&mut reader, began + Duration::from_millis(20))
        .err()
        .unwrap();
    assert!(matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    ));
    assert!(began.elapsed() < Duration::from_secs(1));
    delayed.join().unwrap();
}

#[test]
fn capture_ids_and_channel_owners_do_not_cross_processes() {
    let directory = directory();
    let (first, first_service) = Host::bind(
        &endpoint(&directory, "one"),
        Some(AppState::new(serde_json::json!({"count":7})).unwrap()),
    )
    .unwrap();
    let (peer, peer_service) = Host::bind(&endpoint(&directory, "two"), None).unwrap();
    assert_ne!(first.owner, peer.owner);
    assert!(!format!("{first:?}").contains("count"));
    let mut a = connect(&first);
    let mut b = connect(&peer);
    let wrong_owner = peer.owner;
    let child_a = thread::spawn(move || {
        let Message::Capture { id, .. } = read_message(&mut a, Instant::now() + BUDGET).unwrap()
        else {
            panic!("capture absent");
        };
        write_message(
            &mut a,
            Message::Captured {
                owner: wrong_owner,
                id,
                state: Some(AppState::new(serde_json::json!({"private-marker":true})).unwrap()),
            },
            Instant::now() + BUDGET,
        )
        .unwrap();
    });
    let owner = peer.owner;
    let child_b = thread::spawn(move || {
        for serial in 1..=2 {
            assert!(
                matches!(read_message(&mut b, Instant::now()+BUDGET).unwrap(), Message::Capture { owner: got, id } if got==owner && id==serial)
            );
            let message = if serial == 1 {
                Message::Failed { owner, id: serial }
            } else {
                Message::Captured {
                    owner,
                    id: serial,
                    state: Some(AppState::new(serde_json::json!({"count":9})).unwrap()),
                }
            };
            write_message(&mut b, message, Instant::now() + BUDGET).unwrap();
        }
    });
    let mut cx = TestAppContext::single();
    let mut host = AppHost::new(2).unwrap();
    let (mut one, mut two) = (None, None);
    cx.add_window(|window, cx| {
        let a = host
            .mount(&Proxy(first), &AppConfig::default(), window, cx)
            .unwrap();
        let b = host
            .mount(&Proxy(peer), &AppConfig::default(), window, cx)
            .unwrap();
        one = Some(host.capture_state(a, cx).unwrap());
        two = Some((b, host.capture_state(b, cx).unwrap()));
        View
    });
    let error = pollster::block_on(one.unwrap()).unwrap_err();
    assert_eq!(error, Error::Unavailable);
    assert!(!error.to_string().contains("private-marker"));
    let (b, first_reply) = two.unwrap();
    assert_eq!(
        pollster::block_on(first_reply).unwrap_err(),
        Error::Unavailable
    );
    // A valid semantic failure consumes its ID and keeps the channel available.
    let second_reply = cx.update(|cx| host.capture_state(b, cx).unwrap());
    let state = pollster::block_on(second_reply).unwrap().unwrap();
    assert_eq!(state.decode::<serde_json::Value>().unwrap()["count"], 9);
    child_a.join().unwrap();
    child_b.join().unwrap();
    drop(first_service);
    drop(peer_service);
}

#[test]
fn service_shutdown_interrupts_unfinished_handshakes() {
    let directory = directory();
    let (host, service) = Host::bind(&endpoint(&directory, "cancel"), None).unwrap();
    let mut stream = LocalStream::connect(&host.endpoint).unwrap();
    stream.write_all(&[0]).unwrap();
    let start = Instant::now();
    drop(service);
    assert!(start.elapsed() < Duration::from_secs(1));
    assert!(host.control.stopped.load(Ordering::Acquire));
}

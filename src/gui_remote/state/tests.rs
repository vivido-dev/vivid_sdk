//! Native state framing and exact channel identities, independent of any GPU.

// Rust guideline compliant 2026-10-07
use super::*;
use crate::{
    gui as gpui,
    gui_host::{AppConfig, AppDescriptor, AppFactory, AppHost, HostedApp, Lifecycle},
};
use gpui::{App, AppContext as _, Context, Entity, IntoElement, Render, TestAppContext, Window};
use std::io::Write as _;

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
    fn process_automation_methods<'a>(&'a self, _: &'a App) -> Option<&'a [Method<String>]> {
        Some(self.1.0.methods())
    }
    fn automate(&mut self, request: Request, response: Responder, _: &mut App) {
        self.1.0.call(request, response);
    }
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
    connect_methods(host, Vec::new())
}
fn connect_methods(host: &Host, methods: Vec<Method<String>>) -> LocalStream {
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
        Message::Ready {
            owner: host.owner,
            methods,
        },
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

#[test]
fn cancelled_offers_never_receive_a_grant_and_the_next_call_still_completes() {
    let directory = directory();
    let (process, _service) = Host::bind(&endpoint(&directory, "methods"), None).unwrap();
    let owner = process.owner;
    let mut socket = connect_methods(
        &process,
        vec![Method {
            name: "change".into(),
            access: automation::Access::Command,
            description: "Update model".into(),
        }],
    );
    let end = Instant::now() + BUDGET;
    while !process.is_ready() {
        assert!(Instant::now() < end);
        thread::yield_now();
    }
    let mut cx = TestAppContext::single();
    let mut host = AppHost::new(1).unwrap();
    let mut pane = None;
    cx.add_window(|window, cx| {
        let id = host
            .mount(&Proxy(process), &AppConfig::default(), window, cx)
            .unwrap();
        host.activate(id, window, cx);
        pane = Some(id);
        View
    });
    let pane = pane.unwrap();
    for (id, cancelled) in [(1, true), (2, false)] {
        let reply = cx
            .update(|cx| {
                host.automate(
                    pane,
                    Request::new("change".into(), Data::default()).unwrap(),
                    cx,
                )
            })
            .unwrap();
        let end = Instant::now() + BUDGET;
        assert!(
            matches!(read_message(&mut socket,end).unwrap(), Message::Offer {owner: got, id: seq, ..} if got == owner && seq == id)
        );
        let mut reply = Some(reply);
        if cancelled {
            drop(reply.take());
        }
        write_message(&mut socket, Message::Claim { owner, id }, end).unwrap();
        assert!(
            matches!(read_message(&mut socket,end).unwrap(), Message::Decision {owner: got,id: seq,remaining_us} if got == owner && seq == id && (remaining_us > 0) != cancelled)
        );
        if let Some(reply) = reply {
            write_message(
                &mut socket,
                Message::Result {
                    owner,
                    id,
                    result: Ok(Data::new(serde_json::json!({"count":1})).unwrap()),
                },
                end,
            )
            .unwrap();
            assert_eq!(
                pollster::block_on(reply)
                    .unwrap()
                    .decode::<serde_json::Value>()
                    .unwrap()["count"],
                1
            );
        }
    }
}

#[test]
fn invalid_method_registrations_do_not_become_ready() {
    let base = serde_json::json!({"name":"inspect","access":"query","description":"Read model"});
    for methods in [
        vec![base.clone(); 33],
        vec![base.clone(), base.clone()],
        vec![serde_json::json!({"name":"private/method","access":"command","description":""})],
        vec![serde_json::json!({"name":"read","access":"query","description":"x".repeat(257)})],
    ] {
        let frame = serde_json::json!({"version":1,"message":{"method":"ready","owner":vec![0u8;16],"methods":methods}});
        assert!(serde_json::from_value::<Envelope>(frame).is_err());
    }
}

#[cfg(unix)]
#[test]
fn expired_grant_cannot_run_at_a_later_model_turn() {
    let (mut host, stream) = LocalStream::pair().unwrap();
    let owner = [8u8; 16];
    let child = Connection {
        owner,
        stream,
        initial: None,
        methods_enabled: true,
    };
    let (_service, model) = child
        .start_io(&[Method {
            name: "increment",
            access: automation::Access::Command,
            description: "Increment",
        }])
        .unwrap();
    let end = Instant::now() + BUDGET;
    assert!(matches!(
        read_message(&mut host, end).unwrap(),
        Message::Ready { .. }
    ));
    let mut count = 0;
    for (id, lifetime) in [(1, 5_000), (2, 1_000_000)] {
        write_message(
            &mut host,
            Message::Offer {
                owner,
                id,
                name: "increment".into(),
                data: Data::default(),
            },
            end,
        )
        .unwrap();
        model.recv_blocking().unwrap().service(
            &mut (),
            |_| panic!("unexpected capture"),
            |_, _| panic!("call before grant"),
        );
        assert!(
            matches!(read_message(&mut host,end).unwrap(),Message::Claim{id: claim,..} if claim==id)
        );
        write_message(
            &mut host,
            Message::Decision {
                owner,
                id,
                remaining_us: lifetime,
            },
            end,
        )
        .unwrap();
        let job = model.recv_blocking().unwrap();
        if id == 1 {
            thread::sleep(Duration::from_millis(25));
        }
        job.service(
            &mut (),
            |_| panic!("unexpected capture"),
            |_, _| {
                count += 1;
                Ok(Data::default())
            },
        );
        let Message::Result {
            id: finished,
            result,
            ..
        } = read_message(&mut host, end).unwrap()
        else {
            panic!("missing result")
        };
        assert_eq!(finished, id);
        if id == 1 {
            assert_eq!(result.unwrap_err(), Error::Expired);
            assert_eq!(count, 0);
        } else {
            assert!(result.is_ok());
            assert_eq!(count, 1);
        }
    }
}

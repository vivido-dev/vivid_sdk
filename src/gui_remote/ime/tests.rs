//! Private native composition framing, revocation and owner isolation.

// Rust guideline compliant 2026-10-07
use super::*;
use vivid_protocol::revision::{GrantGeneration, InputEpoch, SurfaceGeneration};

fn tag() -> crate::InputTuple {
    crate::InputTuple {
        producer_epoch: InputEpoch::new(1),
        grant_generation: GrantGeneration::new(1),
        context_id: 1,
        surface_id: 7,
        surface_generation: SurfaceGeneration::new(1),
    }
}
fn connect(host: &Host) -> LocalStream {
    let mut stream = LocalStream::connect(&host.endpoint).unwrap();
    let end = Instant::now() + BUDGET;
    local::send(
        &mut stream,
        &Message::Hello {
            version: 1,
            owner: *host.owner,
        },
        1024,
        end,
    )
    .unwrap();
    assert!(matches!(
        local::receive(&mut stream, 1024, end).unwrap(),
        Message::Ready { version: 1 }
    ));
    stream
}
fn reply(stream: &mut LocalStream, context: Option<Native>) -> Option<Operation> {
    let end = Instant::now() + BUDGET;
    let Message::Tick {
        id,
        generation,
        binding,
        edit,
    } = local::receive(stream, MAX_FRAME, end).unwrap()
    else {
        panic!("not a tick");
    };
    local::send(
        stream,
        &Message::Snapshot {
            id,
            generation,
            binding,
            editor: context,
        },
        MAX_FRAME,
        end,
    )
    .unwrap();
    edit
}
fn wait(mut predicate: impl FnMut() -> bool) {
    let end = Instant::now() + BUDGET;
    while !predicate() {
        assert!(Instant::now() < end, "composition state did not settle");
        thread::sleep(POLL);
    }
}
fn directory() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt as _;
    tempfile::Builder::new()
        .prefix("vvime-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap()
}
#[test]
fn owner_private_targets_reject_foreign_focus_round_trips_and_stale_editors() {
    let dir = directory();
    let (a, sa) = Host::bind(&dir.path().join("a")).unwrap();
    let (b, sb) = Host::bind(&dir.path().join("b")).unwrap();
    a.set_binding(Some(tag()));
    b.set_binding(Some(tag()));
    let mut one = connect(&a);
    let mut two = connect(&b);
    let context = Native {
        editor: 1,
        reset: 1,
        caret: Some([0.1, 0.2, 0.01, 0.1]),
    };
    reply(&mut one, Some(context));
    reply(&mut two, Some(context));
    wait(|| a.editor().is_some() && b.editor().is_some());
    let old = a.editor().unwrap().target;
    let peer = b.editor().unwrap().target;
    let edit = Edit {
        commit: Some("é😀漢".into()),
        ..Default::default()
    };
    assert!(!b.dispatch(&old, &edit).unwrap());
    assert!(a.dispatch(&old, &edit).unwrap());
    a.set_binding(None);
    a.set_binding(Some(tag()));
    assert!(!a.dispatch(&old, &edit).unwrap());
    // The host may already have started one tick. A superseded reply cannot
    // install context in the new binding generation.
    for _ in 0..3 {
        reply(&mut one, Some(context));
        if a.editor().is_some() {
            break;
        }
    }
    wait(|| a.editor().is_some());
    assert!(!old.same_editor(&a.editor().unwrap().target));
    assert!(b.dispatch(&peer, &edit).unwrap());
    let mut got = None;
    for _ in 0..3 {
        let op = reply(&mut two, Some(context));
        if op.is_some() {
            got = op;
            break;
        }
    }
    assert_eq!(got.unwrap().edit.commit.as_deref(), Some("é😀漢"));
    let current = a.editor().unwrap().target;
    reply(
        &mut one,
        Some(Native {
            editor: 2,
            ..context
        }),
    );
    wait(|| a.editor().is_some_and(|s| !s.target.same_editor(&current)));
    assert!(!a.dispatch(&current, &edit).unwrap());
    a.close();
    assert!(a.editor().is_none());
    assert!(b.dispatch(&peer, &edit).unwrap());
    assert!(!format!("{edit:?}").contains("é😀漢"));
    drop(sa);
    drop(sb);
}
#[test]
fn native_admission_bounds_text_ranges_and_queue_capacity_before_copying() {
    let dir = directory();
    let (host, service) = Host::bind(&dir.path().join("host")).unwrap();
    host.set_binding(Some(tag()));
    let mut stream = connect(&host);
    let context = Native {
        editor: 1,
        reset: 1,
        caret: None,
    };
    reply(&mut stream, Some(context));
    wait(|| host.editor().is_some());
    let target = host.editor().unwrap().target;
    assert!(
        host.dispatch(
            &target,
            &Edit {
                preedit: "😀".into(),
                selection: Some((1, 4)),
                ..Default::default()
            }
        )
        .is_err()
    );
    assert!(
        host.dispatch(
            &target,
            &Edit {
                commit: Some("x".repeat(MAX_TEXT_BYTES + 1)),
                ..Default::default()
            }
        )
        .is_err()
    );
    // Freeze host transport under this mutex so native queue admission is deterministic.
    {
        let mut bound = host.state.bound.lock().unwrap();
        for _ in 0..INPUT_CAPACITY {
            bound.jobs.push_back(Job {
                operation: Operation {
                    editor: 1,
                    reset: 1,
                    edit: Edit {
                        commit: Some("x".into()),
                        ..Default::default()
                    },
                },
                end: Instant::now() + BUDGET,
            });
        }
    }
    // One operation can be in flight independently of the bounded native queue.
    for _ in 0..2 {
        let _ = host.dispatch(
            &target,
            &Edit {
                commit: Some("x".into()),
                ..Default::default()
            },
        );
    }
    assert_eq!(
        host.dispatch(
            &target,
            &Edit {
                commit: Some("x".into()),
                ..Default::default()
            }
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::WouldBlock
    );
    host.set_binding(None);
    assert!(host.editor().is_none());
    assert!(
        !host
            .dispatch(
                &target,
                &Edit {
                    commit: Some("x".into()),
                    ..Default::default()
                }
            )
            .unwrap()
    );
    drop(service);
}
#[test]
fn malformed_native_geometry_or_owner_authentication_closes_only_its_channel() {
    let dir = directory();
    let (host, service) = Host::bind(&dir.path().join("host")).unwrap();
    let mut stream = connect(&host);
    reply(
        &mut stream,
        Some(Native {
            editor: 1,
            reset: 1,
            caret: Some([0., 0., 2., 1.]),
        }),
    );
    wait(|| host.state.stopped.load(Ordering::Acquire));
    assert!(host.editor().is_none());
    drop(service);
    let (host, service) = Host::bind(&dir.path().join("wrong")).unwrap();
    let mut stream = LocalStream::connect(&host.endpoint).unwrap();
    local::send(
        &mut stream,
        &Message::Hello {
            version: 1,
            owner: [0; 16],
        },
        1024,
        Instant::now() + BUDGET,
    )
    .unwrap();
    assert!(local::receive::<Message>(&mut stream, 1024, Instant::now() + BUDGET).is_err());
    wait(|| host.state.stopped.load(Ordering::Acquire));
    drop(service);
}

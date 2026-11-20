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
            version: VERSION,
            owner: *host.owner,
        },
        1024,
        end,
    )
    .unwrap();
    assert!(matches!(
        local::receive(&mut stream, 1024, end).unwrap(),
        Message::Ready { version: VERSION }
    ));
    stream
}
fn reply(stream: &mut LocalStream, context: Option<Native>) -> Option<Operation> {
    exchange(stream, context, INPUT_CAPACITY).edit
}
struct Tick {
    binding: Option<Binding>,
    layout: Option<Mapping>,
    repeat: Option<KeyRepeat>,
    edit: Option<Operation>,
    keyboard: Vec<KeyboardContext>,
}
fn exchange(stream: &mut LocalStream, context: Option<Native>, keyboard_credit: usize) -> Tick {
    let end = Instant::now() + BUDGET;
    let Message::Tick {
        id,
        generation,
        binding,
        edit,
        layout,
        repeat,
        keyboard,
        ..
    } = local::receive(stream, MAX_TICK, end).unwrap()
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
            tree: TreePublication::Empty,
            keyboard_credit,
        },
        MAX_FRAME,
        end,
    )
    .unwrap();
    Tick {
        repeat,
        binding,
        layout,
        edit,
        keyboard,
    }
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
fn layout_updates_are_sent_once_per_grant_and_remain_private_to_each_child() {
    fn map(letter: &str) -> Arc<CharacterLayout> {
        Arc::new(serde_json::from_value(serde_json::json!({
            "id": letter, "keys": [{"usage": 4, "layers": [letter, letter, letter, letter, letter, letter]}]
        })).unwrap())
    }
    fn read_key(stream: &mut LocalStream, expected: crate::InputTuple) -> String {
        for _ in 0..3 {
            let tick = exchange(stream, None, INPUT_CAPACITY);
            if tick.binding == Some(binding(expected)) {
                let Some(Mapping::Host(Some(layout))) = tick.layout else {
                    panic!("missing grant layout")
                };
                return layout.keystroke(4, Default::default(), false).unwrap().key;
            }
        }
        panic!("replacement grant did not arrive");
    }
    let dir = directory();
    let (first, _first_service) = Host::bind(&dir.path().join("first")).unwrap();
    let (peer, _peer_service) = Host::bind(&dir.path().join("peer")).unwrap();
    first.set_binding_with_layout(Some(tag()), Some(map("a")));
    peer.set_binding_with_layout(Some(tag()), Some(map("b")));
    let mut one = connect(&first);
    let mut two = connect(&peer);
    assert_eq!(read_key(&mut one, tag()), "a");
    assert_eq!(read_key(&mut two, tag()), "b");
    first.set_binding_with_layout(Some(tag()), Some(map("c")));
    assert!(exchange(&mut one, None, INPUT_CAPACITY).layout.is_none());
    let next = crate::InputTuple {
        grant_generation: GrantGeneration::new(2),
        ..tag()
    };
    first.set_binding_with_layout(Some(next), Some(map("c")));
    assert_eq!(read_key(&mut one, next), "c");
    let tick = exchange(&mut two, None, INPUT_CAPACITY);
    assert_eq!(tick.binding, Some(binding(tag())));
    assert!(tick.layout.is_none());
}

#[test]
fn live_repeat_updates_stay_with_their_grant_and_owner() {
    let dir = directory();
    let (first, _a) = Host::bind(&dir.path().join("first")).unwrap();
    let (peer, _b) = Host::bind(&dir.path().join("peer")).unwrap();
    let disabled = KeyRepeat::new(0, 0).unwrap();
    let fast = KeyRepeat::new(110, 40).unwrap();
    first.set_binding_with_keyboard_policy(Some(tag()), None, false, disabled);
    peer.set_binding_with_keyboard_policy(Some(tag()), None, true, fast);
    let mut a = connect(&first);
    let mut b = connect(&peer);
    assert_eq!(exchange(&mut a, None, 0).repeat, Some(disabled));
    assert_eq!(exchange(&mut b, None, 0).repeat, Some(fast));
    first.set_binding_with_keyboard_policy(Some(tag()), None, false, fast);
    wait(|| {
        let tick = exchange(&mut a, None, 0);
        assert_eq!(tick.binding, Some(binding(tag())));
        assert!(tick.layout.is_none());
        tick.repeat == Some(fast)
    });
    first.set_binding(None);
    wait(|| {
        let tick = exchange(&mut a, None, 0);
        tick.binding.is_none() && tick.repeat.is_none()
    });
    assert_eq!(exchange(&mut b, None, 0).repeat, Some(fast));
}

#[test]
fn lock_queue_respects_credit_and_a_new_grant_retires_its_backlog() {
    let dir = directory();
    let (host, _service) = Host::bind(&dir.path().join("credit")).unwrap();
    host.set_binding_with_lock_state(Some(tag()), None, true);
    let mut stream = connect(&host);
    let first = exchange(&mut stream, None, 0);
    assert_eq!(first.keyboard.len(), 1);
    assert!(first.keyboard[0].key.is_none() && first.keyboard[0].caps_lock);
    for _ in 0..INPUT_CAPACITY {
        host.record_lock_state(tag(), Some((4, true)), false)
            .unwrap();
    }
    assert_eq!(
        host.record_lock_state(tag(), None, false)
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    assert!(exchange(&mut stream, None, 2).keyboard.is_empty());
    assert_eq!(exchange(&mut stream, None, 0).keyboard.len(), 2);
    let next = crate::InputTuple {
        grant_generation: GrantGeneration::new(2),
        ..tag()
    };
    host.set_binding_with_lock_state(Some(next), None, false);
    for _ in 0..3 {
        let tick = exchange(&mut stream, None, INPUT_CAPACITY);
        if tick.binding == Some(binding(next)) {
            assert_eq!(tick.keyboard.len(), 1);
            assert!(tick.keyboard[0].key.is_none() && !tick.keyboard[0].caps_lock);
            return;
        }
        assert!(tick.keyboard.is_empty());
    }
    panic!("new grant did not replace the stalled keyboard");
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
        document: None,
    };
    reply(&mut one, Some(context.clone()));
    reply(&mut two, Some(context.clone()));
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
        reply(&mut one, Some(context.clone()));
        if a.editor().is_some() {
            break;
        }
    }
    wait(|| a.editor().is_some());
    assert!(!old.same_editor(&a.editor().unwrap().target));
    assert!(b.dispatch(&peer, &edit).unwrap());
    let mut got = None;
    for _ in 0..3 {
        let op = reply(&mut two, Some(context.clone()));
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
        document: None,
    };
    reply(&mut stream, Some(context.clone()));
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
            document: None,
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
            version: VERSION,
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

#[test]
fn document_edits_require_exact_revisions_and_masked_fields_never_export_secrets() {
    let document = Document {
        revision: 2,
        text: "a😀éz".into(),
        cursor: 7,
        anchor: 5,
        sensitive: false,
    };
    let mut edit = Edit {
        revision: Some(2),
        replacement: Some((1, 7)),
        commit: Some("文".into()),
        ..Default::default()
    };
    assert!(document.valid());
    assert!(edit.matches(Some(&document)));
    edit.revision = Some(1);
    assert!(!edit.matches(Some(&document)));
    edit.replacement = None;
    assert!(
        edit.matches(Some(&document)),
        "relative typing survives own earlier commits"
    );
    edit.revision = Some(3);
    assert!(!edit.matches(Some(&document)));
    edit.revision = Some(2);
    edit.replacement = Some((2, 7));
    assert!(!edit.matches(Some(&document)), "split UTF-8 boundary");
    edit.replacement = Some((0, 5));
    assert!(!edit.matches(Some(&document)), "must cover full selection");
    let masked = Document {
        revision: 1,
        text: String::new(),
        cursor: 0,
        anchor: 0,
        sensitive: true,
    };
    assert!(masked.valid());
    edit.revision = Some(1);
    edit.replacement = Some((0, 0));
    assert!(!edit.matches(Some(&masked)));
    assert!(
        !Document {
            sensitive: true,
            ..document.clone()
        }
        .valid()
    );
    assert!(
        !Document {
            text: "x".repeat(MAX_TEXT_BYTES + 1),
            ..document.clone()
        }
        .valid()
    );
    assert!(!format!("{document:?}").contains("a😀éz"));
    assert!(!format!("{edit:?}").contains("文"));
}

#[test]
fn absent_snapshots_cannot_reset_document_revision_or_service_version() {
    let dir = directory();
    let (host, service) = Host::bind(&dir.path().join("revision")).unwrap();
    host.set_binding(Some(tag()));
    let mut stream = connect(&host);
    let mut native = Native {
        editor: 1,
        reset: 1,
        caret: None,
        document: Some(Document {
            revision: 2,
            text: "document".into(),
            cursor: 8,
            anchor: 8,
            sensitive: false,
        }),
    };
    reply(&mut stream, Some(native.clone()));
    wait(|| host.editor().is_some());
    reply(&mut stream, None);
    wait(|| host.editor().is_none());
    native.document.as_mut().unwrap().revision = 1;
    reply(&mut stream, Some(native));
    wait(|| host.state.stopped.load(Ordering::Acquire));
    drop(service);
    let (host, service) = Host::bind(&dir.path().join("version")).unwrap();
    let mut stream = LocalStream::connect(&host.endpoint).unwrap();
    local::send(
        &mut stream,
        &Message::Hello {
            version: VERSION - 1,
            owner: *host.owner,
        },
        1024,
        Instant::now() + BUDGET,
    )
    .unwrap();
    assert!(local::receive::<Message>(&mut stream, 1024, Instant::now() + BUDGET).is_err());
    wait(|| host.state.stopped.load(Ordering::Acquire));
    drop(service);
}

#[test]
fn child_accessibility_exports_only_bounded_tree_metadata() {
    use crate::gui::accesskit::{Node, NodeId, Role, Tree, TreeId, TreeUpdate};
    let mut root = Node::new(Role::Dialog);
    let mut private = Node::new(Role::PasswordInput);
    private.set_value("private field value");
    private.set_label("Password");
    private.set_children(vec![NodeId(3)]);
    let mut protected = Node::new(Role::Label);
    protected.set_label("password descendant");
    let mut hidden = Node::new(Role::Label);
    hidden.set_label("hidden label");
    hidden.set_hidden();
    root.set_children(vec![NodeId(2), NodeId(4)]);

    let mut update = TreeUpdate {
        focus: NodeId(2),
        nodes: vec![
            (NodeId(2), private),
            (NodeId(1), root),
            (NodeId(3), protected),
            (NodeId(4), hidden),
        ],
        tree: Some(Tree::new(NodeId(1))),
        tree_id: TreeId::ROOT,
    };
    let tree = AccessibilityTree::capture(&update, [100., 100.], true).unwrap();
    let json = serde_json::to_string(&tree).unwrap();
    assert!(json.contains("Password"));
    assert!(!json.contains("private field value"));
    assert!(!json.contains("password descendant"));
    assert!(!json.contains("hidden label"));
    let decoded: AccessibilityTree = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded, tree);
    let mut malformed = serde_json::to_value(&tree).unwrap();
    malformed["entries"][1]["parent"] = 1.into();
    assert!(serde_json::from_value::<AccessibilityTree>(malformed).is_err());
    let root_entry = serde_json::to_value(&tree).unwrap()["entries"][0].clone();
    for (count, chain, label_len) in [
        (128, false, 0),
        (129, false, 0),
        (26, true, 0),
        (10, false, 900),
    ] {
        let entries = (0..count)
            .map(|i| {
                let mut entry = root_entry.clone();
                entry["id"] = i.into();
                entry["parent"] = if i == 0 {
                    serde_json::Value::Null
                } else if chain {
                    (i - 1).into()
                } else {
                    0.into()
                };
                entry["label"] = "x".repeat(label_len).into();
                entry
            })
            .collect::<Vec<_>>();
        let parsed = serde_json::from_value::<AccessibilityTree>(
            serde_json::json!({"entries": entries, "focused": 0}),
        );
        assert_eq!(parsed.is_ok(), count == 128);
    }
    update.nodes[0].1.set_description("x".repeat(1025));
    assert!(AccessibilityTree::capture(&update, [100., 100.], true).is_none());
    update.nodes[0].1.clear_description();
    update.nodes[0].1.set_role(Role::Group);
    update.nodes[0].1.set_children(vec![NodeId(1)]);
    assert!(AccessibilityTree::capture(&update, [100., 100.], true).is_none());
}

#[test]
fn assistive_actions_require_the_original_owner_publication_and_live_grant() {
    use crate::gui::accesskit::{Action, Node, NodeId, Role, Tree, TreeId, TreeUpdate};
    fn publish(disabled: bool) -> Arc<AccessibilityTree> {
        let mut root = Node::new(Role::Group);
        if disabled {
            root.set_disabled();
        }
        root.set_children(vec![NodeId(2)]);
        let mut button = Node::new(Role::Button);
        button.add_action(Action::Click);
        Arc::new(
            AccessibilityTree::capture(
                &TreeUpdate {
                    nodes: vec![(NodeId(1), root), (NodeId(2), button)],
                    focus: NodeId(2),
                    tree: Some(Tree::new(NodeId(1))),
                    tree_id: TreeId::ROOT,
                },
                [100., 100.],
                true,
            )
            .unwrap(),
        )
    }
    fn tick(stream: &mut LocalStream, tree: TreePublication) -> Option<AssistRequest> {
        let end = Instant::now() + BUDGET;
        let Message::Tick {
            id,
            generation,
            binding,
            tree_action,
            ..
        } = local::receive(stream, MAX_TICK, end).unwrap()
        else {
            panic!("expected tick")
        };
        local::send(
            stream,
            &Message::Snapshot {
                id,
                generation,
                binding,
                editor: None,
                tree,
                keyboard_credit: INPUT_CAPACITY,
            },
            MAX_FRAME,
            end,
        )
        .unwrap();
        tree_action
    }
    let dir = directory();
    let (first, _a) = Host::bind(&dir.path().join("first")).unwrap();
    let (peer, _b) = Host::bind(&dir.path().join("peer")).unwrap();
    for host in [&first, &peer] {
        host.set_binding(Some(tag()));
        assert!(host.accessibility_tree(Some(tag()), true).is_none());
    }
    let mut a = connect(&first);
    let mut b = connect(&peer);
    for socket in [&mut a, &mut b] {
        assert!(tick(socket, TreePublication::Published(publish(false))).is_none());
    }
    let tree = |host: &Host| {
        wait(|| host.accessibility_tree(Some(tag()), true).is_some());
        host.accessibility_tree(Some(tag()), true).unwrap()
    };
    let old = tree(&first);
    let live = tree(&peer);
    assert!(!first.request_accessibility_action(tag(), &live, 2, TreeAction::Click));
    assert!(!peer.request_accessibility_action(tag(), &old, 2, TreeAction::Click));
    assert!(!first.request_accessibility_action(tag(), &old, 2, TreeAction::Focus));
    assert!(!first.request_accessibility_action(tag(), &old, 8, TreeAction::Click));
    // Publish an equal replacement while a callback still holds the previous tree.
    tick(&mut a, TreePublication::Published(publish(false)));
    wait(|| {
        first
            .accessibility_tree(Some(tag()), true)
            .is_some_and(|t| !Arc::ptr_eq(&t, &old))
    });
    assert!(!first.request_accessibility_action(tag(), &old, 2, TreeAction::Click));
    tick(&mut a, TreePublication::Published(publish(true)));
    wait(|| !tree(&first).supports(2, TreeAction::Click));
    assert!(!first.request_accessibility_action(tag(), &tree(&first), 2, TreeAction::Click));
    first.set_binding(None);
    assert!(!first.request_accessibility_action(tag(), &old, 2, TreeAction::Click));
    assert!(peer.request_accessibility_action(tag(), &live, 2, TreeAction::Click));
    // A tick can already be in flight when the presenter admits the action.
    let request = tick(&mut b, TreePublication::Retained)
        .or_else(|| tick(&mut b, TreePublication::Retained))
        .unwrap();
    assert_eq!(request.node, 2);
    assert!(matches!(request.action, TreeAction::Click));
    assert!(request.remaining().is_some());
    assert!(tick(&mut b, TreePublication::Retained).is_none());
}

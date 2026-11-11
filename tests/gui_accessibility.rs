//! Readable text and activation through the real GUI accessibility callback path.
#![cfg(feature = "gui-testing")]

// Rust guideline compliant 2026-10-07
use std::{cell::RefCell, io, rc::Rc, sync::Arc};
use vivid_sdk::{
    accessible_text::{Builder, Document, Position, Selection},
    gui::{
        self, AnyView, App, AppContext, Context, Entity, InteractiveElement, IntoElement,
        ParentElement, Render, Role, StatefulInteractiveElement, Styled, TestAppContext,
        VisualTestContext, Window, div, px,
    },
    gui_host::{
        ActivationRequest, AppConfig, AppDescriptor, AppFactory, AppHost, HostedApp, InstanceId,
        Lifecycle, accessibility,
    },
};

#[derive(Debug)]
struct Factory(Rc<RefCell<Vec<ActivationRequest>>>);
impl AppFactory for Factory {
    fn descriptor(&self) -> AppDescriptor {
        AppDescriptor::new("accessible", "Accessible")
    }
    fn create(
        &self,
        _: &AppConfig,
        _: &mut Window,
        cx: &mut App,
    ) -> io::Result<Box<dyn HostedApp>> {
        let mut builder =
            Builder::new(&format!("Visible terminal {}", self.0.borrow().len())).unwrap();
        builder
            .push_line(
                7,
                [0., 0., 1., 1.],
                [('🦇', 0., 0.5), ('\u{301}', 0.5, 0.), ('\n', 0.5, 0.)],
            )
            .unwrap();
        let position = Position {
            line: 7,
            character: 2,
        };
        let document = Arc::new(
            builder
                .finish(
                    Some(Selection {
                        anchor: position,
                        focus: position,
                    }),
                    true,
                )
                .unwrap(),
        );
        Ok(Box::new(Instance {
            view: cx.new(|_| View {
                document,
                activation: None,
            }),
            requests: self.0.clone(),
        }))
    }
}
#[derive(Debug)]
struct View {
    document: Arc<Document>,
    activation: Option<ActivationRequest>,
}
impl Render for View {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let document = self.document.clone();
        let activation = self.activation.clone();
        div()
            .id(("document", cx.entity_id().as_u64()))
            .role(Role::Terminal)
            .w(px(100.))
            .h(px(50.))
            .on_a11y_action(gui::AccessibleAction::Focus, move |_, _, _| {
                activation.as_ref().unwrap().request();
            })
            .a11y_synthetic_children(move |builder| {
                accessibility::populate(builder, &document);
                let expected = builder.synthetic_node_id(7u64);
                let selection = builder.parent_node().text_selection().unwrap();
                assert_eq!(selection.anchor.node, expected);
                assert_eq!(selection.focus.character_index, 2);
                let bounds = builder.parent_node().bounds().unwrap();
                // The test platform runs at 2x: normalized rows inherit physical bounds.
                assert_eq!(bounds.x1 - bounds.x0, 200.);
                assert_eq!(bounds.y1 - bounds.y0, 100.);
            })
    }
}
#[derive(Debug)]
struct Instance {
    view: Entity<View>,
    requests: Rc<RefCell<Vec<ActivationRequest>>>,
}
impl HostedApp for Instance {
    fn view(&self) -> AnyView {
        self.view.clone().into()
    }
    fn lifecycle(&mut self, _: Lifecycle, _: &mut Window, _: &mut App) {}
    fn set_activation_request(&mut self, request: ActivationRequest, cx: &mut App) {
        self.requests.borrow_mut().push(request.clone());
        self.view
            .update(cx, |view, _| view.activation = Some(request));
    }
}
struct Root {
    host: AppHost,
    ids: Vec<InstanceId>,
}
impl Render for Root {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().flex().children(self.ids.iter().filter_map(|id| {
            self.host
                .is_visible(*id)
                .filter(|v| *v)
                .and_then(|_| self.host.view(*id))
        }))
    }
}
fn draw(visual: &mut VisualTestContext) -> serde_json::Value {
    visual.update(|window, cx| {
        window.refresh();
        let arena = window.draw(cx);
        arena.clear(cx);
        window.simulate_next_frame(cx);
        window
            .debug_a11y_tree_json()
            .map(|s| serde_json::from_str(&s).unwrap())
            .unwrap_or_default()
    })
}
fn nodes(tree: &serde_json::Value, role: &str) -> Vec<u64> {
    tree["nodes"]
        .as_object()
        .unwrap()
        .values()
        .filter(|node| node["aria"]["role"] == role)
        .map(|node| node["accesskit_id"].as_str().unwrap().parse().unwrap())
        .collect()
}

#[gui::test]
fn readable_rows_and_focus_requests_follow_exact_host_lifetimes(cx: &mut TestAppContext) {
    let requests = Rc::new(RefCell::new(Vec::new()));
    let factory = Factory(requests.clone());
    let handle = cx.add_window(|window, cx| {
        let mut host = AppHost::new(2).unwrap();
        let ids = (0..2)
            .map(|_| {
                host.mount(&factory, &AppConfig::default(), window, cx)
                    .unwrap()
            })
            .collect();
        Root { host, ids }
    });
    let mut visual = VisualTestContext::from_window(handle.into(), cx);
    cx.run_until_parked();
    visual.update(|window, _| assert!(!window.is_a11y_active()));
    assert!(cx.simulate_accessibility_activation(handle.into(), true));
    cx.run_until_parked();
    let tree = draw(&mut visual);
    let rows = nodes(&tree, "TextRun");
    assert_eq!(rows.len(), 2);
    assert_ne!(
        rows[0], rows[1],
        "identical local row keys need distinct owners"
    );
    for node in tree["nodes"]
        .as_object()
        .unwrap()
        .values()
        .filter(|n| n["aria"]["role"] == "TextRun")
    {
        assert_eq!(node["aria"]["value"], "🦇\u{301}\n");
    }
    let target = tree["nodes"]
        .as_object()
        .unwrap()
        .values()
        .find(|node| {
            node["aria"]["role"] == "Terminal" && node["aria"]["label"] == "Visible terminal 1"
        })
        .unwrap()["accesskit_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(cx.simulate_accessibility_action(
        handle.into(),
        gui::AccessibleNodeId(target),
        gui::AccessibleAction::Focus
    ));
    cx.run_until_parked();
    handle
        .update(cx, |root, window, cx| {
            assert_eq!(
                root.host.active(),
                None,
                "adapter actions must await host policy"
            );
            let pending = root.host.take_activation_requests();
            assert_eq!(pending.len(), 1);
            assert_eq!(pending[0], root.ids[1]);
            assert!(root.host.focus(pending[0], window, cx));
            assert!(requests.borrow()[0].request());
            assert!(root.host.set_visible(root.ids[0], false, window, cx));
            assert!(!requests.borrow()[0].request());
            assert!(root.host.take_activation_requests().is_empty());
            cx.notify();
        })
        .unwrap();
    let after = nodes(&draw(&mut visual), "TextRun");
    assert_eq!(after.len(), 1);
    assert!(
        rows.contains(&after[0]),
        "surviving owner keeps its row identity"
    );
    handle
        .update(cx, |root, window, cx| {
            let old = requests.borrow()[0].clone();
            let replacement = root
                .host
                .replace(root.ids[0], &factory, &AppConfig::default(), window, cx)
                .unwrap();
            root.ids[0] = replacement;
            assert!(!old.request());
            let fresh = requests.borrow()[2].clone();
            assert!(!fresh.request(), "hidden replacement must stay hidden");
            assert!(root.host.set_visible(replacement, true, window, cx));
            assert!(fresh.request());
            assert_eq!(root.host.take_activation_requests(), vec![replacement]);
            let mut peer = AppHost::new(1).unwrap();
            let peer_id = peer
                .mount(&factory, &AppConfig::default(), window, cx)
                .unwrap();
            let peer_request = requests.borrow()[3].clone();
            assert!(root.host.close(replacement, window, cx));
            assert!(!fresh.request());
            assert!(peer_request.request());
            assert_eq!(peer.take_activation_requests(), vec![peer_id]);
            drop(peer);
            assert!(!peer_request.request());
            cx.notify();
        })
        .unwrap();
    assert!(cx.simulate_accessibility_activation(handle.into(), false));
    cx.run_until_parked();
    draw(&mut visual);
    visual.update(|window, _| assert!(!window.is_a11y_active()));
}

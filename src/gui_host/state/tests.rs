//! State capture retains complete owner identity through deferred completion.

// Rust guideline compliant 2026-10-07
use super::*;
use crate::{
    gui as gpui,
    gui_host::{AppDescriptor, AppFactory, AppHost, HostedApp, Lifecycle, automation::Error},
};
use gpui::{App, AppContext as _, Context, Entity, IntoElement, Render, TestAppContext, Window};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

#[derive(Debug)]
struct View;
impl Render for View {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui::div()
    }
}
#[derive(Debug, Clone)]
struct Factory(Rc<RefCell<Vec<StateResponder>>>);
#[derive(Debug)]
struct Instance(Entity<View>, Factory);
impl AppFactory for Factory {
    fn descriptor(&self) -> AppDescriptor {
        AppDescriptor::new("deferred-state", "Deferred state")
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
        self.1.0.borrow_mut().push(response);
    }
}

#[test]
fn deferred_state_survives_hide_but_not_owner_replacement_or_close() {
    let mut cx = TestAppContext::single();
    let first_factory = Factory(Rc::new(RefCell::new(Vec::new())));
    let peer_factory = Factory(Rc::new(RefCell::new(Vec::new())));
    let mut first = AppHost::new(1).unwrap();
    let mut peer = AppHost::new(1).unwrap();
    let mut ids = None;
    let window = cx.add_window(|window, cx| {
        let a = first
            .mount(&first_factory, &AppConfig::default(), window, cx)
            .unwrap();
        let b = peer
            .mount(&peer_factory, &AppConfig::default(), window, cx)
            .unwrap();
        assert_eq!(a.serial, b.serial);
        assert_ne!(a, b);
        ids = Some((a, b));
        View
    });
    let (a, b) = ids.unwrap();
    let (hidden, valid) = window
        .update(&mut cx, |_, window, cx| {
            assert_eq!(first.capture_state(b, cx).unwrap_err(), Error::Closed);
            let capture = first.capture_state(a, cx).unwrap();
            first.set_visible(a, false, window, cx);
            (capture, peer.capture_state(b, cx).unwrap())
        })
        .unwrap();
    first_factory
        .0
        .borrow_mut()
        .pop()
        .unwrap()
        .run(|| Ok(Some(AppState::new(serde_json::json!({"count":7})).unwrap())));
    let state = pollster::block_on(hidden).unwrap().unwrap();
    assert_eq!(state.decode::<serde_json::Value>().unwrap()["count"], 7);
    assert!(!format!("{state:?}").contains("count"));
    let replaced = window
        .update(&mut cx, |_, window, cx| {
            let capture = first.capture_state(a, cx).unwrap();
            let replacement = first
                .replace(a, &first_factory, &AppConfig::default(), window, cx)
                .unwrap();
            assert_ne!(replacement, a);
            capture
        })
        .unwrap();
    let executed = Cell::new(false);
    first_factory.0.borrow_mut().pop().unwrap().run(|| {
        executed.set(true);
        Ok(None)
    });
    assert_eq!(pollster::block_on(replaced).unwrap_err(), Error::Closed);
    assert!(!executed.get());
    peer_factory
        .0
        .borrow_mut()
        .pop()
        .unwrap()
        .run(|| Ok(Some(AppState::new(serde_json::json!({"count":9})).unwrap())));
    assert_eq!(
        pollster::block_on(valid)
            .unwrap()
            .unwrap()
            .decode::<serde_json::Value>()
            .unwrap()["count"],
        9
    );
    let closed = window
        .update(&mut cx, |_, window, cx| {
            let capture = peer.capture_state(b, cx).unwrap();
            peer.close(b, window, cx);
            capture
        })
        .unwrap();
    peer_factory.0.borrow_mut().pop().unwrap().run(|| {
        executed.set(true);
        Ok(None)
    });
    assert_eq!(pollster::block_on(closed).unwrap_err(), Error::Closed);
    assert!(!executed.get());
}

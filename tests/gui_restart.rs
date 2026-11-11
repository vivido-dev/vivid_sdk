//! Replacement remains atomic and scoped to the complete host identity.
#![cfg(feature = "gui-testing")]

// Rust guideline compliant 2026-10-07
use std::{
    cell::{Cell, RefCell},
    io,
    rc::Rc,
};
use vivid_sdk::gui::{
    self as gpui, AnyView, App, AppContext, Context, Entity, IntoElement, Render, TestAppContext,
    Window, div,
};
use vivid_sdk::gui_host::{
    AppConfig, AppDescriptor, AppFactory, AppHost, AppStatus, HostedApp, Lifecycle,
};

#[derive(Debug)]
struct View {
    local_id: u64,
    value: u64,
}
impl Render for View {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}
#[derive(Debug)]
struct Factory {
    fail: Rc<Cell<bool>>,
    events: Rc<RefCell<Vec<Lifecycle>>>,
    status: Rc<Cell<AppStatus>>,
    value: u64,
    visibility: Rc<RefCell<Vec<bool>>>,
}
#[derive(Debug)]
struct Instance(
    Entity<View>,
    Rc<RefCell<Vec<Lifecycle>>>,
    Rc<Cell<AppStatus>>,
    Rc<RefCell<Vec<bool>>>,
);
impl HostedApp for Instance {
    fn view(&self) -> AnyView {
        self.0.clone().into()
    }
    fn status(&self, _: &App) -> AppStatus {
        self.2.get()
    }
    fn visibility(&mut self, visible: bool, _: &mut Window, _: &mut App) {
        self.3.borrow_mut().push(visible);
    }
    fn lifecycle(&mut self, event: Lifecycle, _: &mut Window, _: &mut App) {
        self.1.borrow_mut().push(event);
    }
}
impl AppFactory for Factory {
    fn descriptor(&self) -> AppDescriptor {
        AppDescriptor::new("test", "Test")
    }
    fn create(
        &self,
        _: &AppConfig,
        _: &mut Window,
        cx: &mut App,
    ) -> io::Result<Box<dyn HostedApp>> {
        if self.fail.get() {
            return Err(io::Error::other("creation failed"));
        }
        Ok(Box::new(Instance(
            cx.new(|_| View {
                local_id: 7,
                value: self.value,
            }),
            self.events.clone(),
            self.status.clone(),
            self.visibility.clone(),
        )))
    }
}

#[gpui::test]
fn hiding_revokes_activation_preserves_views_and_scopes_replacement(cx: &mut TestAppContext) {
    cx.add_window(|window, cx| {
        let make = |value| Factory {
            fail: Rc::new(Cell::new(false)),
            events: Rc::new(RefCell::new(Vec::new())),
            status: Rc::new(Cell::new(AppStatus::Running)),
            value,
            visibility: Rc::new(RefCell::new(Vec::new())),
        };
        let left_factory = make(11);
        let right_factory = make(29);
        let mut left = AppHost::new(1).unwrap();
        let mut right = AppHost::new(1).unwrap();
        let config = AppConfig::default();
        let a = left.mount(&left_factory, &config, window, cx).unwrap();
        let b = right.mount(&right_factory, &config, window, cx).unwrap();
        left.activate(a, window, cx);
        right.activate(b, window, cx);
        assert!(!right.set_visible(a, false, window, cx));
        assert!(left.set_visible(a, false, window, cx));
        assert!(left.set_visible(a, false, window, cx));
        assert_eq!(left.active(), None);
        assert!(!left.focus(a, window, cx));
        assert_eq!(left.is_visible(a), Some(false));
        assert_eq!(*left_factory.visibility.borrow(), [false]);
        assert_eq!(
            *left_factory.events.borrow(),
            [
                Lifecycle::Mounted,
                Lifecycle::Activated,
                Lifecycle::Deactivated
            ]
        );
        let view = left.view(a).unwrap().downcast::<View>().unwrap();
        assert_eq!((view.read(cx).local_id, view.read(cx).value), (7, 11));
        let replaced = left.replace(a, &left_factory, &config, window, cx).unwrap();
        assert_ne!(a, replaced);
        assert_eq!(left.is_visible(replaced), Some(false));
        assert!(!left.activate(replaced, window, cx));
        assert_eq!(left.is_visible(a), None);
        assert!(left.set_visible(replaced, true, window, cx));
        assert_eq!(left.active(), None);
        assert!(left.focus(replaced, window, cx));
        assert_eq!(*left_factory.visibility.borrow(), [false, false, true]);
        assert_eq!(right.active(), Some(b));
        assert_eq!(right.is_visible(b), Some(true));
        let view = right.view(b).unwrap().downcast::<View>().unwrap();
        assert_eq!((view.read(cx).local_id, view.read(cx).value), (7, 29));
        assert!(right_factory.visibility.borrow().is_empty());
        assert_eq!(
            *right_factory.events.borrow(),
            [Lifecycle::Mounted, Lifecycle::Activated]
        );
        left.shutdown(window, cx);
        right.shutdown(window, cx);
        View {
            local_id: 0,
            value: 0,
        }
    });
}

#[gpui::test]
fn replacement_preserves_failed_attempts_capacity_activation_and_other_owners(
    cx: &mut TestAppContext,
) {
    cx.add_window(|window, cx| {
        let events = Rc::new(RefCell::new(Vec::new()));
        let fail = Rc::new(Cell::new(false));
        let status = Rc::new(Cell::new(AppStatus::Running));
        let mut factory = Factory {
            fail: fail.clone(),
            events: events.clone(),
            status: status.clone(),
            value: 11,
            visibility: Rc::new(RefCell::new(Vec::new())),
        };
        let mut first = AppHost::new(1).unwrap();
        let mut second = AppHost::new(1).unwrap();
        let config = AppConfig::default();
        let left = first.mount(&factory, &config, window, cx).unwrap();
        factory.value = 29;
        factory.events = Rc::new(RefCell::new(Vec::new()));
        let other_events = factory.events.clone();
        let right = second.mount(&factory, &config, window, cx).unwrap();
        first.activate(left, window, cx);
        second.activate(right, window, cx);
        status.set(AppStatus::Exited);
        assert_eq!(first.status(left, cx), Some(AppStatus::Exited));
        assert_eq!(second.status(left, cx), None);
        let before = events.borrow().clone();
        assert!(second.replace(left, &factory, &config, window, cx).is_err());
        fail.set(true);
        assert!(first.replace(left, &factory, &config, window, cx).is_err());
        assert_eq!(first.active(), Some(left));
        assert_eq!(*events.borrow(), before);
        assert_eq!(
            first
                .view(left)
                .unwrap()
                .downcast::<View>()
                .unwrap()
                .read(cx)
                .value,
            11
        );
        fail.set(false);
        factory.value = 17;
        factory.events = Rc::new(RefCell::new(Vec::new()));
        let new_events = factory.events.clone();
        let replacement = first.replace(left, &factory, &config, window, cx).unwrap();
        assert_ne!(left, replacement);
        assert_eq!(first.active(), Some(replacement));
        assert!(first.view(left).is_none());
        assert!(!first.close(left, window, cx));
        assert!(first.mount(&factory, &config, window, cx).is_err());
        assert_eq!(
            &events.borrow()[before.len()..],
            &[Lifecycle::Deactivated, Lifecycle::Closing]
        );
        assert_eq!(
            *new_events.borrow(),
            [Lifecycle::Mounted, Lifecycle::Activated]
        );
        let view = second.view(right).unwrap().downcast::<View>().unwrap();
        assert_eq!((view.read(cx).local_id, view.read(cx).value), (7, 29));
        assert_eq!(second.active(), Some(right));
        assert_eq!(
            *other_events.borrow(),
            [Lifecycle::Mounted, Lifecycle::Activated]
        );
        // Replacing an inactive pane must leave its neighbor active.
        let mut third = AppHost::new(2).unwrap();
        let a = third.mount(&factory, &config, window, cx).unwrap();
        let b = third.mount(&factory, &config, window, cx).unwrap();
        third.activate(b, window, cx);
        let replacement_a = third.replace(a, &factory, &config, window, cx).unwrap();
        assert_ne!(a, replacement_a);
        assert_eq!(third.active(), Some(b));
        first.shutdown(window, cx);
        second.shutdown(window, cx);
        third.shutdown(window, cx);
        assert_eq!(
            events
                .borrow()
                .iter()
                .filter(|e| **e == Lifecycle::Closing)
                .count(),
            1
        );
        View {
            local_id: 0,
            value: 0,
        }
    });
}

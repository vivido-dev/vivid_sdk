//! Advisory close checks include hidden apps and never cross owner boundaries.
#![cfg(feature = "gui-testing")]

// Rust guideline compliant 2026-10-07
use std::{cell::Cell, io, rc::Rc};
use vivid_sdk::gui::{
    self as gpui, AnyView, App, AppContext, Context, Entity, IntoElement, Render, TestAppContext,
    Window, div,
};
use vivid_sdk::gui_host::{
    AppConfig, AppDescriptor, AppFactory, AppHost, CloseReason, CloseStatus, HostedApp, Lifecycle,
};
#[derive(Debug)]
struct View(u64);
impl Render for View {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}
#[derive(Debug)]
struct Factory {
    status: Rc<Cell<CloseStatus>>,
    queries: Rc<Cell<usize>>,
    closes: Rc<Cell<usize>>,
}
#[derive(Debug)]
struct Instance(
    Entity<View>,
    Rc<Cell<CloseStatus>>,
    Rc<Cell<usize>>,
    Rc<Cell<usize>>,
);
impl HostedApp for Instance {
    fn view(&self) -> AnyView {
        self.0.clone().into()
    }
    fn close_status(&self, _: u64, _: &App) -> CloseStatus {
        self.2.set(self.2.get() + 1);
        self.1.get()
    }
    fn lifecycle(&mut self, event: Lifecycle, _: &mut Window, _: &mut App) {
        if event == Lifecycle::Closing {
            self.3.set(self.3.get() + 1);
        }
    }
}
impl AppFactory for Factory {
    fn descriptor(&self) -> AppDescriptor {
        AppDescriptor::new("guarded", "Guarded")
    }
    fn create(
        &self,
        _: &AppConfig,
        _: &mut Window,
        cx: &mut App,
    ) -> io::Result<Box<dyn HostedApp>> {
        Ok(Box::new(Instance(
            cx.new(|_| View(7)),
            self.status.clone(),
            self.queries.clone(),
            self.closes.clone(),
        )))
    }
}
#[gpui::test]
fn close_status_is_advisory_scoped_and_available_for_hidden_instances(cx: &mut TestAppContext) {
    cx.add_window(|window, cx| {
        let make = || Factory {
            status: Rc::new(Cell::new(CloseStatus::Ready)),
            queries: Rc::new(Cell::new(0)),
            closes: Rc::new(Cell::new(0)),
        };
        let a = make();
        let b = make();
        let config = AppConfig::default();
        let mut left = AppHost::new(1).unwrap();
        let mut right = AppHost::new(1).unwrap();
        let first = left.mount(&a, &config, window, cx).unwrap();
        let second = right.mount(&b, &config, window, cx).unwrap();
        left.activate(first, window, cx);
        right.activate(second, window, cx);
        a.status.set(CloseStatus::Pending);
        assert_eq!(
            left.close_status(first, 0, cx),
            Some(CloseStatus::Confirm(CloseReason::Unknown))
        );
        assert_eq!(a.queries.get(), 0);
        assert_eq!(left.close_status(first, 1, cx), Some(CloseStatus::Pending));
        assert_eq!(right.close_status(first, 1, cx), None);
        assert_eq!(b.queries.get(), 0);
        left.set_visible(first, false, window, cx);
        a.status
            .set(CloseStatus::Confirm(CloseReason::UnsavedChanges));
        assert_eq!(
            left.close_status(first, 1, cx),
            Some(CloseStatus::Confirm(CloseReason::UnsavedChanges))
        );
        assert_eq!(a.closes.get(), 0);
        assert_eq!(right.active(), Some(second));
        assert_eq!(right.is_visible(second), Some(true));
        assert_eq!(
            right
                .view(second)
                .unwrap()
                .downcast::<View>()
                .unwrap()
                .read(cx)
                .0,
            7
        );
        // Explicit disposal remains available for rollback, shutdown, or confirmed closes.
        assert!(left.close(first, window, cx));
        assert_eq!(a.closes.get(), 1);
        assert_eq!(right.close_status(second, 1, cx), Some(CloseStatus::Ready));
        assert_eq!(b.closes.get(), 0);
        right.shutdown(window, cx);
        View(0)
    });
}

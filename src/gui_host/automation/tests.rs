//! Request bounds, deferred-work retirement and real host lifecycle routing.

// Rust guideline compliant 2026-10-07
use super::*;
use crate::gui_host::{AppDescriptor, AppFactory, AppHost, HostedApp, Lifecycle};
use gpui::{
    App, AppContext as _, Context as ViewContext, Entity, IntoElement, Render, TestAppContext,
    Window, div,
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

#[test]
fn data_and_diagnostics_are_bounded() {
    assert!(Data::new(serde_json::json!([])).is_err());
    assert!(Data::new(serde_json::json!({"value":"x".repeat(MAX_DATA_BYTES)})).is_err());
    let data = Data::new(serde_json::json!({"value":"private-marker"})).unwrap();
    assert!(!format!("{data:?}").contains("private-marker"));
    let request = Request::new("read".into(), data).unwrap();
    assert!(!format!("{request:?}").contains("private-marker"));
    assert!(Request::new("Bad Method".into(), Data::default()).is_err());
}

#[test]
fn cancellation_keeps_worker_admission_and_expiry_skips_work() {
    let cx = TestAppContext::single();
    let mut broker = Broker::new();
    let id = InstanceId {
        host: [1; 16],
        serial: 1,
    };
    let mut pairs = Vec::new();
    for _ in 0..MAX_OWNER_CALLS {
        pairs.push(cx.update(|cx| broker.admit(id, false, false, cx)).unwrap());
    }
    assert_eq!(
        cx.update(|cx| broker.admit(id, false, false, cx))
            .unwrap_err(),
        Error::Busy
    );
    let (reply, response) = pairs.pop().unwrap();
    drop(reply);
    assert_eq!(
        cx.update(|cx| broker.admit(id, false, false, cx))
            .unwrap_err(),
        Error::Busy
    );
    drop(response);
    let (reply, response) = cx.update(|cx| broker.admit(id, false, false, cx)).unwrap();
    cx.executor().advance_clock(CALL_TIMEOUT);
    assert_eq!(pollster::block_on(reply).unwrap_err(), Error::Expired);
    let mut ran = false;
    response.run(|| {
        ran = true;
        Ok(Data::default())
    });
    assert!(!ran);
    drop(pairs);
    assert_eq!(broker.budget.used(), 0);
    let mut held = Vec::new();
    for serial in 1..=MAX_HOST_CALLS as u64 {
        held.push(
            cx.update(|cx| {
                broker.admit(
                    InstanceId {
                        host: [2; 16],
                        serial,
                    },
                    false,
                    false,
                    cx,
                )
            })
            .unwrap(),
        );
    }
    assert_eq!(
        cx.update(|cx| broker.admit(id, false, false, cx))
            .unwrap_err(),
        Error::Busy
    );
    drop(held);
    assert_eq!(broker.budget.used(), 0);
}

#[test]
fn completed_replies_remain_bounded_and_cancelled_commands_are_not_retry_receipts() {
    let cx = TestAppContext::single();
    let mut broker = Broker::new();
    let id = InstanceId {
        host: [3; 16],
        serial: 1,
    };
    let mut replies = Vec::new();
    for _ in 0..MAX_OWNER_CALLS {
        let (reply, response) = cx.update(|cx| broker.admit(id, false, true, cx)).unwrap();
        response.run(|| Ok(Data::default()));
        replies.push(reply);
    }
    assert_eq!(
        cx.update(|cx| broker.admit(id, false, true, cx))
            .unwrap_err(),
        Error::Busy
    );
    let mut reply = replies.pop().unwrap();
    assert!(pollster::block_on(&mut reply).is_ok());
    assert_eq!(broker.budget.used(), MAX_OWNER_CALLS - 1);
    drop(replies);
    let (reply, response) = cx.update(|cx| broker.admit(id, true, true, cx)).unwrap();
    let mut changed = false;
    response.run(|| {
        changed = true;
        broker.interactive(id, false);
        Ok(Data::default())
    });
    assert!(changed);
    assert_eq!(pollster::block_on(reply).unwrap_err(), Error::Inactive);
    assert_eq!(broker.budget.used(), 0);
}

#[derive(Debug)]
struct View;
impl Render for View {
    fn render(&mut self, _: &mut Window, _: &mut ViewContext<Self>) -> impl IntoElement {
        div()
    }
}
type Queue = Rc<RefCell<Vec<(Request, Responder)>>>;
#[derive(Debug)]
struct Factory(Queue);
#[derive(Debug)]
struct Instance(Entity<View>, Queue);
impl HostedApp for Instance {
    fn view(&self) -> gpui::AnyView {
        self.0.clone().into()
    }
    fn lifecycle(&mut self, _: Lifecycle, _: &mut Window, _: &mut App) {}
    fn automation_methods(&self, _: &App) -> &'static [Method] {
        &[
            Method {
                name: "read",
                access: Access::Query,
                description: "Read state.",
            },
            Method {
                name: "write",
                access: Access::Command,
                description: "Change state.",
            },
        ]
    }
    fn automate(&mut self, request: Request, response: Responder, _: &mut App) {
        self.1.borrow_mut().push((request, response));
    }
}
impl AppFactory for Factory {
    fn descriptor(&self) -> AppDescriptor {
        AppDescriptor::new("automation-test", "Automation test")
    }
    fn create(
        &self,
        _: &AppConfig,
        _: &mut Window,
        cx: &mut App,
    ) -> io::Result<Box<dyn HostedApp>> {
        Ok(Box::new(Instance(cx.new(|_| View), self.0.clone())))
    }
}

#[test]
fn dropping_host_revokes_before_application_destructors_run() {
    #[derive(Debug)]
    struct DropInstance(Instance, Rc<Cell<usize>>);
    impl HostedApp for DropInstance {
        fn view(&self) -> gpui::AnyView {
            self.0.view()
        }
        fn lifecycle(&mut self, _: Lifecycle, _: &mut Window, _: &mut App) {}
        fn automation_methods(&self, cx: &App) -> &'static [Method] {
            self.0.automation_methods(cx)
        }
        fn automate(&mut self, request: Request, response: Responder, cx: &mut App) {
            self.0.automate(request, response, cx);
        }
    }
    impl Drop for DropInstance {
        fn drop(&mut self) {
            for (_, response) in self.0.1.borrow_mut().drain(..) {
                response.run(|| {
                    self.1.set(self.1.get() + 1);
                    Ok(Data::default())
                });
            }
        }
    }
    #[derive(Debug)]
    struct DropFactory(Queue, Rc<Cell<usize>>);
    impl AppFactory for DropFactory {
        fn descriptor(&self) -> AppDescriptor {
            AppDescriptor::new("drop-test", "Drop test")
        }
        fn create(
            &self,
            _: &AppConfig,
            _: &mut Window,
            cx: &mut App,
        ) -> io::Result<Box<dyn HostedApp>> {
            Ok(Box::new(DropInstance(
                Instance(cx.new(|_| View), self.0.clone()),
                self.1.clone(),
            )))
        }
    }
    let mut cx = TestAppContext::single();
    let executed = Rc::new(Cell::new(0));
    let factory = DropFactory(Rc::new(RefCell::new(Vec::new())), executed.clone());
    let mut host = AppHost::new(1).unwrap();
    let mut reply = None;
    cx.add_window(|window, cx| {
        let id = host
            .mount(&factory, &AppConfig::default(), window, cx)
            .unwrap();
        reply = Some(
            host.automate(
                id,
                Request::new("read".into(), Data::default()).unwrap(),
                cx,
            )
            .unwrap(),
        );
        View
    });
    drop(host);
    assert_eq!(executed.get(), 0);
    assert_eq!(
        pollster::block_on(reply.unwrap()).unwrap_err(),
        Error::Closed
    );
}

#[test]
fn host_close_and_deactivation_revoke_exact_owners_without_retagging() {
    let mut cx = TestAppContext::single();
    let queue: Queue = Rc::new(RefCell::new(Vec::new()));
    let factory = Factory(queue.clone());
    let mut first = AppHost::new(2).unwrap();
    let mut peer = AppHost::new(2).unwrap();
    let mut ids = None;
    let mut replies = None;
    let window = cx.add_window(|window, cx| {
        let a = first
            .mount(&factory, &AppConfig::default(), window, cx)
            .unwrap();
        let b = peer
            .mount(&factory, &AppConfig::default(), window, cx)
            .unwrap();
        assert_eq!(a.serial, b.serial);
        assert_ne!(a.host, b.host);
        assert_eq!(
            first
                .automate(b, Request::new("read".into(), Data::default()).unwrap(), cx)
                .unwrap_err(),
            Error::Closed
        );
        first.activate(a, window, cx);
        let query = first
            .automate(a, Request::new("read".into(), Data::default()).unwrap(), cx)
            .unwrap();
        let command = first
            .automate(
                a,
                Request::new("write".into(), Data::default()).unwrap(),
                cx,
            )
            .unwrap();
        first.set_visible(a, false, window, cx);
        assert_eq!(
            first
                .automate(
                    a,
                    Request::new("write".into(), Data::default()).unwrap(),
                    cx
                )
                .unwrap_err(),
            Error::Inactive
        );
        first.set_visible(a, true, window, cx);
        first.activate(a, window, cx);
        replies = Some((query, command));
        ids = Some((a, b));
        View
    });
    let executed = Cell::new(0);
    for (_, response) in queue.borrow_mut().drain(..) {
        response.run(|| {
            executed.set(executed.get() + 1);
            Ok(Data::default())
        });
    }
    let (query, command) = replies.take().unwrap();
    assert!(pollster::block_on(query).is_ok());
    assert_eq!(pollster::block_on(command).unwrap_err(), Error::Inactive);
    assert_eq!(executed.get(), 1);
    let (a, b) = ids.unwrap();
    window
        .update(&mut cx, |_, window, cx| {
            let one = first
                .automate(a, Request::new("read".into(), Data::default()).unwrap(), cx)
                .unwrap();
            let two = peer
                .automate(b, Request::new("read".into(), Data::default()).unwrap(), cx)
                .unwrap();
            first.close(a, window, cx);
            replies = Some((one, two));
        })
        .unwrap();
    for (_, response) in queue.borrow_mut().drain(..) {
        response.run(|| {
            executed.set(executed.get() + 1);
            Ok(Data::default())
        });
    }
    let (one, two) = replies.take().unwrap();
    assert_eq!(pollster::block_on(one).unwrap_err(), Error::Closed);
    assert_eq!(two.instance_id(), b);
    assert!(pollster::block_on(two).is_ok());
    assert_eq!(executed.get(), 2);
    window
        .update(&mut cx, |_, window, cx| peer.shutdown(window, cx))
        .unwrap();
}

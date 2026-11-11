//! Hosts trusted applications within one GUI runtime and native event loop.
//!
//! Applications supply views rather than native windows. The host owns placement
//! and focus. Embedded applications share the host's crash boundary; this module
//! does not claim process isolation or grant Vivid protocol authority.

// Rust guideline compliant 2026-10-07
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, io,
};

use gpui::{AnyView, App, Window};

pub mod accessibility;
mod activation;
pub mod automation;
pub mod clipboard;
#[doc(inline)]
pub use activation::ActivationRequest;
mod registry;
mod state;
#[doc(inline)]
pub use registry::{AppConfig, AppDescriptor, AppRegistry, MAX_CONFIG_BYTES, validate_app_id};
#[doc(inline)]
pub use state::{AppLaunch, AppState, StateReply, StateResponder};

/// Bounds resources whose cleanup can outlive their mounted application.
///
/// Clones share one budget. A permit remains charged until its owner drops it,
/// allowing an asynchronous worker to retain capacity after its GUI pane closes.
#[derive(Clone, Debug)]
pub struct ResourceBudget {
    state: std::sync::Arc<BudgetState>,
}
#[derive(Debug)]
struct BudgetState {
    limit: usize,
    used: std::sync::atomic::AtomicUsize,
}

/// Holds reserved resource units until actual cleanup has finished.
#[derive(Debug)]
pub struct ResourcePermit {
    budget: ResourceBudget,
    amount: usize,
}
impl ResourceBudget {
    /// Creates a shared budget; zero capacity disables admission.
    pub fn new(limit: usize) -> Self {
        Self {
            state: std::sync::Arc::new(BudgetState {
                limit,
                used: std::sync::atomic::AtomicUsize::new(0),
            }),
        }
    }
    /// Acquires one slot without waiting on the GUI thread.
    ///
    /// # Errors
    /// Returns `WouldBlock` while all resource slots are held.
    pub fn try_acquire(&self) -> io::Result<ResourcePermit> {
        self.try_acquire_many(1)
    }
    /// Reserves multiple units, such as bytes retained by a queued payload.
    ///
    /// # Errors
    /// Returns `InvalidInput` for zero units and `WouldBlock` for capacity or arithmetic overflow.
    pub fn try_acquire_many(&self, amount: usize) -> io::Result<ResourcePermit> {
        use std::sync::atomic::Ordering;
        if amount == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "resource amount must be nonzero",
            ));
        }
        self.state
            .used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(amount)
                    .filter(|next| *next <= self.state.limit)
            })
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "application resource limit reached",
                )
            })?;
        Ok(ResourcePermit {
            budget: self.clone(),
            amount,
        })
    }
    /// Returns the number of units currently held, including pending cleanup.
    pub fn used(&self) -> usize {
        self.state.used.load(std::sync::atomic::Ordering::Acquire)
    }
}
impl Drop for ResourcePermit {
    fn drop(&mut self) {
        self.budget
            .state
            .used
            .fetch_sub(self.amount, std::sync::atomic::Ordering::Release);
    }
}

/// Identifies one application instance within a host.
///
/// The host allocates monotonically increasing IDs and never reuses them.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct InstanceId {
    host: [u8; 16],
    serial: u64,
}

impl std::fmt::Display for InstanceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for byte in self.host {
            write!(f, "{byte:02x}")?;
        }
        write!(f, ":{}", self.serial)
    }
}

/// Lifecycle transitions delivered by the embedding host.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Lifecycle {
    /// The application's view has been attached.
    Mounted,
    /// The application became the active pane.
    Activated,
    /// The application lost activation.
    Deactivated,
    /// The host is releasing the application's view and resources.
    Closing,
}

/// Current execution state of one hosted application.
///
/// Status is advisory and contains no application data or protocol authority.
/// Hosts must arrange observation or bounded polling when showing status changes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AppStatus {
    /// Initialization is still in progress.
    Starting,
    /// The application is available for interaction.
    Running,
    /// The application's runtime has finished normally.
    Exited,
    /// Initialization or execution failed.
    Failed,
}

/// A bounded reason a user should confirm application closure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CloseReason {
    /// Closing would stop running application work.
    RunningWork,
    /// Closing would discard unsaved application state.
    UnsavedChanges,
    /// The application cannot currently determine whether closure loses work.
    Unknown,
}

/// Advisory close readiness without blocking the GUI thread.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CloseStatus {
    /// No application-specific confirmation is currently needed.
    Ready,
    /// A bounded asynchronous readiness query is still in progress.
    Pending,
    /// Closure requires explicit user consent for the identified application.
    Confirm(CloseReason),
}

/// An application view and its lifecycle behavior.
pub trait HostedApp: fmt::Debug + 'static {
    /// Receives an instance-scoped handle for advisory host activation requests.
    fn set_activation_request(&mut self, _request: ActivationRequest, _cx: &mut App) {}
    /// Returns the application's root view.
    fn view(&self) -> AnyView;

    /// Declares bounded application-local queries and commands without blocking.
    fn automation_methods(&self, _cx: &App) -> &'static [automation::Method] {
        &[]
    }

    /// Handles a call through its cancellation-aware responder without blocking.
    ///
    /// Defer slow work to a bounded worker queue. Use `Responder::run` around
    /// the work itself, so retired requests cannot begin mutations later.
    fn automate(
        &mut self,
        _request: automation::Request,
        response: automation::Responder,
        _cx: &mut App,
    ) {
        response.run(|| Err(automation::Error::Unsupported));
    }

    /// Reports whether user-initiated closure would discard or stop work.
    ///
    /// This must not block. Hosts poll Pending with a bounded deadline, include
    /// hidden apps, and scope user consent to the exact checked identities.
    /// Readiness is advisory; forced disposal and shutdown still use `close`.
    /// `request` is a fresh nonzero generation per host close attempt; polls of
    /// that attempt reuse it. Samples for an older generation must not authorize a new close.
    fn close_status(&self, _request: u64, _cx: &App) -> CloseStatus {
        CloseStatus::Ready
    }

    /// Captures optional application state without blocking or persisting authority.
    ///
    /// Return owned data independent of the live view. Hosts may call this for
    /// hidden apps. Keep secrets and runtime/input/clipboard handles out of state.
    ///
    /// # Errors
    /// Returns an error when a consistent snapshot is currently unavailable.
    fn save_state(&self, _cx: &App) -> io::Result<Option<AppState>> {
        Ok(None)
    }

    /// Captures state immediately or through a bounded asynchronous worker.
    ///
    /// The default delegates to `save_state`. Deferred implementations move the
    /// responder to their own bounded queue and call `run` around capture itself.
    fn capture_state(&mut self, response: StateResponder, cx: &mut App) {
        response.run(|| {
            self.save_state(cx)
                .map_err(|_| automation::Error::Unavailable)
        });
    }

    /// Reports execution state without blocking or acquiring runtime resources.
    fn status(&self, _cx: &App) -> AppStatus {
        AppStatus::Running
    }

    /// Receives visibility changes without destroying application state.
    ///
    /// Hidden applications cannot be activated through `AppHost`. Suspend frame
    /// transfers and revoke view-local input/clipboard work while hidden, but
    /// keep background application work independent of this presentation hint.
    fn visibility(&mut self, _visible: bool, _window: &mut Window, _cx: &mut App) {}

    /// Receives a host lifecycle transition on the GUI thread.
    fn lifecycle(&mut self, event: Lifecycle, window: &mut Window, cx: &mut App);

    /// Restores keyboard focus within an already active application.
    ///
    /// Hosts can invoke this after their own controls temporarily receive focus.
    fn focus(&mut self, _window: &mut Window, _cx: &mut App) {}
}

/// Creates application instances without starting another native event loop.
pub trait AppFactory: fmt::Debug + 'static {
    /// Describes the application's stable ID and human-readable name.
    fn descriptor(&self) -> AppDescriptor;

    /// Validates options without creating GUI or runtime resources.
    ///
    /// Validation may run more than once. Keep diagnostics bounded and free of
    /// configuration values; acquire runtime resources only in `create`.
    ///
    /// # Errors
    /// The default rejects all nonempty options; applications may decode their own schema.
    fn validate(&self, config: &AppConfig) -> io::Result<()> {
        if config.is_empty() {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "this application accepts no options",
            ))
        }
    }

    /// Validates saved state without creating views or runtime resources.
    ///
    /// # Errors
    /// The default rejects saved state rather than silently discarding it.
    fn validate_state(&self, _config: &AppConfig, _state: &AppState) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "application state restoration is unsupported",
        ))
    }

    /// Creates a fresh instance, optionally restoring validated application state.
    ///
    /// # Errors
    /// Returns an error for unsupported state or application initialization failure.
    fn create_restored(
        &self,
        launch: AppLaunch<'_>,
        window: &mut Window,
        cx: &mut App,
    ) -> io::Result<Box<dyn HostedApp>> {
        if launch.state.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "application state restoration is unsupported",
            ));
        }
        self.create(launch.options, window, cx)
    }

    /// Creates a hosted application on the host's GUI thread.
    ///
    /// # Errors
    /// Returns an error for invalid options or application initialization failure.
    fn create(
        &self,
        config: &AppConfig,
        window: &mut Window,
        cx: &mut App,
    ) -> io::Result<Box<dyn HostedApp>>;
}

/// Owns a bounded set of embedded application instances.
///
/// A host is local to one GUI thread and window. Closing one instance cannot
/// remove another instance, even when their internal view IDs coincide.
#[derive(Debug)]
pub struct AppHost {
    identity: [u8; 16],
    instances: BTreeMap<InstanceId, Box<dyn HostedApp>>,
    hidden: BTreeSet<InstanceId>,
    active: Option<InstanceId>,
    next_id: u64,
    limit: usize,
    activation_requests: activation::Requests,
    automation: automation::Broker,
    state: automation::Broker<Option<AppState>>,
}

impl AppHost {
    /// Returns this host's random identity, independently of application serials.
    ///
    /// This identifies a host lifetime; it is not an authentication capability.
    pub fn identity(&self) -> [u8; 16] {
        self.identity
    }
    /// Creates an empty host with an explicit instance limit.
    ///
    /// # Errors
    /// Returns an error if host identity entropy is unavailable.
    pub fn new(limit: usize) -> io::Result<Self> {
        let mut identity = [0; 16];
        getrandom::fill(&mut identity).map_err(io::Error::other)?;
        Ok(Self {
            identity,
            instances: BTreeMap::new(),
            hidden: BTreeSet::new(),
            active: None,
            next_id: 1,
            limit,
            activation_requests: activation::Requests::new(limit),
            automation: automation::Broker::new(),
            state: automation::Broker::new(),
        })
    }

    /// Creates and mounts an application without changing activation.
    ///
    /// # Errors
    /// Returns an error for capacity, ID exhaustion, invalid metadata/options, or initialization.
    pub fn mount(
        &mut self,
        factory: &dyn AppFactory,
        config: &AppConfig,
        window: &mut Window,
        cx: &mut App,
    ) -> io::Result<InstanceId> {
        self.mount_launch(factory, AppLaunch::new(config), window, cx)
    }

    /// Mounts a fresh owner from options and optional validated restoration state.
    ///
    /// # Errors
    /// Returns capacity, identity, metadata, launch validation, or creation errors.
    pub fn mount_launch(
        &mut self,
        factory: &dyn AppFactory,
        launch: AppLaunch<'_>,
        window: &mut Window,
        cx: &mut App,
    ) -> io::Result<InstanceId> {
        if self.instances.len() >= self.limit {
            return Err(io::Error::other("application instance limit reached"));
        }
        let next = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| io::Error::other("application identity exhausted"))?;
        factory.descriptor().validate()?;
        factory.validate(launch.options)?;
        if let Some(state) = launch.state {
            factory.validate_state(launch.options, state)?;
        }
        let mut instance = factory.create_restored(launch, window, cx)?;
        let id = InstanceId {
            host: self.identity,
            serial: self.next_id,
        };
        self.next_id = next;
        instance.set_activation_request(self.activation_requests.bind(id), cx);
        instance.lifecycle(Lifecycle::Mounted, window, cx);
        self.instances.insert(id, instance);
        Ok(id)
    }

    /// Returns a mounted instance's view.
    pub fn view(&self, id: InstanceId) -> Option<AnyView> {
        self.instances.get(&id).map(|instance| instance.view())
    }

    /// Returns execution state only for an instance owned by this host.
    pub fn status(&self, id: InstanceId, cx: &App) -> Option<AppStatus> {
        self.instances.get(&id).map(|instance| instance.status(cx))
    }

    /// Discovers validated methods on exactly one mounted owner, including hidden apps.
    ///
    /// # Errors
    /// Returns `Closed` for foreign/retired IDs or `InvalidRequest` for invalid metadata.
    pub fn automation_methods(
        &self,
        id: InstanceId,
        cx: &App,
    ) -> Result<Vec<automation::Method>, automation::Error> {
        let instance = self.instances.get(&id).ok_or(automation::Error::Closed)?;
        automation::methods(instance.automation_methods(cx))
    }

    /// Dispatches one bounded application call without changing focus or placement.
    ///
    /// Queries may address hidden apps. Commands require the active visible owner;
    /// deactivation cancels queued commands, and close/replacement cancels all calls.
    /// Completion is application-defined, never an implicit display or input receipt.
    ///
    /// # Errors
    /// Returns identity, method, execution-state, active-owner or admission errors.
    pub fn automate(
        &mut self,
        id: InstanceId,
        request: automation::Request,
        cx: &mut App,
    ) -> Result<automation::Reply, automation::Error> {
        let methods = self.automation_methods(id, cx)?;
        let method = methods
            .iter()
            .find(|method| method.name == request.method())
            .ok_or(automation::Error::Unsupported)?;
        if self.status(id, cx) != Some(AppStatus::Running) {
            return Err(automation::Error::Unavailable);
        }
        let (reply, response) = self.automation.admit(
            id,
            method.access == automation::Access::Command,
            self.active == Some(id) && !self.hidden.contains(&id),
            cx,
        )?;
        // Admission above cannot remove an instance on this GUI thread.
        self.instances
            .get_mut(&id)
            .expect("admitted application exists")
            .automate(request, response, cx);
        Ok(reply)
    }
    /// Returns advisory close readiness only for an owned mounted instance.
    ///
    /// Use a fresh nonzero request generation for each attempt and reuse it
    /// only while polling that attempt. Zero produces Unknown without calling the app.
    pub fn close_status(&self, id: InstanceId, request: u64, cx: &App) -> Option<CloseStatus> {
        if request == 0 {
            return self
                .instances
                .contains_key(&id)
                .then_some(CloseStatus::Confirm(CloseReason::Unknown));
        }
        self.instances
            .get(&id)
            .map(|instance| instance.close_status(request, cx))
    }

    /// Captures state from exactly one owned instance, including hidden apps.
    ///
    /// # Errors
    /// Returns `NotFound` for foreign/missing identities or an application snapshot error.
    pub fn save_state(&self, id: InstanceId, cx: &App) -> io::Result<Option<AppState>> {
        self.instances
            .get(&id)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, "application instance is absent")
            })?
            .save_state(cx)
    }

    /// Begins owner-scoped state capture without blocking the GUI thread.
    ///
    /// Hidden instances may be captured. Close/replacement cancels capture, and
    /// its reservation remains held until any deferred worker actually retires.
    ///
    /// # Errors
    /// Returns `Closed` for a foreign/retired owner or `Busy` at admission capacity.
    pub fn capture_state(
        &mut self,
        id: InstanceId,
        cx: &mut App,
    ) -> Result<StateReply, automation::Error> {
        if !self.instances.contains_key(&id) {
            return Err(automation::Error::Closed);
        }
        let (reply, response) = self.state.admit(id, false, false, cx)?;
        self.instances
            .get_mut(&id)
            .expect("capture owner exists")
            .capture_state(response, cx);
        Ok(reply)
    }

    /// Returns whether a mounted instance is visible, rejecting foreign identities.
    pub fn is_visible(&self, id: InstanceId) -> Option<bool> {
        self.instances
            .contains_key(&id)
            .then(|| !self.hidden.contains(&id))
    }

    /// Drains coalesced visible-owner activation requests in user-request order.
    ///
    /// Hosts arbitrate these requests against modal and workspace policy before
    /// calling [`Self::focus`]. At most one request per mounted owner is retained.
    pub fn take_activation_requests(&self) -> Vec<InstanceId> {
        self.activation_requests.take()
    }

    /// Shows or hides exactly one mounted application without closing it.
    ///
    /// Hiding first deactivates an active instance. Repeated values are no-ops;
    /// showing does not activate or restore keyboard focus automatically.
    /// Returns false for a missing or foreign instance without invoking callbacks.
    pub fn set_visible(
        &mut self,
        id: InstanceId,
        visible: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> bool {
        let Some(previous) = self.is_visible(id) else {
            return false;
        };
        if previous == visible {
            return true;
        }
        self.activation_requests.visible(id, visible);
        if visible {
            self.hidden.remove(&id);
        } else {
            if self.active == Some(id) {
                self.deactivate(window, cx);
            }
            self.hidden.insert(id);
        }
        if let Some(instance) = self.instances.get_mut(&id) {
            instance.visibility(visible, window, cx);
        }
        true
    }

    /// Deactivates the current application without selecting another owner.
    pub fn deactivate(&mut self, window: &mut Window, cx: &mut App) -> bool {
        let Some(id) = self.active.take() else {
            return false;
        };
        self.automation.interactive(id, false);
        if let Some(instance) = self.instances.get_mut(&id) {
            instance.lifecycle(Lifecycle::Deactivated, window, cx);
        }
        true
    }

    /// Replaces one instance with a freshly identified application.
    ///
    /// Creation and validation finish before the old instance is deactivated or
    /// closed. Failure preserves its view, activation, and identity. Success
    /// closes it exactly once and transfers activation to the new instance.
    /// One replacement may be under construction beyond the mounted limit;
    /// factories must still enforce their own live/retiring resource budgets.
    ///
    /// # Errors
    /// Returns an error for a foreign/missing ID, ID exhaustion, invalid launch
    /// settings, or initialization failure, without changing mounted instances.
    pub fn replace(
        &mut self,
        id: InstanceId,
        factory: &dyn AppFactory,
        config: &AppConfig,
        window: &mut Window,
        cx: &mut App,
    ) -> io::Result<InstanceId> {
        self.replace_launch(id, factory, AppLaunch::new(config), window, cx)
    }

    /// Atomically replaces one owner with a launch containing optional saved state.
    ///
    /// Visibility and activation follow `replace`; no runtime identity is restored.
    ///
    /// # Errors
    /// Returns identity, validation, or creation errors without closing the old instance.
    pub fn replace_launch(
        &mut self,
        id: InstanceId,
        factory: &dyn AppFactory,
        launch: AppLaunch<'_>,
        window: &mut Window,
        cx: &mut App,
    ) -> io::Result<InstanceId> {
        if !self.instances.contains_key(&id) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "application instance is absent",
            ));
        }
        let next = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| io::Error::other("application identity exhausted"))?;
        factory.descriptor().validate()?;
        factory.validate(launch.options)?;
        if let Some(state) = launch.state {
            factory.validate_state(launch.options, state)?;
        }
        let mut instance = factory.create_restored(launch, window, cx)?;
        let replacement = InstanceId {
            host: self.identity,
            serial: self.next_id,
        };
        self.next_id = next;
        let active = self.active == Some(id);
        let visible = !self.hidden.contains(&id);
        self.close(id, window, cx);
        instance.set_activation_request(self.activation_requests.bind(replacement), cx);
        instance.lifecycle(Lifecycle::Mounted, window, cx);
        self.instances.insert(replacement, instance);
        if !visible {
            self.set_visible(replacement, false, window, cx);
        }
        if active {
            self.activate(replacement, window, cx);
        }
        Ok(replacement)
    }

    /// Activates an existing instance and deactivates the previous one.
    ///
    /// Returns false for an unknown ID without changing the active application.
    pub fn activate(&mut self, id: InstanceId, window: &mut Window, cx: &mut App) -> bool {
        if !self.instances.contains_key(&id) || self.hidden.contains(&id) {
            return false;
        }
        if self.active == Some(id) {
            return true;
        }
        if let Some(previous_id) = self.active.take() {
            self.automation.interactive(previous_id, false);
            if let Some(previous) = self.instances.get_mut(&previous_id) {
                previous.lifecycle(Lifecycle::Deactivated, window, cx);
            }
        }
        self.active = Some(id);
        self.automation.interactive(id, true);
        if let Some(instance) = self.instances.get_mut(&id) {
            instance.lifecycle(Lifecycle::Activated, window, cx);
        }
        true
    }

    /// Activates a mounted application and asks its view to restore keyboard focus.
    ///
    /// Returns false for an unknown ID without changing application state.
    pub fn focus(&mut self, id: InstanceId, window: &mut Window, cx: &mut App) -> bool {
        if !self.activate(id, window, cx) {
            return false;
        }
        if let Some(instance) = self.instances.get_mut(&id) {
            instance.focus(window, cx);
        }
        true
    }

    /// Deactivates and removes exactly the named instance.
    ///
    /// This disposal primitive does not consult `close_status`. User-facing
    /// hosts obtain scoped consent first; rollback and forced shutdown may dispose directly.
    pub fn close(&mut self, id: InstanceId, window: &mut Window, cx: &mut App) -> bool {
        let Some(mut instance) = self.instances.remove(&id) else {
            return false;
        };
        self.automation.close(id);
        self.state.close(id);
        self.activation_requests.remove(id);
        self.hidden.remove(&id);
        if self.active == Some(id) {
            self.active = None;
            instance.lifecycle(Lifecycle::Deactivated, window, cx);
        }
        instance.lifecycle(Lifecycle::Closing, window, cx);
        true
    }

    /// Deactivates and closes every instance owned by this host.
    pub fn shutdown(&mut self, window: &mut Window, cx: &mut App) {
        let ids: Vec<_> = self.instances.keys().copied().collect();
        for id in ids {
            self.close(id, window, cx);
        }
    }

    /// Returns the currently active application, if any.
    pub fn active(&self) -> Option<InstanceId> {
        self.active
    }
}

impl Drop for AppHost {
    fn drop(&mut self) {
        self.automation.close_all();
        self.state.close_all();
    }
}

#[cfg(all(test, feature = "gui-testing"))]
mod tests {
    use super::*;
    use gpui::{AppContext, Context, Entity, IntoElement, Render, TestAppContext, div};
    use std::{cell::RefCell, rc::Rc};

    #[derive(Debug)]
    struct View;
    impl Render for View {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }
    #[derive(Debug)]
    struct Factory(Rc<RefCell<Vec<Lifecycle>>>);
    #[derive(Debug)]
    struct Instance(Entity<View>, Rc<RefCell<Vec<Lifecycle>>>);
    impl HostedApp for Instance {
        fn view(&self) -> AnyView {
            self.0.clone().into()
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
            Ok(Box::new(Instance(cx.new(|_| View), self.0.clone())))
        }
    }

    #[gpui::test]
    fn lifecycle_and_instance_isolation(cx: &mut TestAppContext) {
        let a = Rc::new(RefCell::new(Vec::new()));
        let b = Rc::new(RefCell::new(Vec::new()));
        cx.add_window(|window, cx| {
            let mut host = AppHost::new(2).unwrap();
            let first = host
                .mount(&Factory(a.clone()), &AppConfig::default(), window, cx)
                .unwrap();
            let second = host
                .mount(&Factory(b.clone()), &AppConfig::default(), window, cx)
                .unwrap();
            assert_ne!(first, second);
            let mut other = AppHost::new(2).unwrap();
            let unrelated = other
                .mount(&Factory(b.clone()), &AppConfig::default(), window, cx)
                .unwrap();
            assert_eq!(first.serial, unrelated.serial);
            assert_ne!(first.host, unrelated.host);
            assert!(!other.close(first, window, cx));
            assert!(other.view(unrelated).is_some());
            other.shutdown(window, cx);
            b.borrow_mut().clear();
            b.borrow_mut().push(Lifecycle::Mounted);
            assert!(
                host.mount(&Factory(a.clone()), &AppConfig::default(), window, cx)
                    .is_err()
            );
            assert!(host.activate(first, window, cx));
            assert!(host.activate(second, window, cx));
            assert!(host.close(first, window, cx));
            assert!(!host.close(first, window, cx));
            assert!(!host.activate(first, window, cx));
            assert_eq!(host.active(), Some(second));
            assert!(host.view(second).is_some());
            assert_eq!(
                *a.borrow(),
                vec![
                    Lifecycle::Mounted,
                    Lifecycle::Activated,
                    Lifecycle::Deactivated,
                    Lifecycle::Closing
                ]
            );
            assert_eq!(*b.borrow(), vec![Lifecycle::Mounted, Lifecycle::Activated]);
            let third = host
                .mount(&Factory(a.clone()), &AppConfig::default(), window, cx)
                .unwrap();
            assert_ne!(first, third);
            assert!(host.close(second, window, cx));
            assert_eq!(host.active(), None);
            View
        });
    }

    #[derive(Debug)]
    struct FocusFactory(Rc<std::cell::Cell<(u32, u32)>>);
    #[derive(Debug)]
    struct FocusApp(Entity<View>, Rc<std::cell::Cell<(u32, u32)>>);
    impl HostedApp for FocusApp {
        fn view(&self) -> AnyView {
            self.0.clone().into()
        }
        fn lifecycle(&mut self, event: Lifecycle, _: &mut Window, _: &mut App) {
            if event == Lifecycle::Activated {
                let (activated, focused) = self.1.get();
                self.1.set((activated + 1, focused));
            }
        }
        fn focus(&mut self, _: &mut Window, _: &mut App) {
            let (activated, focused) = self.1.get();
            self.1.set((activated, focused + 1));
        }
    }
    impl AppFactory for FocusFactory {
        fn descriptor(&self) -> AppDescriptor {
            AppDescriptor::new("focus-test", "Focus test")
        }
        fn create(
            &self,
            _: &AppConfig,
            _: &mut Window,
            cx: &mut App,
        ) -> io::Result<Box<dyn HostedApp>> {
            Ok(Box::new(FocusApp(cx.new(|_| View), self.0.clone())))
        }
    }
    #[gpui::test]
    fn focus_restoration_is_scoped_and_does_not_repeat_activation(cx: &mut TestAppContext) {
        cx.add_window(|window, cx| {
            let events = Rc::new(std::cell::Cell::new((0, 0)));
            let unrelated = Rc::new(std::cell::Cell::new((0, 0)));
            let mut host = AppHost::new(2).unwrap();
            let mut other = AppHost::new(2).unwrap();
            let id = host
                .mount(
                    &FocusFactory(events.clone()),
                    &AppConfig::default(),
                    window,
                    cx,
                )
                .unwrap();
            let foreign = other
                .mount(
                    &FocusFactory(unrelated.clone()),
                    &AppConfig::default(),
                    window,
                    cx,
                )
                .unwrap();
            assert_eq!(id.serial, foreign.serial);
            assert!(host.focus(id, window, cx));
            assert!(host.focus(id, window, cx));
            assert_eq!(events.get(), (1, 2));
            assert!(!host.focus(foreign, window, cx));
            assert_eq!(unrelated.get(), (0, 0));
            assert_eq!(host.active(), Some(id));
            host.close(id, window, cx);
            assert!(!host.focus(id, window, cx));
            assert!(other.focus(foreign, window, cx));
            assert_eq!(unrelated.get(), (1, 1));
            View
        });
    }
}

#[cfg(test)]
mod budget_tests {
    use super::*;
    #[test]
    fn weighted_reservations_bound_bytes_and_reject_overflow_without_mutation() {
        let budget = ResourceBudget::new(10);
        let permit = budget.try_acquire_many(7).unwrap();
        assert!(budget.try_acquire_many(4).is_err());
        assert!(budget.try_acquire_many(0).is_err());
        assert_eq!(budget.used(), 7);
        drop(permit);
        assert_eq!(budget.used(), 0);
        let maximal = ResourceBudget::new(usize::MAX);
        let permit = maximal.try_acquire_many(usize::MAX).unwrap();
        assert!(maximal.try_acquire().is_err());
        assert_eq!(maximal.used(), usize::MAX);
        drop(permit);
        assert_eq!(maximal.used(), 0);
    }
    #[test]
    fn worker_cleanup_retains_shared_capacity_until_permit_drop() {
        let budget = ResourceBudget::new(1);
        let permit = budget.try_acquire().unwrap();
        let started = std::sync::Arc::new(std::sync::Barrier::new(2));
        let finish = std::sync::Arc::new(std::sync::Barrier::new(2));
        let worker_started = started.clone();
        let worker_finish = finish.clone();
        let worker = std::thread::spawn(move || {
            let _permit = permit;
            worker_started.wait();
            worker_finish.wait();
        });
        started.wait();
        assert_eq!(budget.used(), 1);
        assert_eq!(
            budget.clone().try_acquire().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        finish.wait();
        worker.join().unwrap();
        assert_eq!(budget.used(), 0);
        let replacement = budget.try_acquire().unwrap();
        assert_eq!(budget.used(), 1);
        drop(replacement);
        assert!(ResourceBudget::new(0).try_acquire().is_err());
    }
}

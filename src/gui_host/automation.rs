//! Bounded, owner-scoped application calls, separate from Vivid input authority.
//!
//! Apps declare queries and commands and execute requests through `Responder::run`.
//! Work queued before revocation cannot start afterward. Work already admitted may
//! finish after cancellation; callers must not automatically retry commands.

// Rust guideline compliant 2026-10-07
use super::{AppConfig, InstanceId, ResourceBudget, ResourcePermit, validate_app_id};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fmt,
    future::Future,
    io,
    pin::Pin,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    task::{Context, Poll},
    time::{Duration, Instant},
};

/// Maximum encoded bytes in one request or response data object.
pub const MAX_DATA_BYTES: usize = 32 * 1024;
/// Maximum simultaneous calls retained by one application owner.
pub const MAX_OWNER_CALLS: usize = 4;
/// Maximum simultaneous calls retained by one host, including retiring workers.
pub const MAX_HOST_CALLS: usize = 64;
/// Maximum lifetime of a call before admission or response delivery expires.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(2);
// Metadata is static, but still bounded before exposure through a host transport.
pub(crate) const MAX_METHODS: usize = 32;
const MAX_METHOD_BYTES: usize = 64;
const MAX_DESCRIPTION_BYTES: usize = 256;

/// Bounds application-owned JSON without revealing its contents in diagnostics.
#[derive(Clone, Serialize, Default)]
#[serde(transparent)]
pub struct Data(AppConfig);
impl fmt::Debug for Data {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Data").finish_non_exhaustive()
    }
}
impl<'de> Deserialize<'de> for Data {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(Value::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}
impl Data {
    /// Validates an object using configuration depth/node bounds and a smaller byte limit.
    ///
    /// # Errors
    /// Returns `InvalidRequest` for invalid shape, depth, nodes or encoded size.
    pub fn new(value: Value) -> Result<Self, Error> {
        let data = AppConfig::new(value).map_err(|_| Error::InvalidRequest)?;
        struct Limit(usize);
        impl io::Write for Limit {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0 = self
                    .0
                    .checked_add(bytes.len())
                    .filter(|n| *n <= MAX_DATA_BYTES)
                    .ok_or_else(|| io::Error::other("application call data exceeds limit"))?;
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        serde_json::to_writer(Limit(0), &data).map_err(|_| Error::InvalidRequest)?;
        Ok(Self(data))
    }
    /// Decodes an application's schema without including values in errors.
    ///
    /// # Errors
    /// Returns `InvalidRequest` if the schema does not match.
    pub fn decode<T: DeserializeOwned>(&self) -> Result<T, Error> {
        self.0.decode().map_err(|_| Error::InvalidRequest)
    }
}

/// Declares whether a method can change application state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    /// May inspect an owned hidden instance without activating it.
    Query,
    /// Requires the active visible owner; deactivation revokes queued work.
    Command,
}
/// Describes one application-owned operation without runtime capabilities.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Method<S = &'static str> {
    /// Application-local ASCII registration name, at most 64 bytes.
    pub name: S,
    /// Host admission and revocation policy.
    pub access: Access,
    /// Public explanation, at most 256 UTF-8 bytes.
    pub description: S,
}
pub(crate) fn methods<S: AsRef<str>>(values: &[Method<S>]) -> Result<Vec<Method<&str>>, Error> {
    if values.len() > MAX_METHODS {
        return Err(Error::InvalidRequest);
    }
    let mut catalog = Vec::with_capacity(values.len());
    for value in values {
        let method = Method {
            name: value.name.as_ref(),
            access: value.access,
            description: value.description.as_ref(),
        };
        validate_method(method.name)?;
        if method.description.len() > MAX_DESCRIPTION_BYTES
            || method.description.contains('\0')
            || catalog
                .iter()
                .any(|other: &Method<&str>| other.name == method.name)
        {
            return Err(Error::InvalidRequest);
        }
        catalog.push(method);
    }
    Ok(catalog)
}
fn validate_method(name: &str) -> Result<(), Error> {
    if name.len() > MAX_METHOD_BYTES {
        return Err(Error::InvalidRequest);
    }
    validate_app_id(name).map_err(|_| Error::InvalidRequest)
}

/// Carries one validated application operation and bounded arguments.
pub struct Request {
    method: String,
    data: Data,
}
impl fmt::Debug for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Request").finish_non_exhaustive()
    }
}
impl Request {
    /// Validates an application-local operation name and retains bounded arguments.
    ///
    /// # Errors
    /// Returns `InvalidRequest` for a malformed or oversized name.
    pub fn new(method: String, data: Data) -> Result<Self, Error> {
        validate_method(&method)?;
        Ok(Self { method, data })
    }
    /// Returns the application-local operation name.
    pub fn method(&self) -> &str {
        &self.method
    }
    /// Returns the validated arguments.
    pub fn data(&self) -> &Data {
        &self.data
    }
}

/// Reports content-free, stable application-call failures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Error {
    /// The owner does not advertise this operation.
    Unsupported,
    /// Metadata, arguments or response data violate their bounds/schema.
    InvalidRequest,
    /// The runtime or its deferred response is unavailable.
    Unavailable,
    /// Host or owner admission capacity is still held.
    Busy,
    /// A command does not belong to the current active visible owner.
    Inactive,
    /// The addressed owner has retired.
    Closed,
    /// The requester abandoned the call.
    Cancelled,
    /// The call lifetime expired.
    Expired,
}
impl Error {
    /// Returns a stable machine-readable failure code.
    pub fn code(self) -> &'static str {
        match self {
            Self::Unsupported => "unsupported",
            Self::InvalidRequest => "invalid_request",
            Self::Unavailable => "unavailable",
            Self::Busy => "busy",
            Self::Inactive => "inactive",
            Self::Closed => "closed",
            Self::Cancelled => "cancelled",
            Self::Expired => "expired",
        }
    }
    fn tag(self) -> u8 {
        self as u8 + 1
    }
    fn from_tag(value: u8) -> Option<Self> {
        [
            Self::Unsupported,
            Self::InvalidRequest,
            Self::Unavailable,
            Self::Busy,
            Self::Inactive,
            Self::Closed,
            Self::Cancelled,
            Self::Expired,
        ]
        .get(value.wrapping_sub(1) as usize)
        .copied()
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "application call {}", self.code())
    }
}
impl std::error::Error for Error {}

struct Owner<T> {
    closed: AtomicBool,
    interactive: AtomicBool,
    budget: ResourceBudget,
    requests: Mutex<Vec<Weak<State<T>>>>,
}
impl<T> Owner<T> {
    fn cancel(&self, commands_only: bool, error: Error) {
        let states: Vec<_> = self
            .requests
            .lock()
            .expect("automation owner mutex poisoned")
            .iter()
            .filter_map(Weak::upgrade)
            .collect();
        for state in states {
            if !commands_only || state.command {
                state.cancel(error);
            }
        }
    }
}
struct State<T> {
    owner: Arc<Owner<T>>,
    command: bool,
    deadline: Instant,
    cancelled: AtomicU8,
    sender: async_channel::Sender<Result<T, Error>>,
    _owner_permit: ResourcePermit,
    _host_permit: ResourcePermit,
}
impl<T> State<T> {
    fn valid(&self) -> Result<(), Error> {
        if let Some(error) = Error::from_tag(self.cancelled.load(Ordering::Acquire)) {
            return Err(error);
        }
        if self.owner.closed.load(Ordering::Acquire) {
            return Err(Error::Closed);
        }
        if self.command && !self.owner.interactive.load(Ordering::Acquire) {
            return Err(Error::Inactive);
        }
        if Instant::now() >= self.deadline {
            return Err(Error::Expired);
        }
        Ok(())
    }
    fn cancel(&self, error: Error) {
        let _ =
            self.cancelled
                .compare_exchange(0, error.tag(), Ordering::AcqRel, Ordering::Acquire);
        self.sender.close();
    }
}

/// Owns completion of one call, including its admission reservations.
///
/// Move this to a bounded worker queue for deferred work. Dropping it retires
/// the worker reservation and wakes the requester with an unavailable result.
pub struct Responder<T = Data> {
    state: Arc<State<T>>,
}
impl<T> fmt::Debug for Responder<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Responder").finish_non_exhaustive()
    }
}
impl<T> Responder<T> {
    #[cfg(feature = "gui-remote")]
    pub(crate) fn admission_deadline(&self) -> Result<Instant, Error> {
        self.state.valid().map(|()| self.state.deadline)
    }

    /// Runs admitted work once and publishes a bounded response.
    ///
    /// The closure is skipped after cancellation, deactivation, expiry or close.
    /// It must not block the GUI thread. Cancellation after admission cannot
    /// undo work already begun; a late result is discarded instead.
    pub fn run(self, work: impl FnOnce() -> Result<T, Error>) {
        let result = self.state.valid().and_then(|()| {
            let result = work();
            self.state.valid().and(result)
        });
        let _ = self.state.sender.try_send(result);
        self.state.sender.close();
    }
}
impl<T> Drop for Responder<T> {
    fn drop(&mut self) {
        self.state.sender.close();
    }
}

type Receive<T> = Pin<Box<dyn Future<Output = Result<T, Error>> + Send>>;
type Timer = Pin<Box<dyn Future<Output = ()> + Send>>;
/// Awaits one response without blocking the GUI thread.
///
/// Dropping cancels queued work. Reservations remain held until both this future
/// and any deferred responder retire, preventing cancellation floods from
/// bypassing worker admission. Completed data is private application content.
pub struct Reply<T = Data> {
    id: InstanceId,
    state: Option<Arc<State<T>>>,
    receive: Option<Receive<T>>,
    timer: Option<Timer>,
}
impl<T> fmt::Debug for Reply<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Reply").finish_non_exhaustive()
    }
}
impl<T> Reply<T> {
    /// Returns the complete owner identity originally addressed by this call.
    pub fn instance_id(&self) -> InstanceId {
        self.id
    }
    fn finish(&mut self, result: Result<T, Error>) -> Poll<Result<T, Error>> {
        self.receive.take();
        self.timer.take();
        self.state.take();
        Poll::Ready(result)
    }
}
impl<T> Future for Reply<T> {
    type Output = Result<T, Error>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let Some(state) = this.state.clone() else {
            return Poll::Ready(Err(Error::Unavailable));
        };
        if let Err(error) = state.valid() {
            state.cancel(error);
            return this.finish(Err(error));
        }
        if this
            .timer
            .as_mut()
            .is_some_and(|timer| timer.as_mut().poll(cx).is_ready())
        {
            state.cancel(Error::Expired);
            return this.finish(Err(Error::Expired));
        }
        if let Some(receive) = this.receive.as_mut()
            && let Poll::Ready(result) = receive.as_mut().poll(cx)
        {
            return this.finish(state.valid().and(result));
        }
        Poll::Pending
    }
}
impl<T> Drop for Reply<T> {
    fn drop(&mut self) {
        if let Some(state) = &self.state {
            state.cancel(Error::Cancelled);
        }
    }
}

pub(super) struct Broker<T = Data> {
    owners: BTreeMap<InstanceId, Arc<Owner<T>>>,
    budget: ResourceBudget,
}
impl<T> fmt::Debug for Broker<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Broker")
            .field("owners", &self.owners.len())
            .finish_non_exhaustive()
    }
}
impl<T: Send + 'static> Broker<T> {
    pub(super) fn new() -> Self {
        Self {
            owners: BTreeMap::new(),
            budget: ResourceBudget::new(MAX_HOST_CALLS),
        }
    }
    pub(super) fn interactive(&self, id: InstanceId, value: bool) {
        if let Some(owner) = self.owners.get(&id) {
            owner.interactive.store(value, Ordering::Release);
            if !value {
                owner.cancel(true, Error::Inactive);
            }
        }
    }
    pub(super) fn close(&mut self, id: InstanceId) {
        if let Some(owner) = self.owners.remove(&id) {
            owner.closed.store(true, Ordering::Release);
            owner.cancel(false, Error::Closed);
        }
    }
    pub(super) fn admit(
        &mut self,
        id: InstanceId,
        command: bool,
        interactive: bool,
        cx: &gpui::App,
    ) -> Result<(Reply<T>, Responder<T>), Error> {
        if command && !interactive {
            return Err(Error::Inactive);
        }
        let owner = self
            .owners
            .entry(id)
            .or_insert_with(|| {
                Arc::new(Owner {
                    closed: AtomicBool::new(false),
                    interactive: AtomicBool::new(interactive),
                    budget: ResourceBudget::new(MAX_OWNER_CALLS),
                    requests: Mutex::new(Vec::new()),
                })
            })
            .clone();
        let host_permit = self.budget.try_acquire().map_err(|_| Error::Busy)?;
        let owner_permit = owner.budget.try_acquire().map_err(|_| Error::Busy)?;
        let (sender, receiver) = async_channel::bounded(1);
        let state = Arc::new(State {
            owner: owner.clone(),
            command,
            deadline: Instant::now() + CALL_TIMEOUT,
            cancelled: AtomicU8::new(0),
            sender,
            _owner_permit: owner_permit,
            _host_permit: host_permit,
        });
        let mut requests = owner
            .requests
            .lock()
            .expect("automation owner mutex poisoned");
        requests.retain(|request| request.strong_count() != 0);
        requests.push(Arc::downgrade(&state));
        drop(requests);
        let timer = cx.background_executor().timer(CALL_TIMEOUT);
        let reply = Reply {
            id,
            state: Some(state.clone()),
            receive: Some(Box::pin(async move {
                receiver.recv().await.unwrap_or(Err(Error::Unavailable))
            })),
            timer: Some(Box::pin(async move {
                timer.await;
            })),
        };
        Ok((reply, Responder { state }))
    }
}
impl<T> Broker<T> {
    pub(super) fn close_all(&mut self) {
        for (_, owner) in std::mem::take(&mut self.owners) {
            owner.closed.store(true, Ordering::Release);
            owner.cancel(false, Error::Closed);
        }
    }
}
impl<T> Drop for Broker<T> {
    fn drop(&mut self) {
        self.close_all();
    }
}

#[cfg(all(test, feature = "gui-testing"))]
mod tests;

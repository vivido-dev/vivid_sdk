# Durable GUI application state

`gui_host::AppHost::capture_state` starts an asynchronous snapshot for an exact
mounted `InstanceId`. It returns `StateReply`; the app receives a `StateResponder`.
The default `HostedApp::capture_state` invokes synchronous `save_state`. Deferred
implementations queue the responder on a bounded worker and call `run` around
capture itself. GUI callbacks must not wait for native I/O or write files.

An optional `AppState` is a JSON object limited to 256 KiB, 16 levels of depth and
1,024 nodes. Its schema belongs to the app. Never persist protocol secrets,
input grants, lease/resume authority, live IDs, handles or clipboard permissions.
`Debug` and capture errors omit values. Apps should version evolving schemas.

Capture reuses application-call cancellation with a separate budget of four
retained calls per owner and 64 per host, expiring two seconds after admission.
Hidden owners are queryable; close, replacement and host destruction revoke
capture before teardown. Dropping a reply cancels queued work. Retiring
responders and completed unconsumed replies keep their reservations. Complete
owner identities prevent stale capture from selecting a replacement. State
completion is independent of rendering and display receipts.

## Independent processes

Use `gui_remote::run_persistent(config, build, capture)` for a raster GUI app.
`build` receives restored state and validates its schema before creating its view.
`capture(&view, cx)` returns owned state on the app's GUI thread. Both callbacks
must be short and must not block. The [counter example](../examples/rust/gui_remote_counter.rs)
shares its constructor/view with standalone mode and demonstrates count restoration.
With no state environment, `build` receives `None` and no state worker starts.

A native vector producer can use `gui_remote::state::Producer::from_env` with
a closure that validates and installs its durable state. Call `Producer::poll`
between model updates to service one capture on the model's owning thread.
The worker handles framing, deadlines and replies. Keep the handle until the
producer shuts down. A standalone launch without a state endpoint returns
`None` and keeps the producer's normal defaults.

A host calls `gui_remote::state::Host::bind(endpoint, initial_state)` to obtain a
queue proxy and `Service` guard, then calls `Host::configure` on its associated
child's `Command`. Unix requires an absolute endpoint in an owner-private
directory; create it with private permissions from the start. Windows uses a
fresh local named-pipe name. The transport checks OS owners in both directions;
Windows supplies an owner ACL and refuses remote clients. The embedding
supervisor owns temporary-directory cleanup.

Every bind creates a fresh random channel owner. Initial state and captured data
travel over this channel, never arguments, environment data, stdout or a PTY.
Only discovery and public channel identity enter the child environment as
`VIVID_GUI_STATE_ENDPOINT` and `VIVID_GUI_STATE_OWNER`. Partial/malformed values
fail closed. Never reuse the proxy or endpoint for a replacement process.
`Host::is_ready` reports initial-state delivery and producer acceptance. Gate app
readiness on that handshake so incompatible state fails startup rather than
silently showing default state.

This local SDK contract is separate from Vivid 1.5 records and authentication. It
grants no input or protocol authority. A surviving process can retain this state
channel across Vivid reconnect/resume; a new process gets a fresh channel and
fresh Vivid credentials plus only saved app data.

A host opts in with `Host::configure_methods(command, true)`, which sets
`VIVID_GUI_APP_METHODS=1` for the child. Older hosts receive the existing state-only ready message. With
`Host::configure_methods(command, false)`, the child skips registration.
Only a live, registered method can receive an offer. The model loop claims it,
then the host validates the original responder before granting execution. A
declined claim consumes its serial and permits the next call. A granted command
can have changed the model even if its reply is lost.

The service admits four queued requests and one active transaction. A four-byte
big-endian length precedes version-1 JSON, capped at 256 KiB plus 1 KiB envelope
overhead before allocation. Messages bind the exact channel owner and nonzero
monotonic request ID. Startup allows fifteen seconds for native GUI creation;
capture I/O has an absolute two-second deadline that partial reads/writes cannot
extend. Unix uses nonblocking I/O, including final buffered replies after peer
EOF. Windows uses bounded cancellable pipe operations. Drop `Service` on the
native supervisor to interrupt I/O and join its worker while the runtime permit
is still charged; never drop it from a GUI callback.

Failure is not successful empty state. A valid app-level unavailable reply
consumes its ID and permits a later explicit capture. Malformed, mis-correlated,
timed-out or closed transports retire the state channel without requiring the
independent render/input transport to stop. Fail the checkpoint rather than
overwrite it with missing state. No capture or restoration retry is implicit.
Several apps' snapshots are individually consistent, not a distributed transaction.

## Compositor integration

The compositor starts captures together, awaits them asynchronously, validates
the completed configuration, then starts its existing atomic file write. Capture
or schema failure leaves the previous file intact. Manual save receipts cover
collection and disk completion; window close remains deferred during saving.
Tree/launch settings are captured at request time; each state is sampled when
its app callback runs.

The built-in raster SDK counter supports this by default. Set
`options.persistent = false` to disable it, or `true` for an external app using
`run_persistent`. Custom commands default to stateless. Local vector producers opt in with
`persistent = true` on direct or gateway launches. State belongs to the original
producer process. The sample persists its count, received-byte total, committed
text and selection. Restoring creates fresh editor and session identities.
Wayland clients still require a state contract. The compositor validates generic
envelopes locally; the producer validates its schema before declaring readiness.
Ordinary terminals still start fresh rather than persisting PTYs or presenter
sessions.

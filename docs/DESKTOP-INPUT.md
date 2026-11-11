# Desktop presenter input

`PresenterConfig::desktop_with_input` enables the production Rust presenter's
`desktop-input-v1` service. `PresenterConfig::desktop` remains view-only. This uses
the existing Vivid 1.5 desktop records and authenticated interactive lane; it adds
no wire records, registry assignments, or implicit capability forwarding.

The host first renders an active primary desktop track and acknowledges the exact
media delivery. Retaining pixels or admitting a media record does not prove
presentation. `desktop_input_target(source)` then returns an opaque target scoped
to the presenter instance, producer, context, surface, surface generation, and
host target generation. A target from another presenter or an old generation is
rejected before focus changes. Track replacement alone preserves this surface
identity and its existing input grant.

Host routing follows this sequence:

1. Create the presenter with `desktop_with_input`, authenticate the producer, and
   display its primary track. Complete actual media deliveries after presentation.
2. Obtain a `DesktopInputTarget` using the owner-qualified `SourceKey`.
3. Call `set_desktop_input_target(Some(target), allowed_classes)` for physical
   focus and local eligibility. This selects policy; it grants nothing by itself.
4. The producer sends `SET_INPUT_BINDING` with a fresh nonzero producer epoch and
   exact surface generation. The presenter intersects requested classes with host
   policy and surface capabilities. Desired input starts disabled.
5. Read `desktop_input_binding(target)` for the current complete tuple, attach it
   to each physical event, and call `send_desktop_input(target, event)`. Keep that
   original tuple through asynchronous work; never replace a stale tuple with the
   current tuple. Admission revalidates scope, generation, classes, and watchdog.
6. Call `set_desktop_input_target(None, 0)` before the pane loses eligibility.

The fixed held-state model suppresses duplicate physical presses and unbalanced
releases. Pointer coordinates and axes are validated by the protocol decoders.
Event rate is capped by the negotiated contract and at most 1,000 events/second
per session. The existing ordered writer permits 64 queued records. Rate or
writer admission failure revokes the grant and closes only that owner's lane;
the producer's lane-loss/watchdog path must release held input.

A separate bounded maintenance worker renews grants before half their watchdog
interval, independently of media-flow credit. Focus/policy changes, lane loss,
target or surface changes, suspension, authority cleanup, and shutdown revoke
input. Returning eligibility never restores an old epoch. A replacement lane
requires a greater lane generation and a fresh input binding. Grant generations
and the latest producer epoch survive logical-session resume; transport keys
rotate independently. Reconnect callers must advance the lane generation rather
than restarting it at one within the same logical session.

Surface changes revoke input before publishing new geometry. Queued media
deliveries retain their admitted surface and channel generations. An old
acknowledgement or retained replay cannot establish presentation readiness for a
new generation, and old-channel completion cannot credit a replacement channel.

The terminating presenter currently grants input only to root-authenticated
pane principals. Delegated sessions may negotiate the profile but are denied
grants until their explicit context input-class policy is represented. No child
input authority is inferred from profile negotiation or a resource contract.

An input target exposes its captured `logical_size`, producer `semantic_generation`,
and host `target_generation` for host placement policy. Semantic generation remains
producer-owned; these generations are not universally interchangeable. The SDK
remote GUI convention acknowledges a whole-canvas target by matching dimensions
and publishing the target generation as its mapping generation. Its compositor
adapter withholds input until that acknowledgement is presented, including a
resize that returns to the previous dimensions.

Production GUI producers can use `gui_remote` behind `gui-remote`; it owns the
final injection barrier and bounded release path. Custom injectors should apply
`InputBindingGuard::handle_bound_at` at actual receipt time so the initial grant
expires even if no renewal ever arrives. Revocation/reset carries a greater
presenter grant generation for the exact old epoch/context/surface generation.

Dedicated runtime supervisors can call `VirtualVivid::shutdown` to revoke all
owned authority, close authenticated channels, disconnect media waiters, and stop
admission. It is idempotent. Wait for `active_connection_count() == 0` before
returning a runtime permit; incomplete handshakes retain their bounded deadline.

`disconnect_desktop_session(target)` performs owner-scoped unclean transport loss
while retaining the principal. It rejects stale/foreign targets and immediately
retires a non-resumable root owner's resources. Leased sessions retain their
existing suspension policy. The remote GUI driver reconnects root sessions with
fresh authority, frame readiness, and input lanes while retaining its view.

The producer must still decode events with current surface dimensions and perform
the final injection-boundary gate. `Revoked`, `Reset`, `LaneClosed`, queue overflow,
or watchdog expiry must stop injection and release held state on the original
target. Presenter admission alone does not make an arbitrary asynchronous injector
conformant. This service carries physical HID keys and pointer transitions; it
does not add IME text, clipboard, accessibility, or vector-overlay rendering.

The production socket regressions in
[`desktop_input_presenter.rs`](../tests/desktop_input_presenter.rs) cover reused
owner IDs, forged lane authentication, foreign presenter tokens, stale epochs,
class narrowing, pointer bounds, lane replacement, renewal under blocked bulk
flow, rate exhaustion, live geometry invalidation, stale media acknowledgements,
and track replacement. They also verify principal-preserving disconnect, stale
target rejection, and an unrelated owner reusing local IDs. The production GUI
driver and compositor adapter are documented in [GUI.md](GUI.md); physical OS
input and platform service acceptance remain separate work.

# vivid_sdk

`vivid_sdk` is the reusable full-duplex Vivid 1.1 producer client shared by Vivi, Vvrd, and Veston.
It owns authentication, control dispatch, heartbeat handling, reply correlation, text anchors,
scene transactions, authoritative observability, cancellation-safe waits, transport attachment,
and credit-aware media senders.

The crate deliberately separates each source's `MediaSender` from the control session. A blocked
video credit wait therefore cannot prevent audio delivery, display/visibility processing, `PING`
replies, or scene updates. Bulk endpoint fallback is attempted before `ATTACH_CHANNEL`. If the
attachment write itself fails ambiguously, the SDK queries authoritative source state; it retries
on the primary endpoint only when attachment state proves the ticket was not consumed. An attached
or closed ticket is never replayed.

`play_at` returns when `PLAY` is admitted. Call `wait_until_playing` for the authoritative playback
transition, or use `play_and_wait_until_playing` when both steps are intentionally required.
`begin_wait_source` returns a handle that sends `CANCEL_WAIT` when cancelled or dropped. Query and
event APIs maintain scene and source revisions beside `DisplayState`, and scene pagination is
explicitly caller-bounded.

Version fallback is disabled by default. Callers may explicitly enable one retry after a typed
version rejection; the SDK then opens a fresh connection only for a fully implemented reported
version and never carries a media ticket across attempts.

No token-bearing configuration implements `Debug`. Applications should continue to keep
`VIVID_TOKEN`, tickets, and derived anchor material out of arguments, logs, and child environments.

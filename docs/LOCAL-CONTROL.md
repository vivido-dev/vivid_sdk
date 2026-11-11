# Native local control transport

Enable `local-control` for `vivid_sdk::local_control::{LocalListener, LocalStream}`.
The optional `local-control/` support crate contains the native implementation
migrated from Vivido's `src/polling/transport.rs`, with its Apache-2.0 license.
It depends on libc on Unix and windows-sys on Windows, independently of GPUI,
renderers, presenter roles and the Vivid wire codec. Default SDK builds do not
enable it. Vivido now consumes the same implementation through the SDK.

`LocalListener::bind` refuses an existing endpoint. Unix sockets have mode 0600
and compare peer credentials with the effective user on both acceptance and
connection. Applications must protect the containing directory during creation;
the compositor requires owner-only directory permissions. Windows named pipes
use the current user's security descriptor, remote-client rejection, process
owner checks, and first-instance protection.

`accept_timeout` bounds an accept, while streams support explicit read/write
timeouts, cloning and shutdown. Deadlines at the application message layer must
remain absolute across partial reads and writes. Windows overlapped operations
retain their event, operation storage and borrowed buffers through completion
or cancellation; successful completion racing a timeout preserves its byte
count. Shutdown serializes with operation admission, so a clone cannot start
new I/O after cancellation. `pair` creates connected streams for local work and
transport tests.

Operation storage stays live until native completion, following Microsoft's
[CancelIoEx lifetime requirements](https://learn.microsoft.com/windows/win32/api/ioapiset/nf-ioapiset-cancelioex).
Requesting cancellation alone does not establish completion.

The existing Windows `set_nonblocking` methods accept compatibility hints for
dedicated blocking threads. Use explicit timeout methods for portable bounded
waits. Unix follows its native nonblocking socket behavior, and accepted
connections may need restoring to blocking mode before a dedicated reader uses
timeouts. Endpoint cleanup remains the application's responsibility; the
compositor checks the original socket inode before unlinking it.

These streams carry the application's own control framing and policy. Vivid
sessions continue using their independent authentication, profiles and authority.
Opening a local control connection supplies no Vivid session, input binding,
lease, media channel or display receipt.

The native crate's real-stream tests cover accepted ownership, byte transfer,
private socket mode, existing-endpoint refusal, reusable timed accepts, read
timeout and clone shutdown. Run them from `local-control/` with the normal
format/test/Clippy checks. Windows-specific code and tests also cross-compile;
native Windows execution remains required before claiming platform acceptance.

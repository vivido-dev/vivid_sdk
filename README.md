# vivid_sdk

`vivid_sdk` is the reusable full-duplex Vivid 1.1 producer client shared by Vivi and Veston.
It owns authentication, control dispatch, heartbeat handling, reply correlation, text anchors,
scene transactions, source state, transport attachment, and credit-aware media senders.

The crate deliberately separates each source's `MediaSender` from the control session. A blocked
video credit wait therefore cannot prevent audio delivery, display/visibility processing, `PING`
replies, or scene updates. Bulk endpoint fallback is attempted only while opening the connection,
before `ATTACH_CHANNEL`; an attached channel is never replayed on the primary endpoint.

No token-bearing configuration implements `Debug`. Applications should continue to keep
`VIVID_TOKEN`, tickets, and derived anchor material out of arguments, logs, and child environments.

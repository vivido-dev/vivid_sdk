# Session reconciliation

`Session::query_session_page(&vivid_protocol::session_status::Query)` returns a
validated, bounded core `SESSION_STATUS` page. The terminating SDK presenter and
Vivido use the same codec, cursor handling, page sizing and revision clock.
`Session::query_session()` remains an unparsed initial-page query.

Start with `Query::default()`. Its entry ceiling is 64; callers may request from
one through 256. A response may contain fewer entries to fit the negotiated
control-body limit. Pass its opaque cursor in the next query. An absent cursor
means the snapshot is complete. Never combine pages with different session
revisions, scene revisions, target generations or resume generations. Bound the
number of pages, total collected summaries, retry count and elapsed time according
to the application's resource contract.

A stale or foreign cursor returns `PRECONDITION_FAILED`; discard the partial
snapshot and restart within that budget. Malformed request fields are
`BAD_MESSAGE`. If one entry cannot fit the negotiated body, the presenter returns
`LIMIT_EXCEEDED` instead of dropping entries or producing an endless continuation.
No cursor contains authentication material or selects another owner's objects.

Summaries are flat, ordered records for contexts, surfaces, tracks and active
slots. Every track and slot includes its full context/surface identity. A root
context appears even when no surface exists. Scene revision and aggregate
surface/track/node counts accompany every page. Query individual objects and the
scene to resolve their complete state; summary receipt is not a mutation receipt.
Deletion/recreation advances the session clock even if the final summaries look
identical. Changes to reported asynchronous track, input and target state invalidate earlier
cursors. Another owner's changes do not.

After authenticated lease resume, the same owner retains its objects but advances
the resume generation and session revision. Input starts disabled. Compare the
snapshot with retained handles before adopting objects, advancing detached
channels and sending fresh recovery media. Old posters never establish current
presentation or input authority. Both the current presentation milestone and a
fresh input grant remain required.

The production GUI process backend currently reconnects root sessions. This
reconciliation endpoint is the prerequisite for its later leased-session resume
path; it does not enable that path by itself. Root sessions remain non-resumable.
The core wire fields are specified in
[core §10](../../vivid_protocol/vivid-protocol-1.5-core.md#10-observability).

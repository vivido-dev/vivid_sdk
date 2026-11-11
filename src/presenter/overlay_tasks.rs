//! Bounded host-service work that leaves the authenticated control reader live.

// Rust guideline compliant 2026-10-07
use super::*;

// Host callbacks already have bounded reply deadlines. These limits bound
// their waiting threads and prevent one producer from occupying the whole pool.
const MAX_JOBS: usize = 64;
const MAX_OWNER_JOBS: usize = 8;

struct Job {
    state: Arc<Mutex<State>>,
    key: (u64, u64, u64),
}
impl Drop for Job {
    fn drop(&mut self) {
        lock(&self.state)
            .overlay_relay
            .active_jobs
            .remove(&self.key);
    }
}

pub(super) fn defer(
    shared: &Arc<Mutex<State>>,
    session: u64,
    writer: &Arc<Writer>,
    record: &Record,
) -> Result<bool, ControlError> {
    if !matches!(
        record.record_type,
        messages::MEASURE_OVERLAY_TEXT
            | messages::MEASURE_OVERLAY_TEXT_BATCH
            | messages::RELEASE_OVERLAY_TEXT_LAYOUTS
            | messages::SET_OVERLAY_EDITOR
            | messages::SET_OVERLAY_TEXT_CONTEXT
            | messages::SET_OVERLAY_CLIPBOARD
            | messages::SET_OVERLAY_SEMANTICS
    ) {
        return Ok(false);
    }
    let mut state = lock(shared);
    if !state.overlay_relay.enabled {
        return Ok(false);
    }
    if state.overlay_relay.local_host_claimed && !state.overlay_relay.local_host_available {
        return Err(ControlError::state("native overlay host is unavailable"));
    }
    if record.body.len() > super::super::overlay_relay::MAX_OVERLAY_HOST_BODY {
        return Err(ControlError::bad(
            "overlay host request exceeds transport budget",
        ));
    }
    let envelope = messages::decode_control(&record.body)
        .map_err(|_| ControlError::bad("invalid strict control envelope"))?;
    envelope
        .validate_request()
        .map_err(|_| ControlError::bad("invalid request identity"))?;
    let runtime = state
        .sessions
        .get(&session)
        .ok_or_else(|| ControlError::missing("session is absent"))?;
    if runtime.closed || !Arc::ptr_eq(&runtime.writer, writer) {
        return Err(ControlError::state(
            "control connection is no longer current",
        ));
    }
    let ceiling = runtime
        .lease
        .and_then(|key| state.leases.get(&key))
        .map(|lease| lease.contract.get(Resource::PendingRequests))
        .or_else(|| {
            state
                .config
                .resource_contract
                .as_ref()
                .map(|contract| contract.get(Resource::PendingRequests))
        })
        .unwrap_or(256)
        .min(MAX_OWNER_JOBS as u64);
    let key = (session, runtime.resume_generation, envelope.request_id);
    let jobs = &mut state.overlay_relay.active_jobs;
    if jobs.len() >= MAX_JOBS
        || jobs.iter().filter(|key| key.0 == session).count() as u64 >= ceiling
    {
        return Err(ControlError {
            code: messages::ERROR_LIMIT_EXCEEDED,
            message: "overlay host worker capacity exhausted",
        });
    }
    if !jobs.insert(key) {
        return Err(ControlError::state(
            "overlay host request is already pending",
        ));
    }
    let job = Job {
        state: shared.clone(),
        key,
    };
    drop(state);
    let record = Record {
        record_type: record.record_type,
        flags: record.flags,
        object_id: record.object_id,
        sequence: record.sequence,
        body: record.body.clone(),
    };
    let writer = writer.clone();
    thread::Builder::new()
        .name("vivid-overlay-host".into())
        .spawn(move || {
            let result = dispatch_control_for_connection(&job.state, session, &record, &writer);
            lock(&job.state).wait_changed.notify_all();
            let result = match result {
                Ok(Some((kind, object, body))) => writer.write_record(kind, object, &body),
                Ok(None) => Ok(0),
                Err(error) => protocol_error(key.2, error.code, false, error.message)
                    .and_then(|body| writer.write_record(messages::ERROR, record.object_id, &body)),
            };
            if result.is_err() {
                writer.close();
            }
        })
        .map_err(|_| ControlError::state("could not start overlay host worker"))?;
    Ok(true)
}

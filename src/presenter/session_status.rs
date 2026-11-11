//! Owner-scoped reconciliation snapshots for the terminating presenter.

// Rust guideline compliant 2026-10-07
use super::*;
use vivid_protocol::session_status::{Kind, Page, PageError, Query, Summary};

fn snapshot(state: &State, owner: u64) -> Result<Page, ControlError> {
    let runtime = state
        .sessions
        .get(&owner)
        .ok_or_else(|| ControlError::missing("session does not exist"))?;
    let mut entries = vec![Summary {
        kind: Kind::Context,
        context_id: runtime.root_context,
        surface_id: 0,
        object_id: 0,
        revision: 1,
        generation: 0,
        state: 1,
        detail: 0,
    }];
    let mut usage = [0_u64; 3];
    for (key, surface) in &state.surfaces {
        if key.session != owner {
            continue;
        }
        usage[0] += 1;
        entries.push(Summary {
            kind: Kind::Surface,
            context_id: key.context,
            surface_id: key.surface,
            object_id: 0,
            revision: surface.state.revision.get(),
            generation: surface.state.generation.get(),
            state: 1,
            detail: 0,
        });
        for (slot, track) in &surface.active_slots {
            entries.push(Summary {
                kind: Kind::Slot,
                context_id: key.context,
                surface_id: key.surface,
                object_id: *slot,
                revision: surface.state.revision.get(),
                generation: surface.state.generation.get(),
                state: 0,
                detail: *track,
            });
        }
    }
    for (key, track) in &state.tracks {
        if key.surface.session != owner {
            continue;
        }
        usage[1] += 1;
        entries.push(Summary {
            kind: Kind::Track,
            context_id: key.surface.context,
            surface_id: key.surface.surface,
            object_id: key.track,
            revision: track.state.revision.get(),
            generation: track.state.channel_generation.get(),
            state: if track.state.lost { 6 } else { 1 },
            detail: track.state.milestones,
        });
    }
    usage[2] = u64::try_from(
        state
            .nodes
            .keys()
            .filter(|key| key.session == owner)
            .count(),
    )
    .map_err(|_| ControlError::state("session node count overflow"))?;
    entries.sort_unstable();
    Ok(Page {
        session_revision: runtime.status_revision.get(),
        scene_revision: runtime.scene_revision.get(),
        target_generation: runtime.target_generation.get(),
        establishment_state: if runtime.closed { 3 } else { 1 },
        resume_generation: runtime.resume_generation,
        input_disabled: runtime.desktop_input.disabled(),
        entries,
        cursor: None,
        usage,
    })
}

pub(super) fn query(
    state: &mut State,
    owner: u64,
    object: u64,
    request: u64,
    value: &Value,
) -> Result<messages::PayloadMap, ControlError> {
    let query = Query::decode(object, value)
        .map_err(|_| ControlError::bad("invalid session status query"))?;
    let mut snapshot = snapshot(state, owner)?;
    let runtime = state.sessions.get_mut(&owner).expect("checked session");
    snapshot.session_revision = runtime
        .status_revision
        .observe(&snapshot)
        .map_err(|_| ControlError::state("session status revision exhausted or invalid"))?;
    let identity = SessionIdentity::new(state.presenter, owner)
        .map_err(|_| ControlError::state("invalid session identity"))?;
    let page = vivid_protocol::session_status::paginate(
        &snapshot,
        identity,
        &query,
        request,
        runtime.control_body_limit,
    )
    .map_err(|error| ControlError {
        code: match error {
            PageError::InvalidCursor | PageError::RevisionChanged => {
                messages::ERROR_PRECONDITION_FAILED
            }
            PageError::BodyLimit => messages::ERROR_LIMIT_EXCEEDED,
            PageError::InvalidSnapshot => messages::ERROR_BAD_STATE,
        },
        message: "session status cannot continue at the requested revision or body limit",
    })?;
    page.payload()
        .map_err(|_| ControlError::state("invalid session status page"))
}

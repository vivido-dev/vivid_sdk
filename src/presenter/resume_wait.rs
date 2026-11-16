//! Coordinate authenticated resume with preceding control retirement.

// Rust guideline compliant 2026-10-07
use super::*;

pub(super) fn retire(
    shared: &Arc<Mutex<State>>,
    changed: &Arc<Condvar>,
    preface: &[u8; 16],
    hello: &Hello,
    deadline: Instant,
) -> io::Result<()> {
    let HelloAuthentication::Resume {
        context_id,
        lease_id,
        session_id,
        resume_generation,
        ..
    } = &hello.authentication
    else {
        return Ok(());
    };
    let key = (*context_id, *lease_id);
    let active = |state: &State| {
        state.leases.get(&key).is_some_and(|lease| {
            lease.active_session == Some(*session_id)
                && lease.machine.state() == LeaseState::Active
                && lease.machine.resume_generation().get() == *resume_generation
        }) && state.sessions.get(session_id).is_some_and(|session| {
            !session.closed && session.resume_generation == *resume_generation
        })
    };
    let mut state = lock(shared);
    if !active(&state) {
        return Ok(());
    }
    let session = state
        .sessions
        .get(session_id)
        .expect("matched active session");
    if !hello.verify_resume(&auth::ResumeKey::new(*session.resume_key.expose()), preface)? {
        // Preserve the ordinary generic authentication rejection. Invalid
        // proofs never retain a parked handshake or affect the active owner.
        return Ok(());
    }
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "resume awaited preceding control retirement",
            ));
        }
        if !active(&state) {
            break;
        }
        // Connection admission already bounds these handlers. Release the
        // presenter lock so EOF retirement and unrelated owners can progress.
        // This neither cancels the old transport nor authorizes takeover.
        #[cfg(test)]
        {
            state.waiting_resumes += 1;
        }
        let (next, _) = changed
            .wait_timeout(state, remaining)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state = next;
        #[cfg(test)]
        {
            state.waiting_resumes -= 1;
        }
    }
    // Normal establishment must still prove suspended state, grace, identity,
    // generation, proof and profile equality under its own state lock. Another
    // contender can win before that check; no authority was reserved here.
    Ok(())
}

//! Reuses committed window scenes while reserving current peer submissions.

// Rust guideline compliant 2026-10-07
use super::*;

impl NativeVectorHost {
    /// Prepares fresh peers while retaining selected windows' last displayed scenes.
    ///
    /// `previous` must be this host's latest admitted frame for `pane`. Select
    /// window identities from [`VectorFrame::drawings`]. Each
    /// selected window keeps its exact committed scene, resources and receipt;
    /// pending newer bytes are neither compiled nor acknowledged. Other windows
    /// use their current accepted scenes in the original composition order.
    /// Render the complete returned composition, then call [`Self::presented`]
    /// only after actual display. Preparation grants no input or display proof.
    ///
    /// This permits one additional bounded frame while a diagnostic reservation
    /// and the last displayed frame remain live: at most three for this pane,
    /// within the unchanged host-wide 64-frame ceiling. Ordinary [`Self::prepare`]
    /// keeps its two-frame limit. Drop obsolete frames promptly.
    ///
    /// # Errors
    /// Rejects foreign, unadmitted or stale previous frames, duplicate or absent
    /// windows, changed topology/placement, target/visibility, input generation,
    /// owner resume, surface/channel/writer or committed scene, and ordinary
    /// preparation errors including invalid scale and exhausted frame capacity.
    pub fn prepare_retaining(
        &mut self,
        pane: PaneId,
        scale: f64,
        previous: &VectorFrame,
        windows: &[SurfaceIdentity],
    ) -> io::Result<VectorFrame> {
        validate(
            self,
            &lock(&self.presenter.state),
            pane,
            scale,
            previous,
            windows,
        )?;
        self.prepare_inner(pane, scale, Some((previous, windows)))
    }
}

pub(super) fn validate(
    host: &NativeVectorHost,
    state: &State,
    pane: PaneId,
    scale: f64,
    previous: &VectorFrame,
    windows: &[SurfaceIdentity],
) -> io::Result<()> {
    let frame = &previous.0;
    if frame.presenter != state.presenter
        || !Weak::ptr_eq(&frame.host, &Arc::downgrade(&host.alive))
        || !host.alive.load(Ordering::Acquire)
        || state.overlays.native.is_none()
        || frame.pane != pane
        || host.presented.get(&pane) != Some(&frame.serial)
        || windows.len() > frame.entries.len()
    {
        return Err(invalid("foreign or unadmitted retained native frame"));
    }
    let mut owners = overlay_sessions_for_pane(state, pane);
    owners.sort_unstable();
    if !scale.is_finite()
        || scale <= 0.
        || scale > 8.
        || state.native_pane_hidden(pane)
        || state.metrics.get(&pane).is_none_or(|metrics| {
            metrics.generation != frame.target
                || (f64::from(metrics.viewport_width) * scale).ceil()
                    != f64::from(frame.dimensions.0)
                || (f64::from(metrics.viewport_height) * scale).ceil()
                    != f64::from(frame.dimensions.1)
        })
        || state.native_visibility.get(&pane).copied().unwrap_or(0) != frame.visibility
        || owners != frame.owners
        || frame.resumes.iter().any(|(owner, generation)| {
            state
                .sessions
                .get(owner)
                .is_none_or(|session| session.resume_generation != *generation)
        })
        || frame
            .input_context
            .iter()
            .any(|(owner, generation)| state.overlays.input_generation(*owner) != *generation)
        || state.overlays.native_stamp(&owners) != frame.stamp
    {
        return Err(invalid("stale retained native composition"));
    }
    for (index, window) in windows.iter().enumerate() {
        let Some((entry, drawing)) = frame
            .entries
            .iter()
            .zip(&frame.drawings)
            .find(|(_, drawing)| drawing.window == *window)
        else {
            return Err(invalid("retained window is absent from its frame"));
        };
        if windows[..index].contains(window)
            || drawing.scale != scale
            || !current(state, entry, drawing)
            || state
                .overlays
                .native
                .as_ref()
                .and_then(|native| native.committed.get(&entry.window))
                != Some(&drawing.submission)
        {
            return Err(invalid("stale or duplicate retained native window"));
        }
    }
    Ok(())
}

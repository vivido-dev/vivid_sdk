//! Exact-source refusal after the native host isolates an unrenderable drawing.

// Rust guideline compliant 2026-10-07
use super::*;

impl NativeVectorHost {
    /// Closes one current window whose prepared drawing cannot be rendered.
    ///
    /// Call only after isolating failure to this drawing. Failure of an entire
    /// mixed composition does not identify an offending window. This method
    /// grants no display receipt and does not acknowledge any peer drawing.
    /// Descendants of this window close under the ordinary window policy.
    ///
    /// Returns `false` if the drawing is absent, already displayed or stale.
    /// New accepted content, track/writer replacement, resume, placement and
    /// target/visibility changes fence a delayed refusal. Other owners may
    /// change independently without preventing refusal of this exact source.
    ///
    /// # Errors
    /// Rejects a foreign frame or a detached native host.
    pub fn reject_drawing(
        &mut self,
        frame: &VectorFrame,
        window: SurfaceIdentity,
    ) -> io::Result<bool> {
        let frame = &frame.0;
        let mut state = lock(&self.presenter.state);
        if frame.presenter != state.presenter
            || !Weak::ptr_eq(&frame.host, &Arc::downgrade(&self.alive))
            || state.overlays.native.is_none()
        {
            return Err(invalid("foreign native drawing refusal"));
        }
        let Some((entry, drawing)) = frame
            .entries
            .iter()
            .zip(&frame.drawings)
            .find(|(_, drawing)| drawing.window == window)
        else {
            return Ok(false);
        };
        let track = TrackKey {
            surface: SurfaceKey {
                session: entry.source.producer,
                context: entry.source.context,
                surface: entry.source.surface,
            },
            track: entry.source.track,
        };
        if self
            .presented
            .get(&frame.pane)
            .is_some_and(|serial| *serial >= frame.serial)
            || state
                .metrics
                .get(&frame.pane)
                .is_none_or(|metrics| metrics.generation != frame.target)
            || state
                .native_visibility
                .get(&frame.pane)
                .copied()
                .unwrap_or(0)
                != frame.visibility
            || !overlay_sessions_for_pane(&state, frame.pane).contains(&entry.source.producer)
            || !current(&state, entry, drawing)
            // Display reservations may admit older accepted content. Refusal
            // must instead match the latest bytes, so it cannot close a source
            // that has already replaced the failed scene with a healthy one.
            || !state
                .tracks
                .get(&track)
                .and_then(|track| track.retained.as_ref())
                .is_some_and(|body| Arc::ptr_eq(body, &entry.body))
        {
            return Ok(false);
        }
        self.cache.remove(&entry.source);
        self.images.retain(|key, _| key.0 != entry.source);
        state.overlays.close_native(entry.window);
        flush_overlay_lane(&mut state, entry.source.producer);
        state.wake_waits();
        advance_projection(&mut state);
        Ok(true)
    }
}

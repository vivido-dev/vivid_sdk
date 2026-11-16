//! Atomic retained replay cuts with presenter- and incarnation-scoped source fences.

// Rust guideline compliant 2026-10-07
use super::*;

#[derive(Debug)]
pub(super) struct Scope {
    presenter: PresenterInstanceId,
    surface: SurfaceGeneration,
    channel: ChannelGeneration,
    resume: u64,
    reset: u64,
    track: std::sync::Weak<()>,
}
pub(super) type Scopes = HashMap<SourceKey, Scope>;

/// Original source incarnations represented by an atomic retained recovery cut.
///
/// This lightweight fence holds no retained media. Keep it while replaying the
/// accompanying projection, and discard the projection once replay is planned.
#[derive(Debug)]
pub struct BridgeRecoveryFence {
    scopes: Scopes,
}

impl VirtualVivid {
    /// Prepares retained replay and releases selected sources' superseded ingress atomically.
    ///
    /// Select the full source keys whose outer attachments need rebuilding. Other
    /// sources keep their pending deliveries, including owners that reuse local IDs.
    /// Only retained raster, image and vector sources participate. Audio/video
    /// deliveries are left intact; their decoder recovery needs a separate policy.
    /// This has the same projection activation semantics as
    /// [`Self::prepare_projection_snapshot_with_viewports`].
    ///
    /// Call [`Self::bridge_recovery_source_is_current`] before handing retained
    /// records to an attachment. Finish each source's retained records before its
    /// later live events. This cut does not acknowledge physical presentation or
    /// activate the projection. Retained content remains subject to presenter budgets.
    pub fn prepare_bridge_recovery_snapshot_with_viewports(
        &self,
        panes: &HashSet<PaneId>,
        viewport_offsets: &HashMap<PaneId, usize>,
        sources: &HashSet<SourceKey>,
    ) -> (ProjectionSnapshot, BridgeRecoveryFence) {
        let mut scopes = Scopes::new();
        let projection = self.projection_snapshot_inner(
            panes,
            viewport_offsets,
            Some(false),
            Some((sources, &mut scopes)),
        );
        (projection, BridgeRecoveryFence { scopes })
    }

    /// Checks the exact presenter, source, surface, channel, resume and decoder incarnation.
    ///
    /// New media accepted on the same incarnation does not invalidate retained
    /// replay. Retired owners, channel replacement and authenticated resume do.
    /// This is a freshness check; it grants no media or input authority.
    pub fn bridge_recovery_source_is_current(
        &self,
        recovery: &BridgeRecoveryFence,
        source: SourceKey,
    ) -> bool {
        let Some(scope) = recovery.scopes.get(&source) else {
            return false;
        };
        let state = lock(&self.state);
        let key = inner_track_key(source);
        scope.presenter == state.presenter
            && state
                .sessions
                .get(&source.producer)
                .is_some_and(|session| !session.closed && session.resume_generation == scope.resume)
            && state
                .surfaces
                .get(&key.surface)
                .is_some_and(|surface| surface.state.generation == scope.surface)
            && state.tracks.get(&key).is_some_and(|track| {
                !track.state.lost
                    && std::sync::Weak::ptr_eq(&scope.track, &Arc::downgrade(&track.incarnation))
                    && track.state.channel_generation == scope.channel
                    && track.decoder_reset_serial == scope.reset
            })
    }
}

pub(super) fn retire(
    state: &mut State,
    snapshot_sources: &[SnapshotSource],
    selected: &HashSet<SourceKey>,
    scopes: &mut Scopes,
) -> bool {
    for source in snapshot_sources.iter().filter(|source| {
        selected.contains(&source.key)
            && matches!(
                source.descriptor,
                SourceDescriptor::Raster(_)
                    | SourceDescriptor::Image(_)
                    | SourceDescriptor::VectorScene(_)
            )
    }) {
        let key = inner_track_key(source.key);
        scopes.insert(
            source.key,
            Scope {
                presenter: state.presenter,
                surface: state.surfaces[&key.surface].state.generation,
                channel: state.tracks[&key].state.channel_generation,
                resume: state.sessions[&source.key.producer].resume_generation,
                reset: source.decoder_reset_serial,
                track: Arc::downgrade(&state.tracks[&key].incarnation),
            },
        );
    }
    let retired = state
        .deliveries
        .iter()
        .filter_map(|(id, delivery)| {
            scopes
                .contains_key(&bridge_track_key(delivery.track))
                .then_some(*id)
        })
        .collect::<Vec<_>>();
    for id in &retired {
        let delivery = state
            .deliveries
            .remove(id)
            .expect("selected delivery under lock");
        release_delivery_allowance(state, &delivery);
    }
    !retired.is_empty()
}

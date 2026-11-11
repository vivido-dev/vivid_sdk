//! Retained operations for the remote backend's single fixed canvas and node.

// Rust guideline compliant 2026-10-07
use super::*;
use crate::{RequestMetadata, Session, Surface, SurfaceDefinition, Track};
use vivid_protocol::{
    session_status::{Kind, Page, Query},
    track::{KindConfiguration, TrackConfiguration},
};

#[cfg(all(test, feature = "presenter"))]
mod tests;

// One canvas, one primary slot, two resize-handoff tracks and their root context.
const MAX_SUMMARIES: usize = 8;
const MAX_PAGES: usize = 8;
const MAX_TRACKS: usize = 2;

struct SurfacePlan {
    definition: SurfaceDefinition,
    metadata: RequestMetadata,
    handle: Option<Surface>,
}
struct TrackPlan {
    configuration: TrackConfiguration,
    metadata: RequestMetadata,
    handle: Option<Track>,
    retiring: bool,
    advance: bool,
}
#[derive(Default)]
pub(super) struct Graph {
    surface: Option<SurfacePlan>,
    tracks: Vec<TrackPlan>,
    node: bool,
    active: Option<u64>,
    highwater: u64,
}
fn metadata() -> io::Result<RequestMetadata> {
    let mut key = [0; vivid_protocol::messages::IDEMPOTENCY_KEY_BYTES];
    crate::random_bytes(&mut key)?;
    Ok(RequestMetadata {
        idempotency_key: Some(key),
        ..Default::default()
    })
}
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

impl Graph {
    pub(super) fn remember_allocator(&mut self, session: &Session) {
        self.highwater = self
            .highwater
            .max(session.next_id.load(Ordering::Acquire).saturating_sub(1));
    }
    pub(super) fn surface(
        &mut self,
        session: &mut Session,
        definition: SurfaceDefinition,
    ) -> io::Result<Surface> {
        if self.surface.is_none() {
            self.surface = Some(SurfacePlan {
                definition,
                metadata: metadata()?,
                handle: None,
            });
        }
        let plan = self.surface.as_mut().expect("prepared surface");
        if plan.handle.is_none() {
            plan.handle = Some(session.create_surface(plan.definition.clone(), &plan.metadata)?);
        }
        Ok(plan.handle.as_ref().expect("created surface").clone())
    }
    pub(super) fn node(
        &mut self,
        session: &mut Session,
        node: &vivid_protocol::scene::SceneNode,
    ) -> io::Result<()> {
        if !self.node {
            session.create_node(node, &RequestMetadata::default())?;
            self.node = true;
        }
        Ok(())
    }
    pub(super) fn create_track(
        &mut self,
        session: &mut Session,
        configuration: TrackConfiguration,
    ) -> io::Result<Track> {
        if self.tracks.len() >= MAX_TRACKS {
            return Err(invalid("remote GUI retained track bound exceeded"));
        }
        self.tracks.push(TrackPlan {
            configuration,
            metadata: metadata()?,
            handle: None,
            retiring: false,
            advance: false,
        });
        let plan = self.tracks.last_mut().expect("prepared track");
        let track = session.create_track(plan.configuration.clone(), &plan.metadata)?;
        plan.handle = Some(track.clone());
        Ok(track)
    }
    pub(super) fn retire(&mut self, session: &mut Session, track: &Track) -> io::Result<()> {
        let plan = self
            .tracks
            .iter_mut()
            .find(|plan| plan.configuration.track_id == track.id())
            .ok_or_else(|| invalid("remote GUI retirement lost its track"))?;
        plan.retiring = true;
        session.destroy_track(track, &RequestMetadata::default())?;
        self.tracks
            .retain(|plan| plan.configuration.track_id != track.id());
        if self.active == Some(track.id()) {
            self.active = None;
        }
        // Destroying an active track also removes its slot and advances the
        // canvas revision. The next activation must use that authoritative value.
        if let Some(surface) = self.surface.as_ref().and_then(|plan| plan.handle.as_ref()) {
            session.query_surface(surface)?;
        }
        Ok(())
    }
    pub(super) fn reusable(
        &mut self,
        session: &mut Session,
        dimensions: (u32, u32),
    ) -> io::Result<Option<Track>> {
        let Some(plan) = self.tracks.iter_mut().rev().find(|plan| {
            !plan.retiring
                && matches!(&plan.configuration.kind,
            KindConfiguration::Raster(raster) if (raster.width, raster.height) == dimensions)
        }) else {
            return Ok(None);
        };
        let track = plan
            .handle
            .as_ref()
            .ok_or_else(|| invalid("unreconciled remote GUI track"))?;
        if plan.advance {
            session.advance_channel(track, 3, &RequestMetadata::default())?;
            plan.advance = false;
        }
        Ok(Some(track.clone()))
    }
    pub(super) fn other(&self, selected: u64) -> Option<Track> {
        self.tracks
            .iter()
            .find(|plan| plan.configuration.track_id != selected)
            .and_then(|plan| plan.handle.clone())
    }
    pub(super) fn make_room(&mut self, session: &mut Session) -> io::Result<()> {
        if self.tracks.len() >= MAX_TRACKS {
            let track = self
                .tracks
                .iter()
                .find(|plan| Some(plan.configuration.track_id) != self.active)
                .and_then(|plan| plan.handle.clone())
                .ok_or_else(|| invalid("remote GUI cannot retire an unknown handoff"))?;
            self.retire(session, &track)?;
        }
        Ok(())
    }
    pub(super) fn activated(&mut self, id: u64) {
        self.active = Some(id);
    }

    /// Reconciles only the graph this backend can ever create.
    ///
    /// There is exactly one possible node (ID one, always referencing canvas one).
    /// Its authoritative count therefore resolves an interrupted initial scene
    /// transaction; no node mutation is replayed to guess whether it committed.
    pub(super) fn reconcile(&mut self, session: &mut Session) -> io::Result<()> {
        while let Some(event) = session.take_event()? {
            if let crate::SessionEvent::TargetChanged(payload) = event {
                session.apply_target_changed(&payload)?;
            }
        }
        let observed = snapshot(session)?;
        let context = session.info().root_context_id;
        if !observed.input_disabled
            || observed.establishment_state != 1
            || observed.resume_generation != session.info().resume_generation
            || observed.session_revision < session.info().session_revision
            || observed.usage[0] > 1
            || observed.usage[1] > MAX_TRACKS as u64
            || observed.usage[2] > 1
        {
            return Err(invalid(
                "resumed remote GUI state does not match its bounded graph",
            ));
        }
        if observed.target_generation != session.info().target_generation.get() {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "remote GUI target changed during reconciliation",
            ));
        }
        let mut surface_exists = false;
        let mut tracks = Vec::new();
        let mut lost = Vec::new();
        let mut active = None;
        for entry in &observed.entries {
            if entry.context_id != context {
                return Err(invalid("remote GUI snapshot contains an unknown context"));
            }
            match entry.kind {
                Kind::Context if entry.state == 1 => (),
                Kind::Surface if entry.surface_id == 1 && entry.state == 1 => surface_exists = true,
                Kind::Track
                    if entry.surface_id == 1
                        && matches!(entry.state, 1 | 6)
                        && self
                            .tracks
                            .iter()
                            .any(|plan| plan.configuration.track_id == entry.object_id) =>
                {
                    tracks.push(entry.object_id);
                    if entry.state == 6 {
                        lost.push(entry.object_id);
                    }
                }
                Kind::Slot if entry.surface_id == 1 && entry.object_id == 1 => {
                    active = Some(entry.detail)
                }
                _ => return Err(invalid("remote GUI snapshot contains an unknown object")),
            }
        }
        if u64::from(surface_exists) != observed.usage[0]
            || tracks.len() as u64 != observed.usage[1]
            || active.is_some_and(|id| !tracks.contains(&id))
            || (!surface_exists && observed.usage[2] != 0)
        {
            return Err(invalid("remote GUI snapshot counts are inconsistent"));
        }
        session.advance_allocator_past(self.highwater)?;
        if let Some(plan) = &mut self.surface {
            if !surface_exists && plan.handle.is_some() {
                return Err(invalid("resumed remote GUI canvas was retired"));
            }
            if surface_exists {
                // Unknown CREATE outcomes are retried with their exact original
                // idempotency key/body, then queried before any channel reopens.
                if plan.handle.is_none() {
                    plan.handle =
                        Some(session.create_surface(plan.definition.clone(), &plan.metadata)?);
                }
                session.adopt_surface(plan.handle.as_ref().expect("reconciled surface"))?;
            }
        } else if surface_exists {
            return Err(invalid("remote GUI does not own the retained canvas"));
        }
        self.tracks
            .retain(|plan| tracks.contains(&plan.configuration.track_id));
        for plan in &mut self.tracks {
            if plan.handle.is_none() {
                plan.handle =
                    Some(session.create_track(plan.configuration.clone(), &plan.metadata)?);
            }
            if lost.contains(&plan.configuration.track_id) {
                // Terminally lost tracks cannot be revived by a channel advance.
                // Reconcile their exact identity, then retire before replacement.
                session.query_track(plan.handle.as_ref().expect("reconciled track"))?;
                plan.retiring = true;
            } else {
                session.adopt_track(plan.handle.as_ref().expect("reconciled track"))?;
            }
            plan.advance = true;
        }
        let confirmed = snapshot(session)?;
        if confirmed != observed {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "remote GUI reconciliation changed while querying objects",
            ));
        }
        session.info.scene_revision =
            vivid_protocol::revision::SceneRevision::new(observed.scene_revision);
        self.node = observed.usage[2] == 1;
        self.active = active;
        let retired: Vec<_> = self
            .tracks
            .iter()
            .filter(|plan| plan.retiring)
            .filter_map(|plan| plan.handle.clone())
            .collect();
        for track in retired {
            self.retire(session, &track)?;
        }
        Ok(())
    }
}

fn snapshot(session: &Session) -> io::Result<Page> {
    let mut query = Query {
        maximum_entries: MAX_SUMMARIES as u16,
        ..Default::default()
    };
    let mut first: Option<Page> = None;
    for _ in 0..MAX_PAGES {
        let mut page = session.query_session_page(&query).map_err(|error| {
            if query.cursor.is_some()
                && error
                    .get_ref()
                    .and_then(|error| error.downcast_ref::<crate::PresenterError>())
                    .is_some_and(|reply| {
                        !reply.fatal
                            && reply.code == vivid_protocol::messages::ERROR_PRECONDITION_FAILED
                    })
            {
                io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "remote GUI snapshot cursor became stale",
                )
            } else {
                error
            }
        })?;
        query.cursor = page.cursor.take();
        if let Some(first) = &mut first {
            if page.session_revision != first.session_revision
                || page.scene_revision != first.scene_revision
                || page.target_generation != first.target_generation
                || page.resume_generation != first.resume_generation
                || page.establishment_state != first.establishment_state
                || page.input_disabled != first.input_disabled
                || page.usage != first.usage
            {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "remote GUI snapshot changed",
                ));
            }
            if first
                .entries
                .len()
                .checked_add(page.entries.len())
                .is_none_or(|len| len > MAX_SUMMARIES)
            {
                return Err(invalid("remote GUI snapshot exceeded its bound"));
            }
            first.entries.append(&mut page.entries);
        } else {
            first = Some(page);
        }
        if query.cursor.is_none() {
            let page = first.expect("received snapshot page");
            page.payload()?;
            return Ok(page);
        }
    }
    Err(invalid("remote GUI snapshot page bound exceeded"))
}

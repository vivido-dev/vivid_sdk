//! Resume authentication and post-resume re-association.
//!
//! A resumed session keeps its logical identity but shares no key material with the connection it
//! replaces, so handles are adopted explicitly rather than assumed to have survived.

use std::io;
use std::sync::Arc;

use vivid_protocol::auth;
use vivid_protocol::auth::Secret32;
use vivid_protocol::messages::PayloadMap;
use vivid_protocol::revision::TargetGeneration;

use crate::*;

impl Session {
    /// Prepare a fresh, secret-bearing resume authentication request for this leased session.
    ///
    /// Callers should retain this value before releasing the old session object. It implements
    /// neither `Debug` nor `Display`.
    pub fn resume_authentication(&self) -> io::Result<ProducerAuthentication> {
        let (context_id, lease_id) = self.lease_identity.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "root sessions are intentionally non-resumable",
            )
        })?;
        let resume_key = self
            .resume_key
            .as_ref()
            .ok_or_else(|| invalid_data("leased session has no resume key"))?;
        let mut attempt_id = [0; auth::ATTEMPT_ID_BYTES];
        random_bytes(&mut attempt_id)?;
        Ok(ProducerAuthentication::Resume {
            context_id,
            lease_id,
            session_id: self.info.session_id,
            resume_generation: self.info.resume_generation,
            attempt_id,
            prior_resume_key: Secret32::new(*resume_key.expose()),
        })
    }

    /// Validate and apply one terminal `TARGET_CHANGED` payload to the cached target snapshot.
    pub fn apply_target_changed(&mut self, payload: &PayloadMap) -> io::Result<TargetGeneration> {
        // The descriptor's extent depends on the negotiated target profile; the generation and
        // reason always sit immediately above it at keys 9 and 10.
        let last = last_descriptor_key(&self.info.target_profile);
        validate_payload_keys("TARGET_CHANGED", payload, 0..=last, &[9, 10])?;
        required_u64(payload, 10)?;
        let generation = TargetGeneration::new(required_u64(payload, 9)?);
        generation.require_nonzero()?;
        let descriptor = payload
            .iter()
            .filter(|(key, _)| *key <= last)
            .cloned()
            .collect();
        validate_target_descriptor(&self.info.target_profile, &descriptor)?;
        if generation < self.info.target_generation {
            return Err(invalid_data(
                "TARGET_CHANGED moved the target generation backward",
            ));
        }
        if generation == self.info.target_generation {
            let profile = self.info.target_profile.clone();
            let settle_key = settled_key(&profile);
            let current = &self.info.target_descriptor;
            let current_settled = descriptor_settled(&profile, current)?;
            let next_settled = descriptor_settled(&profile, &descriptor)?;
            let same_geometry = current
                .iter()
                .filter(|(key, _)| *key != settle_key)
                .eq(descriptor.iter().filter(|(key, _)| *key != settle_key));
            if current_settled || !next_settled || !same_geometry {
                return Err(invalid_data(
                    "TARGET_CHANGED reused a generation without an identical final settle",
                ));
            }
            self.info.target_descriptor = descriptor;
            return Ok(generation);
        }
        self.info.target_generation = generation;
        self.info.target_descriptor = descriptor;
        Ok(generation)
    }

    /// Re-associate a retained surface handle after authenticated session resume.
    pub fn adopt_surface(&mut self, surface: &Surface) -> io::Result<SurfaceStatus> {
        let status = self.query_surface(surface)?;
        if status.lifecycle == 3 {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "cannot adopt a surface tombstone",
            ));
        }
        let key = (status.context_id, status.surface_id);
        if let Some(existing) = self.surfaces.get(&key) {
            if Arc::ptr_eq(existing, &surface.inner) {
                return Ok(status);
            }
            return Err(invalid_input(
                "a different retained surface uses the same owner-qualified identity",
            ));
        }
        self.surfaces.insert(key, surface.inner.clone());
        self.advance_allocator_past(status.surface_id)?;
        Ok(status)
    }

    /// Re-associate a retained immutable track handle after its surface has been adopted.
    pub fn adopt_track(&mut self, track: &Track) -> io::Result<TrackStatus> {
        let configuration = track.configuration()?;
        if !self
            .surfaces
            .contains_key(&(configuration.context_id, configuration.surface_id))
        {
            return Err(invalid_input(
                "adopt the track's owner surface before adopting the track",
            ));
        }
        let status = self.query_track(track)?;
        if matches!(status.lifecycle, 6 | 7) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "cannot adopt a lost track or tombstone",
            ));
        }
        let key = (status.context_id, status.surface_id, status.track_id);
        let mut tracks = lock(&self.tracks, "track registry")?;
        if let Some(existing) = tracks.get(&key) {
            if Arc::ptr_eq(existing, &track.inner) {
                return Ok(status);
            }
            return Err(invalid_input(
                "a different retained track uses the same complete identity",
            ));
        }
        tracks.insert(key, track.inner.clone());
        drop(tracks);
        self.advance_allocator_past(status.track_id)?;
        Ok(status)
    }
}

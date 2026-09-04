//! Stable surfaces: semantic, scene, policy, and input identity.
//!
//! A surface outlives the tracks bound to it — replacing a codec never disturbs the input binding,
//! which is the property this split is meant to keep obvious.

use std::io;
use std::sync::{Arc, Mutex};

use vivid_protocol::cbor::Value;
use vivid_protocol::messages;
use vivid_protocol::messages::PayloadMap;
use vivid_protocol::revision::{SurfaceGeneration, SurfaceRevision};

use crate::*;

#[derive(Clone)]
pub struct Surface {
    pub(crate) inner: Arc<Mutex<SurfaceLocal>>,
}

#[derive(Debug, Clone)]
pub(crate) struct SurfaceLocal {
    pub(crate) definition: SurfaceDefinition,
    pub(crate) revision: SurfaceRevision,
    pub(crate) generation: SurfaceGeneration,
    pub(crate) destroyed: bool,
}

impl std::fmt::Debug for Surface {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.inner.lock() {
            Ok(state) => formatter
                .debug_struct("Surface")
                .field("context_id", &state.definition.context_id)
                .field("surface_id", &state.definition.surface_id)
                .field("revision", &state.revision)
                .field("generation", &state.generation)
                .field("destroyed", &state.destroyed)
                .finish(),
            Err(_) => formatter.write_str("Surface(<poisoned>)"),
        }
    }
}

impl Surface {
    pub fn context_id(&self) -> u64 {
        self.inner
            .lock()
            .map_or(0, |state| state.definition.context_id)
    }

    pub fn id(&self) -> u64 {
        self.inner
            .lock()
            .map_or(0, |state| state.definition.surface_id)
    }

    pub fn revision(&self) -> SurfaceRevision {
        self.inner
            .lock()
            .map_or(SurfaceRevision::ZERO, |state| state.revision)
    }

    pub fn generation(&self) -> SurfaceGeneration {
        self.inner
            .lock()
            .map_or(SurfaceGeneration::ZERO, |state| state.generation)
    }

    pub fn definition(&self) -> io::Result<SurfaceDefinition> {
        Ok(lock(&self.inner, "surface")?.definition.clone())
    }
}

impl Session {
    pub fn create_surface(
        &mut self,
        definition: SurfaceDefinition,
        metadata: &RequestMetadata,
    ) -> io::Result<Surface> {
        definition.validate()?;
        let key = (definition.context_id, definition.surface_id);
        if self.surfaces.contains_key(&key) {
            return Err(invalid_input("surface identity is already live"));
        }
        let reply = self.request(
            messages::CREATE_SURFACE,
            definition.surface_id,
            definition.create_payload()?,
            metadata,
            None,
            None,
        )?;
        let (revision, generation, effective_policy, parameters) = if let Some(record) = reply {
            expect_record(&record, messages::SURFACE_READY, definition.surface_id)?;
            let payload = decoded_payload(&record)?;
            validate_exact_payload_keys("SURFACE_READY", &payload, 0..=5)?;
            validate_owner_pair(&payload, definition.context_id, definition.surface_id)?;
            (
                SurfaceRevision::new(required_u64(&payload, 2)?),
                SurfaceGeneration::new(required_u64(&payload, 3)?),
                required_u64(&payload, 4)?,
                required_map(&payload, 5)?.to_vec(),
            )
        } else {
            (
                SurfaceRevision::ONE,
                SurfaceGeneration::ONE,
                definition.policy,
                definition.profile_parameters.clone(),
            )
        };
        revision.require_nonzero()?;
        generation.require_nonzero()?;
        if generation != SurfaceGeneration::ONE {
            return Err(invalid_data("SURFACE_READY initial generation is not one"));
        }
        let mut effective_definition = definition;
        effective_definition.policy = effective_policy;
        effective_definition.profile_parameters = parameters;
        let inner = Arc::new(Mutex::new(SurfaceLocal {
            definition: effective_definition,
            revision,
            generation,
            destroyed: false,
        }));
        self.surfaces.insert(key, inner.clone());
        Ok(Surface { inner })
    }

    pub fn update_surface(
        &mut self,
        surface: &Surface,
        replacement: SurfaceDefinition,
        metadata: &RequestMetadata,
    ) -> io::Result<()> {
        replacement.validate()?;
        let current = lock(&surface.inner, "surface")?.clone();
        ensure_live_surface(&current)?;
        if replacement.context_id != current.definition.context_id
            || replacement.surface_id != current.definition.surface_id
            || replacement.semantic_profile != current.definition.semantic_profile
            || replacement.coordinate_model != current.definition.coordinate_model
        {
            return Err(invalid_input(
                "surface update changes immutable identity or semantic profile",
            ));
        }
        let payload = vec![
            (0, Value::Unsigned(replacement.context_id)),
            (1, Value::Unsigned(replacement.surface_id)),
            (2, Value::Unsigned(current.revision.get())),
            (3, Value::Unsigned(current.generation.get())),
            (4, Value::Unsigned(replacement.logical_width)),
            (5, Value::Unsigned(replacement.logical_height)),
            (6, Value::Unsigned(replacement.scale_numerator)),
            (7, Value::Unsigned(replacement.scale_denominator)),
            (8, Value::Unsigned(u64::from(replacement.rotation))),
            (9, replacement.descriptor.to_value()?),
            (10, Value::Unsigned(replacement.policy)),
            (11, Value::Map(replacement.profile_parameters.clone())),
        ];
        self.request_ok(
            messages::UPDATE_SURFACE,
            replacement.surface_id,
            payload,
            metadata,
        )?;
        let mapping_changed = current.definition.logical_width != replacement.logical_width
            || current.definition.logical_height != replacement.logical_height
            || current.definition.scale_numerator != replacement.scale_numerator
            || current.definition.scale_denominator != replacement.scale_denominator
            || current.definition.rotation != replacement.rotation
            || current.definition.profile_parameters != replacement.profile_parameters;
        let mut state = lock(&surface.inner, "surface")?;
        state.revision = state.revision.advance()?;
        if mapping_changed {
            state.generation = state.generation.advance()?;
        }
        let mut replacement = replacement;
        replacement.policy |= state.definition.policy;
        state.definition = replacement;
        Ok(())
    }

    pub fn destroy_surface(
        &mut self,
        surface: &Surface,
        metadata: &RequestMetadata,
    ) -> io::Result<()> {
        let snapshot = lock(&surface.inner, "surface")?.clone();
        ensure_live_surface(&snapshot)?;
        self.request_ok(
            messages::DESTROY_SURFACE,
            snapshot.definition.surface_id,
            vec![
                (0, Value::Unsigned(snapshot.definition.context_id)),
                (1, Value::Unsigned(snapshot.definition.surface_id)),
            ],
            metadata,
        )?;
        let context_id = snapshot.definition.context_id;
        let surface_id = snapshot.definition.surface_id;
        lock(&surface.inner, "surface")?.destroyed = true;
        self.surfaces.remove(&(context_id, surface_id));
        lock(&self.tracks, "track registry")?.retain(|(context, owner, _), state| {
            let keep = *context != context_id || *owner != surface_id;
            if !keep && let Ok(mut state) = state.lock() {
                state.destroyed = true;
                let active_flow = state.active_flow.take();
                state.active_media = None;
                close_track_flow(active_flow.as_ref(), "owning surface destroyed");
            }
            keep
        });
        Ok(())
    }

    pub fn query_surface(&self, surface: &Surface) -> io::Result<SurfaceStatus> {
        let snapshot = lock(&surface.inner, "surface")?.clone();
        let reply = self.request(
            messages::QUERY_SURFACE,
            snapshot.definition.surface_id,
            vec![
                (0, Value::Unsigned(snapshot.definition.context_id)),
                (1, Value::Unsigned(snapshot.definition.surface_id)),
            ],
            &RequestMetadata::default(),
            None,
            None,
        )?;
        let status = if let Some(record) = reply {
            expect_record(
                &record,
                messages::SURFACE_STATUS,
                snapshot.definition.surface_id,
            )?;
            let payload = decoded_payload(&record)?;
            validate_exact_payload_keys("SURFACE_STATUS", &payload, 0..=15)?;
            validate_owner_pair(
                &payload,
                snapshot.definition.context_id,
                snapshot.definition.surface_id,
            )?;
            let rotation = u16::try_from(required_u64(&payload, 10)?)
                .map_err(|_| invalid_data("SURFACE_STATUS rotation exceeds u16"))?;
            let lifecycle = required_u64(&payload, 14)?;
            if !(1..=3).contains(&lifecycle) {
                return Err(invalid_data("SURFACE_STATUS has an unknown lifecycle"));
            }
            SurfaceStatus {
                context_id: required_u64(&payload, 0)?,
                surface_id: required_u64(&payload, 1)?,
                revision: SurfaceRevision::new(required_u64(&payload, 2)?),
                generation: SurfaceGeneration::new(required_u64(&payload, 3)?),
                semantic_profile: required_text(&payload, 4)?.to_owned(),
                coordinate_model: CoordinateModel::try_from(required_u64(&payload, 5)?)
                    .map_err(io::Error::other)?,
                logical_width: required_u64(&payload, 6)?,
                logical_height: required_u64(&payload, 7)?,
                scale_numerator: required_u64(&payload, 8)?,
                scale_denominator: required_u64(&payload, 9)?,
                rotation,
                descriptor: SurfaceDescriptor::from_value(required_value(&payload, 11)?)
                    .map_err(io::Error::other)?,
                effective_policy: required_u64(&payload, 12)?,
                active_slots: required_map(&payload, 13)?.to_vec(),
                lifecycle,
                profile_status: required_map(&payload, 15)?.to_vec(),
            }
        } else {
            SurfaceStatus {
                context_id: snapshot.definition.context_id,
                surface_id: snapshot.definition.surface_id,
                revision: snapshot.revision,
                generation: snapshot.generation,
                semantic_profile: snapshot.definition.semantic_profile.clone(),
                coordinate_model: snapshot.definition.coordinate_model,
                logical_width: snapshot.definition.logical_width,
                logical_height: snapshot.definition.logical_height,
                scale_numerator: snapshot.definition.scale_numerator,
                scale_denominator: snapshot.definition.scale_denominator,
                rotation: snapshot.definition.rotation,
                descriptor: snapshot.definition.descriptor.clone(),
                effective_policy: snapshot.definition.policy,
                active_slots: vec![],
                lifecycle: if snapshot.destroyed { 3 } else { 1 },
                profile_status: snapshot.definition.profile_parameters.clone(),
            }
        };
        status.revision.require_nonzero()?;
        status.generation.require_nonzero()?;
        if status.logical_width == 0
            || status.logical_height == 0
            || status.scale_numerator == 0
            || status.scale_denominator == 0
            || !matches!(status.rotation, 0 | 90 | 180 | 270)
            || status.semantic_profile != snapshot.definition.semantic_profile
            || status.coordinate_model != snapshot.definition.coordinate_model
        {
            return Err(invalid_data(
                "SURFACE_STATUS contains invalid or changed immutable configuration",
            ));
        }
        let mut state = lock(&surface.inner, "surface")?;
        state.revision = status.revision;
        state.generation = status.generation;
        state.definition.logical_width = status.logical_width;
        state.definition.logical_height = status.logical_height;
        state.definition.scale_numerator = status.scale_numerator;
        state.definition.scale_denominator = status.scale_denominator;
        state.definition.rotation = status.rotation;
        state.definition.descriptor = status.descriptor.clone();
        state.definition.policy = status.effective_policy;
        state.destroyed = status.lifecycle == 3;
        Ok(status)
    }
}

/// Authoritative state returned by `QUERY_SURFACE`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceStatus {
    pub context_id: u64,
    pub surface_id: u64,
    pub revision: SurfaceRevision,
    pub generation: SurfaceGeneration,
    pub semantic_profile: String,
    pub coordinate_model: CoordinateModel,
    pub logical_width: u64,
    pub logical_height: u64,
    pub scale_numerator: u64,
    pub scale_denominator: u64,
    pub rotation: u16,
    pub descriptor: SurfaceDescriptor,
    pub effective_policy: u64,
    pub active_slots: PayloadMap,
    pub lifecycle: u64,
    pub profile_status: PayloadMap,
}

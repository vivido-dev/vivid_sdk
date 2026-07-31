//! Scene transactions, node placement, slot activation, and terminal anchors.

use std::collections::BTreeSet;
use std::io;

use vivid_protocol::cbor::Value;
use vivid_protocol::messages::{Envelope, PayloadMap};
use vivid_protocol::revision::{
    ChannelGeneration, SceneRevision, SurfaceRevision, TargetGeneration,
};
use vivid_protocol::{anchor, messages};

use crate::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotBinding {
    pub slot: u64,
    pub track_id: u64,
    pub expected_channel_generation: ChannelGeneration,
    pub required_milestone: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SceneCommit {
    pub scene_revision: SceneRevision,
    pub target_generation: TargetGeneration,
}

impl Session {
    pub fn create_node(
        &mut self,
        node: &SceneNode,
        metadata: &RequestMetadata,
    ) -> io::Result<SceneCommit> {
        node.validate()?;
        self.scene_transaction(
            node.owning_context_id,
            messages::CREATE_NODE,
            node.node_id,
            node.payload()?,
            metadata,
        )
    }

    /// Replace one existing node's placement inside its own scene transaction.
    ///
    /// The node keeps its identity and its surface reference; only geometry, fit, ordering,
    /// visibility, opacity, and clip change. Track replacement uses [`Session::activate_tracks`],
    /// never a scene transaction.
    pub fn update_node(
        &mut self,
        node: &SceneNode,
        metadata: &RequestMetadata,
    ) -> io::Result<SceneCommit> {
        node.validate()?;
        self.scene_transaction(
            node.owning_context_id,
            messages::UPDATE_NODE,
            node.node_id,
            node.payload()?,
            metadata,
        )
    }

    /// Remove one node from the scene inside its own transaction.
    ///
    /// Deleting a node does not destroy the surface it referenced.
    pub fn delete_node(
        &mut self,
        context_id: u64,
        node_id: u64,
        metadata: &RequestMetadata,
    ) -> io::Result<SceneCommit> {
        if context_id == 0 || node_id == 0 {
            return Err(invalid_input("node deletion requires complete identity"));
        }
        self.scene_transaction(
            context_id,
            messages::DELETE_NODE,
            node_id,
            vec![
                (0, Value::Unsigned(context_id)),
                (1, Value::Unsigned(node_id)),
            ],
            metadata,
        )
    }

    /// Apply one node mutation as a complete `BEGIN_TXN`/mutation/`COMMIT_TXN` transaction.
    ///
    /// The commit carries the cached expected target generation and scene-revision precondition,
    /// so a target change that races the mutation is rejected rather than applied against stale
    /// coordinate truth. Any failure aborts the open transaction.
    pub(crate) fn scene_transaction(
        &mut self,
        owning_context_id: u64,
        mutation_type: u16,
        node_id: u64,
        mutation_payload: PayloadMap,
        metadata: &RequestMetadata,
    ) -> io::Result<SceneCommit> {
        let transaction_id = self.allocate_id()?;
        let begin_request = self.next_request()?;
        let mut begin = Envelope::correlated(
            begin_request,
            vec![
                (0, Value::Unsigned(owning_context_id)),
                (1, Value::Unsigned(transaction_id)),
            ],
        )?;
        begin.transaction_id = Some(transaction_id);
        metadata.apply(&mut begin)?;
        self.dispatch_ok(
            begin_request,
            messages::BEGIN_TXN,
            transaction_id,
            &begin.encode()?,
        )?;

        let mutation_request = self.next_request()?;
        let mut mutation = Envelope::correlated(mutation_request, mutation_payload)?;
        mutation.transaction_id = Some(transaction_id);
        metadata.apply(&mut mutation)?;
        if let Err(error) = self.dispatch_ok(
            mutation_request,
            mutation_type,
            node_id,
            &mutation.encode()?,
        ) {
            let _ = self.abort_transaction(transaction_id);
            return Err(error);
        }

        let commit_request = self.next_request()?;
        let mut commit = Envelope::correlated(commit_request, vec![(0, Value::Unsigned(0))])?;
        commit.transaction_id = Some(transaction_id);
        commit.expected_target_generation = Some(self.info.target_generation.get());
        commit.preconditions = vec![(0, Value::Unsigned(self.info.scene_revision.get()))];
        commit.idempotency_key = metadata.idempotency_key;
        commit.causation_id = metadata.causation_id;
        let reply = match self.control.request(
            commit_request,
            messages::COMMIT_TXN,
            transaction_id,
            &commit.encode()?,
        ) {
            Ok(reply) => reply,
            Err(error) => {
                let _ = self.abort_transaction(transaction_id);
                return Err(error);
            }
        };
        let result = if let Some(record) = reply {
            if record.record_type == messages::ERROR {
                let _ = self.abort_transaction(transaction_id);
                return Err(presenter_error(&record.body)?);
            }
            if let Err(error) = expect_record(&record, messages::SCENE_PRESENTED, transaction_id) {
                let _ = self.abort_transaction(transaction_id);
                return Err(error);
            }
            let payload = decoded_payload(&record)?;
            validate_exact_payload_keys("SCENE_PRESENTED", &payload, 0..=1)?;
            SceneCommit {
                scene_revision: SceneRevision::new(required_u64(&payload, 0)?),
                target_generation: TargetGeneration::new(required_u64(&payload, 1)?),
            }
        } else {
            SceneCommit {
                scene_revision: self.info.scene_revision.advance()?,
                target_generation: self.info.target_generation,
            }
        };
        self.info.scene_revision = result.scene_revision;
        Ok(result)
    }

    pub(crate) fn abort_transaction(&self, transaction_id: u64) -> io::Result<()> {
        let request_id = self.next_request()?;
        let mut envelope = Envelope::correlated(request_id, vec![])?;
        envelope.transaction_id = Some(transaction_id);
        self.dispatch_ok(
            request_id,
            messages::ABORT_TXN,
            transaction_id,
            &envelope.encode()?,
        )
    }

    pub fn activate_tracks(
        &mut self,
        surface: &Surface,
        bindings: &[SlotBinding],
        metadata: &RequestMetadata,
    ) -> io::Result<u64> {
        if bindings.is_empty() {
            return Err(invalid_input("ACTIVATE_TRACK requires a slot binding"));
        }
        let snapshot = lock(&surface.inner, "surface")?.clone();
        ensure_live_surface(&snapshot)?;
        let mut seen_slots = BTreeSet::new();
        for binding in bindings {
            if !(1..=4).contains(&binding.slot) || !seen_slots.insert(binding.slot) {
                return Err(invalid_input(
                    "ACTIVATE_TRACK slots must be known and unique",
                ));
            }
            if binding.track_id == 0 {
                if binding.expected_channel_generation != ChannelGeneration::ZERO
                    || binding.required_milestone != 0
                {
                    return Err(invalid_input(
                        "a cleared slot requires zero generation and milestone",
                    ));
                }
                continue;
            }
            if binding.expected_channel_generation == ChannelGeneration::ZERO
                || binding.required_milestone.count_ones() != 1
                || binding.required_milestone & !MILESTONE_KNOWN_MASK != 0
            {
                return Err(invalid_input(
                    "active slot binding has an invalid generation or milestone",
                ));
            }
            let tracks = lock(&self.tracks, "track registry")?;
            let track = tracks
                .get(&(
                    snapshot.definition.context_id,
                    snapshot.definition.surface_id,
                    binding.track_id,
                ))
                .ok_or_else(|| {
                    invalid_input("ACTIVATE_TRACK references a track outside this surface")
                })?;
            let track = lock(track, "track")?;
            ensure_live_track(&track)?;
            if track.configuration.slot != binding.slot
                || track.channel_generation != binding.expected_channel_generation
            {
                return Err(invalid_input(
                    "ACTIVATE_TRACK binding does not match current track state",
                ));
            }
        }
        let payload_bindings = bindings
            .iter()
            .map(|binding| {
                Value::Map(vec![
                    (0, Value::Unsigned(binding.slot)),
                    (1, Value::Unsigned(binding.track_id)),
                    (
                        2,
                        Value::Unsigned(binding.expected_channel_generation.get()),
                    ),
                    (3, Value::Unsigned(binding.required_milestone)),
                ])
            })
            .collect();
        let reply = self.request(
            messages::ACTIVATE_TRACK,
            snapshot.definition.surface_id,
            vec![
                (0, Value::Unsigned(snapshot.definition.context_id)),
                (1, Value::Unsigned(snapshot.definition.surface_id)),
                (2, Value::Array(payload_bindings)),
                (3, Value::Unsigned(snapshot.revision.get())),
            ],
            metadata,
            None,
            None,
        )?;
        let (new_revision, presentation_id) = if let Some(record) = reply {
            expect_record(
                &record,
                messages::TRACK_ACTIVATED,
                snapshot.definition.surface_id,
            )?;
            let payload = decoded_payload(&record)?;
            validate_exact_payload_keys("TRACK_ACTIVATED", &payload, 0..=4)?;
            validate_owner_pair(
                &payload,
                snapshot.definition.context_id,
                snapshot.definition.surface_id,
            )?;
            (
                SurfaceRevision::new(required_u64(&payload, 3)?),
                required_u64(&payload, 4)?,
            )
        } else {
            (snapshot.revision.advance()?, 1)
        };
        new_revision.require_nonzero()?;
        if new_revision <= snapshot.revision || presentation_id == 0 {
            return Err(invalid_data(
                "TRACK_ACTIVATED did not advance revision and presentation identity",
            ));
        }
        lock(&surface.inner, "surface")?.revision = new_revision;
        Ok(presentation_id)
    }

    /// Create a terminal grid node using signed 32.32 cell coordinates.
    #[allow(clippy::too_many_arguments)]
    pub fn place_terminal_surface(
        &mut self,
        surface: &Surface,
        node_id: u64,
        x: i64,
        y: i64,
        width: i64,
        height: i64,
        text_layer: u64,
    ) -> io::Result<SceneCommit> {
        if width <= 0 || height <= 0 || text_layer > 2 {
            return Err(invalid_input("invalid terminal node geometry"));
        }
        let node = SceneNode {
            owning_context_id: surface.context_id(),
            node_id,
            surface_context_id: surface.context_id(),
            surface_id: surface.id(),
            geometry: vec![
                (0, Value::Unsigned(1)),
                (1, signed(x)),
                (2, signed(y)),
                (3, signed(width)),
                (4, signed(height)),
                (5, Value::Unsigned(text_layer)),
            ],
            fit: Fit::Contain,
            linear_sampling: true,
            z_index: 0,
            visible: true,
            opacity: u16::MAX,
            clip: None,
        };
        self.create_node(&node, &RequestMetadata::default())
    }

    pub fn anchor_marker(&self, context_id: u64, anchor_id: u64) -> io::Result<String> {
        anchor::encode_marker(
            &self.anchor_key,
            &self.info.session_tag,
            context_id,
            anchor_id,
        )
        .map_err(|message| invalid_input(message.to_owned()))
    }

    pub fn conpty_anchor_marker(&self, context_id: u64, anchor_id: u64) -> io::Result<String> {
        anchor::encode_conpty_marker(
            &self.anchor_key,
            &self.info.session_tag,
            context_id,
            anchor_id,
        )
        .map_err(|message| invalid_input(message.to_owned()))
    }

    pub fn query_anchor(&self, context_id: u64, anchor_id: u64) -> io::Result<AnchorStatus> {
        if context_id == 0 || anchor_id == 0 {
            return Err(invalid_input("anchor identity must be nonzero"));
        }
        let reply = self.request(
            messages::QUERY_ANCHOR,
            anchor_id,
            vec![
                (0, Value::Unsigned(context_id)),
                (1, Value::Unsigned(anchor_id)),
            ],
            &RequestMetadata::default(),
            None,
            None,
        )?;
        let payload = if let Some(record) = reply {
            expect_record(&record, messages::ANCHOR_STATUS, anchor_id)?;
            let payload = decoded_payload(&record)?;
            validate_payload_keys("ANCHOR_STATUS", &payload, 0..=2, &[3, 4, 5, 6])?;
            payload
        } else {
            vec![
                (0, Value::Unsigned(context_id)),
                (1, Value::Unsigned(anchor_id)),
                (2, Value::Unsigned(0)),
                (6, Value::Unsigned(self.info.target_generation.get())),
            ]
        };
        if required_u64(&payload, 0)? != context_id || required_u64(&payload, 1)? != anchor_id {
            return Err(invalid_data(
                "ANCHOR_STATUS changed complete anchor identity",
            ));
        }
        let state = required_u64(&payload, 2)?;
        if state > 2 {
            return Err(invalid_data("ANCHOR_STATUS has an unknown lifecycle state"));
        }
        let target_generation = optional_u64(&payload, 6)?
            .map(TargetGeneration::new)
            .filter(|generation| *generation != TargetGeneration::ZERO);
        Ok(AnchorStatus {
            context_id,
            anchor_id,
            state,
            target_generation,
            payload,
        })
    }

    pub fn emit_anchor<W: io::Write>(
        &self,
        output: &mut W,
        context_id: u64,
        anchor_id: u64,
    ) -> io::Result<()> {
        output.write_all(self.anchor_marker(context_id, anchor_id)?.as_bytes())?;
        output.flush()
    }
}

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

// ---------------------------------------------------------------------------------------------
// Terminal-grid placement
//
// Contain-fit of a fixed-size source into the terminal grid, in 32.32 fixed-point cells. Shared
// because every terminal producer needs exactly this and the integer arithmetic is easy to get
// subtly wrong; the grid metrics come from the negotiated target descriptor, never from an ioctl,
// because through `vvssh` or inside `vvmux` the local terminal is not the presenter's terminal.
// ---------------------------------------------------------------------------------------------

/// Grid metrics of the `terminal-surface-v1` target, extracted from the target descriptor.
///
/// Vivid 1.5 carries these on the negotiated target instead of a producer-side display state, so
/// they are read from the session and never from an ioctl or an escape-sequence query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalDisplay {
    pub grid_columns: u32,
    pub grid_rows: u32,
    pub cell_width: u32,
    pub cell_height: u32,
}

/// Grid-cell coordinate space of the terminal target.
pub const COORDINATE_SPACE_GRID_CELL: u64 = 1;
/// The text layer between the background and glyph layers of the target.
pub const TEXT_LAYER_BETWEEN_BACKGROUND_AND_GLYPH: u64 = 1;

const FIXED_ONE: i128 = 1_i128 << 32;

/// A contain-fit rectangle in 32.32 fixed-point terminal cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalPlacement {
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
    pub source_width: u32,
    pub source_height: u32,
    status_row: u32,
}

impl TerminalPlacement {
    /// Fit `source_width` by `source_height` into the grid, reserving the last row for status.
    pub fn calculate(
        display: TerminalDisplay,
        source_width: u32,
        source_height: u32,
    ) -> io::Result<Self> {
        Self::calculate_mode(display, source_width, source_height, true)
    }

    /// Fit into the whole grid with no status row reserved.
    pub fn calculate_full(
        display: TerminalDisplay,
        source_width: u32,
        source_height: u32,
    ) -> io::Result<Self> {
        Self::calculate_mode(display, source_width, source_height, false)
    }

    fn calculate_mode(
        display: TerminalDisplay,
        source_width: u32,
        source_height: u32,
        reserve_status_row: bool,
    ) -> io::Result<Self> {
        let reserved_rows = if reserve_status_row { 1 } else { 0 };
        if source_width == 0
            || source_height == 0
            || display.grid_columns == 0
            || display.grid_rows < 1 + reserved_rows
            || display.cell_width == 0
            || display.cell_height == 0
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "display or source geometry cannot reserve a streaming area",
            ));
        }
        let available_columns = i128::from(display.grid_columns);
        let available_rows = i128::from(display.grid_rows - reserved_rows);
        let available_width_px = available_columns
            .checked_mul(i128::from(display.cell_width))
            .ok_or_else(overflow)?;
        let available_height_px = available_rows
            .checked_mul(i128::from(display.cell_height))
            .ok_or_else(overflow)?;
        let source_width = i128::from(source_width);
        let source_height = i128::from(source_height);
        let width_limited_height = available_width_px
            .checked_mul(source_height)
            .and_then(|value| value.checked_div(source_width))
            .ok_or_else(overflow)?;
        let (render_width_px, render_height_px) = if width_limited_height <= available_height_px {
            (available_width_px, width_limited_height)
        } else {
            let width = available_height_px
                .checked_mul(source_width)
                .and_then(|value| value.checked_div(source_height))
                .ok_or_else(overflow)?;
            (width, available_height_px)
        };
        let x_px = (available_width_px - render_width_px) / 2;
        let y_px = (available_height_px - render_height_px) / 2;
        let fixed = |pixels: i128, cell: u32| -> io::Result<i64> {
            pixels
                .checked_mul(FIXED_ONE)
                .and_then(|value| value.checked_div(i128::from(cell)))
                .and_then(|value| i64::try_from(value).ok())
                .ok_or_else(overflow)
        };
        Ok(Self {
            x: fixed(x_px, display.cell_width)?,
            y: fixed(y_px, display.cell_height)?,
            width: fixed(render_width_px, display.cell_width)?,
            height: fixed(render_height_px, display.cell_height)?,
            source_width: u32::try_from(source_width).expect("input was u32"),
            source_height: u32::try_from(source_height).expect("input was u32"),
            status_row: display.grid_rows - reserved_rows,
        })
    }

    /// The grid row the status line is drawn on, the first row below the media rectangle.
    pub fn status_row(self) -> u32 {
        self.status_row
    }

    /// The scene node that places this surface on the terminal grid.
    pub fn node(self, node_id: u64, surface_context_id: u64, surface_id: u64) -> SceneNode {
        SceneNode {
            owning_context_id: surface_context_id,
            node_id,
            surface_context_id,
            surface_id,
            geometry: vec![
                (0, Value::Unsigned(COORDINATE_SPACE_GRID_CELL)),
                (1, Value::Unsigned(u64::try_from(self.x).unwrap_or(0))),
                (2, Value::Unsigned(u64::try_from(self.y).unwrap_or(0))),
                (3, Value::Unsigned(u64::try_from(self.width).unwrap_or(0))),
                (4, Value::Unsigned(u64::try_from(self.height).unwrap_or(0))),
                (5, Value::Unsigned(TEXT_LAYER_BETWEEN_BACKGROUND_AND_GLYPH)),
            ],
            fit: vivid_protocol::scene::Fit::Contain,
            linear_sampling: true,
            z_index: 0,
            visible: true,
            opacity: u16::MAX,
            clip: None,
        }
    }
}

fn overflow() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "scene geometry overflow")
}

#[cfg(test)]
mod terminal_placement_tests {
    use super::*;

    fn display(columns: u32, rows: u32) -> TerminalDisplay {
        TerminalDisplay {
            grid_columns: columns,
            grid_rows: rows,
            cell_width: 10,
            cell_height: 20,
        }
    }

    #[test]
    fn contains_wide_video_and_reserves_status_row() {
        let placement = TerminalPlacement::calculate(display(80, 25), 1920, 1080).unwrap();
        assert!(placement.height <= 24_i64 << 32);
        assert!(placement.x >= 0);
        assert!(placement.y >= 0);
        assert_eq!(placement.status_row(), 24);
    }

    #[test]
    fn full_mode_uses_every_row() {
        let reserved = TerminalPlacement::calculate(display(80, 25), 1920, 1080).unwrap();
        let full = TerminalPlacement::calculate_full(display(80, 25), 1920, 1080).unwrap();
        assert_eq!(full.status_row(), 25);
        assert!(full.height >= reserved.height);
    }

    #[test]
    fn tall_source_is_pillarboxed() {
        let placement = TerminalPlacement::calculate(display(80, 25), 800, 1200).unwrap();
        assert!(placement.x > 0, "a tall source must leave side margins");
        assert!(placement.height <= 24_i64 << 32);
    }

    #[test]
    fn never_exceeds_the_available_grid() {
        for (source_width, source_height) in [(1920, 1080), (800, 1200), (640, 480), (65, 33)] {
            let placement =
                TerminalPlacement::calculate(display(80, 25), source_width, source_height).unwrap();
            assert!(placement.x + placement.width <= 80_i64 << 32);
            assert!(placement.y + placement.height <= 24_i64 << 32);
        }
    }

    #[test]
    fn rejects_degenerate_geometry() {
        // One row cannot hold both a status row and any media.
        assert!(TerminalPlacement::calculate(display(80, 1), 640, 480).is_err());
        assert!(TerminalPlacement::calculate(display(0, 25), 640, 480).is_err());
        assert!(TerminalPlacement::calculate(display(80, 25), 0, 480).is_err());
        assert!(TerminalPlacement::calculate(display(80, 25), 640, 0).is_err());
        let zero_cell = TerminalDisplay {
            grid_columns: 80,
            grid_rows: 25,
            cell_width: 0,
            cell_height: 20,
        };
        assert!(TerminalPlacement::calculate(zero_cell, 640, 480).is_err());
    }

    #[test]
    fn node_encodes_grid_cell_geometry() {
        let placement = TerminalPlacement::calculate(display(80, 25), 1280, 720).unwrap();
        let node = placement.node(7, 2, 3);
        assert_eq!(node.node_id, 7);
        assert_eq!(node.surface_context_id, 2);
        assert_eq!(node.surface_id, 3);
        assert_eq!(
            node.geometry[0],
            (0, Value::Unsigned(COORDINATE_SPACE_GRID_CELL))
        );
        assert_eq!(
            node.geometry[5],
            (5, Value::Unsigned(TEXT_LAYER_BETWEEN_BACKGROUND_AND_GLYPH))
        );
        assert!(node.visible);
        assert_eq!(node.opacity, u16::MAX);
    }
}

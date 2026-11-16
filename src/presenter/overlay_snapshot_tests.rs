//! Initial snapshot replacement preserves owner-scoped non-snapshot records.

// Rust guideline compliant 2026-10-07
use super::*;

fn geometry(width: f64) -> Viewport {
    Viewport {
        width: Scalar::new(width).unwrap(),
        height: Scalar::new(240.).unwrap(),
        scale_numerator: 1,
        scale_denominator: 1,
    }
}
fn records() -> Vec<LaneRecord> {
    let address = WindowAddress {
        context_id: 1,
        surface_id: 1,
        generation: 1,
    };
    let mut records = [true, false]
        .into_iter()
        .map(|down| LaneRecord {
            retention: None,
            record_type: messages::OVERLAY_INPUT_EVENT,
            object_id: 1,
            payload: vivid_protocol::overlay::wire::InputEvent {
                address,
                scene_revision: 7,
                event: Event::Key {
                    physical: 4,
                    down,
                    repeat: false,
                    modifiers: 0,
                },
            }
            .payload()
            .unwrap(),
        })
        .collect::<Vec<_>>();
    records.push(LaneRecord {
        retention: None,
        record_type: messages::OVERLAY_SUBMISSION_OUTCOME,
        object_id: 1,
        payload: SubmissionOutcome {
            submission: Submission {
                address,
                track_id: 5,
                channel_generation: 1,
                epoch: 1,
                revision: 7,
            },
            outcome: PresentationOutcome::Superseded,
        }
        .payload()
        .unwrap(),
    });
    records
}
fn copies(records: &[LaneRecord]) -> Vec<(u16, u64, PayloadMap)> {
    records
        .iter()
        .map(|r| (r.record_type, r.object_id, r.payload.clone()))
        .collect()
}
fn fixture() -> Overlays {
    let mut overlays = Overlays::default();
    for owner in [1, 2] {
        overlays
            .install_session_viewport(owner, owner, geometry(320.))
            .unwrap();
        for record in records() {
            overlays.queue_for(owner, record);
        }
    }
    overlays
}
fn check_snapshot(
    overlays: &mut Overlays,
    owner: u64,
    width: f64,
    tail: &[(u16, u64, PayloadMap)],
) {
    let records = overlays.take_pending(owner);
    assert_eq!(records.len(), tail.len() + 2);
    assert_eq!(records[0].record_type, messages::OVERLAY_ENV_CHANGED);
    assert_eq!(records[1].record_type, messages::OVERLAY_VIEWPORT_CHANGED);
    let environment = vivid_protocol::overlay::wire::EnvironmentChanged::decode(
        0,
        &Value::Map(records[0].payload.clone()),
    )
    .unwrap();
    let viewport = vivid_protocol::overlay::wire::ViewportChanged::decode(
        0,
        &Value::Map(records[1].payload.clone()),
    )
    .unwrap();
    assert_eq!(environment.revision, overlays.environment_revision);
    assert_eq!(environment.environment, overlays.environment);
    assert_eq!(
        viewport.revision,
        overlays.viewport_of(owner).unwrap().revision
    );
    assert_eq!(viewport.viewport, geometry(width));
    assert_eq!(copies(&records[2..]), tail);
}
#[test]
fn late_lane_sends_current_snapshots_once_before_preserved_input_and_outcomes() {
    let mut overlays = fixture();
    overlays.set_viewport(1, geometry(400.));
    overlays.set_viewport(1, geometry(500.));
    overlays.set_environment(Environment {
        appearance: vivid_protocol::overlay::wire::Appearance::Dark,
        ..Default::default()
    });
    overlays.set_environment(Environment {
        appearance: vivid_protocol::overlay::wire::Appearance::Light,
        ..Default::default()
    });
    let peer = copies(overlays.pending.get(&2).unwrap());
    overlays.open_lane(1, 1);
    assert_eq!(copies(overlays.pending.get(&2).unwrap()), peer);
    check_snapshot(&mut overlays, 1, 500., &copies(&records()));
    // Reopening replaces unsent snapshot history too, without manufacturing a
    // revision change or draining the unrelated owner's queue.
    overlays.close_lane(1);
    overlays.set_viewport(1, geometry(600.));
    let peer = copies(overlays.pending.get(&2).unwrap());
    overlays.open_lane(1, 2);
    assert_eq!(copies(overlays.pending.get(&2).unwrap()), peer);
    check_snapshot(&mut overlays, 1, 600., &[]);
    overlays.open_lane(2, 1);
    check_snapshot(&mut overlays, 2, 320., &copies(&records()));
}
#[test]
fn unsent_snapshot_families_coalesce_without_losing_owner_input_or_outcomes() {
    let mut overlays = fixture();
    for sequence in 1..=1000_u32 {
        overlays.set_viewport(1, geometry(320. + f64::from(sequence)));
        overlays.set_environment(Environment {
            font_size: Scalar::new(10. + f64::from(sequence % 7)).unwrap(),
            ..Default::default()
        });
        for owner in [1, 2] {
            assert!(
                overlays.pending.get(&owner).unwrap().len() <= 5,
                "snapshot backlog exceeded two records beside three ordered non-snapshot records"
            );
        }
    }
    let peer = copies(overlays.pending.get(&2).unwrap());
    overlays.open_lane(1, 1);
    assert_eq!(copies(overlays.pending.get(&2).unwrap()), peer);
    check_snapshot(&mut overlays, 1, 1320., &copies(&records()));
    overlays.open_lane(2, 1);
    check_snapshot(&mut overlays, 2, 320., &copies(&records()));
}

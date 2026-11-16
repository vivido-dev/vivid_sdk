//! Deferred receipt and input bounds preserve complete owner identities.

// Rust guideline compliant 2026-10-07
use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

fn fixture() -> (Overlays, SurfaceIdentity, SurfaceIdentity) {
    let mut overlays = tests::viewport();
    let first = tests::window(&mut overlays, 1);
    let peer = tests::window(&mut overlays, 2);
    (overlays, first, peer)
}
fn submit(overlays: &mut Overlays, owner: u64, revision: u64) -> bool {
    overlays.media_record(
        owner,
        messages::VECTOR_FRAME,
        5,
        &tests::scene(0x00ff00ff, revision),
    )
}
fn queue_event(overlays: &mut Overlays, owner: u64, event: Event) {
    overlays.queue_for(
        owner,
        LaneRecord {
            record_type: messages::OVERLAY_INPUT_EVENT,
            object_id: 1,
            payload: vivid_protocol::overlay::wire::InputEvent {
                address: WindowAddress {
                    context_id: 1,
                    surface_id: 1,
                    generation: 1,
                },
                scene_revision: 1,
                event,
            }
            .payload()
            .unwrap(),
            retention: None,
        },
    );
}
fn queue_key(overlays: &mut Overlays, owner: u64, down: bool) {
    queue_event(
        overlays,
        owner,
        Event::Key {
            physical: 4,
            down,
            repeat: false,
            modifiers: 0,
        },
    );
}
fn queue_button(overlays: &mut Overlays, owner: u64, down: bool) {
    queue_event(
        overlays,
        owner,
        Event::Pointer {
            position: Point::new(2., 3.).unwrap(),
            region: 1,
            button: Some((1, down)),
            modifiers: 0,
            clicks: u8::from(down),
            pressure: None,
        },
    );
}
fn copy(records: &[LaneRecord]) -> Vec<(u16, u64, PayloadMap)> {
    records
        .iter()
        .map(|r| (r.record_type, r.object_id, r.payload.clone()))
        .collect()
}
#[test]
fn unresolved_and_queued_outcomes_share_256_slots_before_first_lane() {
    let (mut overlays, first, peer) = fixture();
    let alive = Arc::new(AtomicBool::new(true));
    assert!(overlays.install_presentation_relay(Arc::downgrade(&alive), u64::MAX));
    assert!(submit(&mut overlays, 2, 1));
    queue_key(&mut overlays, 2, true);
    queue_key(&mut overlays, 2, false);
    let peer_queue = copy(&overlays.pending[&2]);
    for revision in 1..=256 {
        assert!(submit(&mut overlays, 1, revision));
    }
    assert_eq!(overlays.backlogs[&1].unresolved.len(), 1);
    assert_eq!(
        overlays.pending[&1]
            .iter()
            .filter(|r| r.record_type == messages::OVERLAY_SUBMISSION_OUTCOME)
            .count(),
        255
    );
    // Taking records for a writer does not refund them before socket completion.
    let unsent = overlays.take_pending(1);
    assert!(!submit(&mut overlays, 1, 257));
    assert!(overlays.take_lane_overflow(1));
    assert!(!overlays.take_lane_overflow(1));
    assert!(!overlays.pending.contains_key(&1));
    assert!(!overlays.input_enabled(1));
    assert_eq!(copy(&overlays.pending[&2]), peer_queue);
    assert!(overlays.window(peer).is_some());
    assert!(overlays.presentation_origin(peer, 5, 1, 1).is_some());
    assert!(overlays.presentation_origin(first, 5, 1, 256).is_none());
    assert!(alive.load(Ordering::Acquire));
    // A newer authenticated lane starts without resurrecting old submissions.
    overlays.open_lane(1, 2);
    assert!(submit(&mut overlays, 1, 258));
    assert!(
        !submit(&mut overlays, 1, 259),
        "old writer records still own their credits"
    );
    drop(unsent);
    assert!(overlays.take_lane_overflow(1));
    overlays.open_lane(1, 3);
    assert!(submit(&mut overlays, 1, 260));
    assert_eq!(copy(&overlays.pending[&2]), peer_queue);
}
#[test]
fn queued_outcomes_preserve_ordered_keys_and_buttons_across_lane_replacement() {
    let (mut overlays, _, _) = fixture();
    for owner in [1, 2] {
        queue_key(&mut overlays, owner, true);
        queue_button(&mut overlays, owner, true);
        for revision in 1..=256 {
            assert!(submit(&mut overlays, owner, revision));
        }
        queue_button(&mut overlays, owner, false);
        queue_key(&mut overlays, owner, false);
    }
    let first = copy(&overlays.pending[&1]);
    let peer = copy(&overlays.pending[&2]);
    overlays.open_lane(1, 1);
    overlays.close_lane(1);
    overlays.open_lane(1, 2);
    let records = overlays.take_pending(1);
    let retained: Vec<_> = records
        .iter()
        .filter(|r| {
            !matches!(
                r.record_type,
                messages::OVERLAY_ENV_CHANGED | messages::OVERLAY_VIEWPORT_CHANGED
            )
        })
        .collect();
    assert_eq!(retained.len(), 260);
    assert_eq!(
        retained
            .iter()
            .map(|r| (r.record_type, r.object_id, r.payload.clone()))
            .collect::<Vec<_>>(),
        first
            .into_iter()
            .filter(|r| r.0 != messages::OVERLAY_VIEWPORT_CHANGED)
            .collect::<Vec<_>>()
    );
    assert_eq!(retained[0].record_type, messages::OVERLAY_INPUT_EVENT);
    assert_eq!(retained[259].record_type, messages::OVERLAY_INPUT_EVENT);
    assert!(!submit(&mut overlays, 1, 257));
    assert_eq!(copy(&overlays.pending[&2]), peer);
    drop(records);
    assert!(overlays.take_lane_overflow(1));
    overlays.open_lane(1, 3);
    assert!(submit(&mut overlays, 1, 258));
    let next = overlays.take_pending(1);
    assert_eq!(
        next.iter()
            .filter(|r| r.record_type == messages::OVERLAY_SUBMISSION_OUTCOME)
            .count(),
        1
    );
    assert_eq!(copy(&overlays.pending[&2]), peer);
}
#[test]
fn deferred_transition_overflow_revokes_only_its_owner() {
    let (mut overlays, _, peer) = fixture();
    overlays.open_lane(1, 1);
    overlays.open_lane(2, 1);
    queue_key(&mut overlays, 2, true);
    queue_key(&mut overlays, 2, false);
    let peer_queue = copy(&overlays.pending[&2]);
    for index in 0..256 {
        queue_key(&mut overlays, 1, index % 2 == 0);
    }
    assert!(!overlays.backlogs[&1].revoked);
    queue_key(&mut overlays, 1, true);
    assert!(overlays.take_lane_overflow(1));
    assert!(!overlays.pending.contains_key(&1));
    assert!(!overlays.input_leases.contains_key(&1));
    assert!(overlays.input_leases.contains_key(&2));
    assert_eq!(copy(&overlays.pending[&2]), peer_queue);
    assert!(overlays.window(peer).is_some());
}
#[cfg(feature = "vector-render")]
#[test]
fn native_prepared_and_pending_submissions_share_the_outcome_bound() {
    let (mut overlays, first, peer) = fixture();
    overlays.native = Some(native::Native::default());
    assert!(submit(&mut overlays, 2, 1));
    assert!(submit(&mut overlays, 1, 1));
    let submission = overlays.native.as_ref().unwrap().pending[&first].submission;
    let body: Arc<[u8]> = Arc::from(tests::scene(0x00ff00ff, 1));
    let pin = Arc::new(());
    overlays.native.as_mut().unwrap().prepared.insert(
        (first, 1),
        native::Prepared {
            submission,
            body: Arc::downgrade(&body),
            pin: Arc::downgrade(&pin),
            resolved: false,
        },
    );
    for revision in 2..=256 {
        assert!(submit(&mut overlays, 1, revision));
    }
    assert_eq!(
        overlays.backlogs[&1].unresolved.len(),
        2,
        "reserved old scene and current pending each retain one slot"
    );
    assert_eq!(
        overlays.pending[&1]
            .iter()
            .filter(|r| r.record_type == messages::OVERLAY_SUBMISSION_OUTCOME)
            .count(),
        254
    );
    assert!(!submit(&mut overlays, 1, 257));
    assert!(overlays.take_lane_overflow(1));
    assert!(
        !overlays
            .native
            .as_ref()
            .unwrap()
            .pending
            .contains_key(&first)
    );
    assert!(
        !overlays
            .native
            .as_ref()
            .unwrap()
            .prepared
            .contains_key(&(first, 1))
    );
    assert!(
        overlays
            .native
            .as_ref()
            .unwrap()
            .pending
            .contains_key(&peer)
    );
    // A late render completion cannot manufacture a terminal result on a new lane.
    overlays.open_lane(1, 2);
    overlays.native_outcome(submission, first, PresentationOutcome::Presented);
    assert!(
        overlays
            .take_pending(1)
            .iter()
            .all(|r| r.record_type != messages::OVERLAY_SUBMISSION_OUTCOME)
    );
    assert!(submit(&mut overlays, 1, 258));
}

#[test]
fn upstream_window_event_overflow_also_retires_only_its_lane() {
    let (mut overlays, first, peer) = fixture();
    for owner in [1, 2] {
        overlays.open_lane(owner, 1);
        assert!(submit(&mut overlays, owner, 1));
        overlays
            .renew(
                owner,
                0,
                &Renew {
                    lane_generation: 1,
                    watchdog_us: 5_000_000,
                }
                .payload()
                .unwrap(),
            )
            .unwrap();
    }
    overlays.windows.request_focus(first).unwrap();
    overlays.drain_events(owner(1));
    drop(overlays.take_pending(1));
    let peer_queue = copy(&overlays.pending[&2]);
    for index in 0..257 {
        overlays.windows.keyboard(
            1,
            Event::Key {
                physical: 4,
                down: index % 2 == 0,
                repeat: false,
                modifiers: 0,
            },
            false,
        );
    }
    overlays.drain_events(owner(1));
    assert!(overlays.take_lane_overflow(1));
    assert!(!overlays.input_enabled(1));
    assert!(overlays.input_enabled(2));
    assert!(overlays.window(peer).is_some());
    assert_eq!(copy(&overlays.pending[&2]), peer_queue);
}

#[test]
fn forwarded_relay_receipts_and_pending_scene_share_queued_outcome_capacity() {
    let (mut overlays, first, peer) = fixture();
    let alive = Arc::new(AtomicBool::new(true));
    assert!(overlays.install_presentation_relay(Arc::downgrade(&alive), u64::MAX));
    let mut reserved = Vec::new();
    for revision in 1..=4 {
        assert!(submit(&mut overlays, 1, revision));
        let (submission, origin) = overlays.presentation_origin(first, 5, 1, revision).unwrap();
        assert!(matches!(
            overlays.claim_presentation(first, submission, true),
            PresentationClaim::Forwarded
        ));
        reserved.push((submission, origin));
    }
    assert!(submit(&mut overlays, 2, 1));
    let (peer_submission, peer_origin) = overlays.presentation_origin(peer, 5, 1, 1).unwrap();
    assert!(matches!(
        overlays.claim_presentation(peer, peer_submission, true),
        PresentationClaim::Forwarded
    ));
    for revision in 5..=256 {
        assert!(submit(&mut overlays, 1, revision));
    }
    assert_eq!(overlays.backlogs[&1].unresolved.len(), 5);
    assert_eq!(
        overlays.pending[&1]
            .iter()
            .filter(|r| r.record_type == messages::OVERLAY_SUBMISSION_OUTCOME)
            .count(),
        251
    );
    assert!(!submit(&mut overlays, 1, 257));
    assert!(overlays.take_lane_overflow(1));
    assert!(
        reserved
            .iter()
            .all(|(_, origin)| origin.upgrade().is_none())
    );
    assert!(peer_origin.upgrade().is_some());
    overlays.open_lane(1, 2);
    for (submission, _) in reserved {
        assert!(!overlays.complete_presentation(first, submission, PresentationOutcome::Presented));
    }
    assert!(overlays.complete_presentation(peer, peer_submission, PresentationOutcome::Presented));
    assert_eq!(overlays.window(peer).unwrap().scene_revision, 1);
    assert!(
        overlays
            .take_pending(1)
            .iter()
            .all(|r| r.record_type != messages::OVERLAY_SUBMISSION_OUTCOME)
    );
    assert!(submit(&mut overlays, 1, 258));
}

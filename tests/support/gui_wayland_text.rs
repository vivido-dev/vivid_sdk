//! Actual text-input-v3 clients with visible acknowledgements of native IME edits.

// Rust guideline compliant 2026-10-07
use super::*;
use vivid_sdk::gui_wayland::{TextEdit, TextInputState};
use wayland_protocols::wp::text_input::zv3::client::{
    zwp_text_input_manager_v3::ZwpTextInputManagerV3,
    zwp_text_input_v3::{self as input, ZwpTextInputV3},
};

pub(super) const INITIAL: &str = "é😀漢字";
#[derive(Default)]
pub(super) struct ImeFixture {
    manager: Option<ZwpTextInputManagerV3>,
    input: Option<ZwpTextInputV3>,
    serial: u32,
    text: String,
    cursor: usize,
    anchor: usize,
    preedit: Option<String>,
    commit: Option<String>,
    delete: (u32, u32),
    count: u8,
    without_context: bool,
}
fn start() -> (WaylandHost, Client, Frame, TextInputState) {
    let mut host = WaylandHost::new(64, 48).unwrap();
    let client = client_with(&host, &[("VIVID_TREE_IME", "1")]);
    let frame = next(&mut host);
    host.set_active(true);
    present(&mut host, &frame);
    let state = metadata(&mut host);
    (host, client, frame, state)
}
fn metadata(host: &mut WaylandHost) -> TextInputState {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(frame) = host.dispatch().unwrap() {
            present(host, &frame);
        }
        if let Some(state) = host.text_input() {
            return state;
        }
        assert!(
            Instant::now() < deadline,
            "native text input metadata timed out"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn edited(host: &mut WaylandHost, count: u8, preview: u8) -> TextInputState {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(frame) = host.dispatch().unwrap() {
            let matched = pixel(&frame, 0, 0) == [240, count, preview, 255];
            present(host, &frame);
            if matched {
                return host.text_input().unwrap();
            }
        }
        assert!(
            Instant::now() < deadline,
            "native text edit acknowledgement timed out"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}
#[test]
fn native_ime_preedit_commit_and_surrounding_edits_are_atomic_and_utf8_exact() {
    let (mut host, _client, _frame, state) = start();
    assert_eq!(state.context.as_ref().unwrap().text, INITIAL);
    assert_eq!(state.caret, Some([12, 13, 2, 14]));
    let preview = "拼😀";
    assert!(
        host.text_edit(
            state.target,
            TextEdit {
                preedit: Some((preview.into(), Some(3..7))),
                ..TextEdit::default()
            }
        )
        .unwrap()
    );
    let after = edited(&mut host, 1, preview.len() as u8);
    assert_eq!(after.context.as_ref().unwrap().text, "é漢字");
    assert!(state.target.same_editor(after.target));
    assert!(
        !host
            .text_edit(
                state.target,
                TextEdit {
                    replacement: Some(0..6),
                    commit: Some("old context".into()),
                    ..TextEdit::default()
                }
            )
            .unwrap()
    );
    assert!(
        host.text_edit(
            after.target,
            TextEdit {
                replacement: Some(0..5),
                commit: Some("好".into()),
                ..TextEdit::default()
            }
        )
        .unwrap()
    );
    let committed = edited(&mut host, 2, 0);
    assert_eq!(committed.context.as_ref().unwrap().text, "好字");
    assert_eq!(committed.context.as_ref().unwrap().cursor, 3);
    assert!(
        host.text_edit(
            committed.target,
            TextEdit {
                preedit: Some(("候補".into(), None)),
                ..TextEdit::default()
            }
        )
        .unwrap()
    );
    let composing = edited(&mut host, 3, 6);
    assert!(
        host.text_edit(composing.target, TextEdit::default())
            .unwrap()
    );
    let cancelled = edited(&mut host, 4, 0);
    assert_eq!(cancelled.context.as_ref().unwrap().text, "好字");
    assert!(!format!("{state:?}").contains(INITIAL));
    assert!(
        !format!(
            "{:?}",
            TextEdit {
                commit: Some("private IME".into()),
                ..TextEdit::default()
            }
        )
        .contains("private IME")
    );
}
#[test]
fn native_ime_enable_disable_focus_and_external_changes_retire_exact_editor_authority() {
    let (mut host, _client, frame, state) = start();
    let (mut peer, _peer, _other, other) = start();
    assert!(!peer.text_edit(state.target, TextEdit::default()).unwrap());
    key(&mut host, frame.target, 112);
    let pending = next(&mut host);
    present(&mut host, &pending);
    assert_eq!(
        host.text_input().unwrap().target,
        state.target,
        "uncommitted disable changed current state"
    );
    key(&mut host, pending.target, 113);
    let disabled = next(&mut host);
    present(&mut host, &disabled);
    assert!(host.text_input().is_none());
    assert!(!host.text_edit(state.target, TextEdit::default()).unwrap());
    key(&mut host, disabled.target, 111);
    let fresh = metadata(&mut host);
    assert!(!state.target.same_editor(fresh.target));
    key(&mut host, disabled.target, 110);
    let external = metadata_changed(&mut host, fresh.target);
    assert!(!fresh.target.same_editor(external.target));
    assert!(
        !host
            .text_edit(
                fresh.target,
                TextEdit {
                    commit: Some("stale".into()),
                    ..TextEdit::default()
                }
            )
            .unwrap()
    );
    host.set_active(false);
    host.set_active(true);
    assert!(host.text_input().is_none());
    assert!(
        !host
            .text_edit(external.target, TextEdit::default())
            .unwrap()
    );
    let restored = metadata(&mut host);
    assert!(!external.target.same_editor(restored.target));
    assert!(
        host.text_edit(
            restored.target,
            TextEdit {
                preedit: Some(("old".into(), Some(3..3))),
                ..TextEdit::default()
            }
        )
        .unwrap()
    );
    let preview = edited(&mut host, 1, 3);
    key(&mut host, frame.target, 110);
    let changed = metadata_changed(&mut host, preview.target);
    assert!(!preview.target.same_editor(changed.target));
    assert_eq!(edited(&mut host, 2, 0).context.unwrap().text, "é漢字");
    assert!(
        peer.text_edit(
            other.target,
            TextEdit {
                commit: Some("peer".into()),
                ..TextEdit::default()
            }
        )
        .unwrap()
    );
    let survivor = edited(&mut peer, 1, 0);
    assert_eq!(survivor.context.unwrap().text, "épeer漢字");
}
fn metadata_changed(
    host: &mut WaylandHost,
    old: vivid_sdk::gui_wayland::TextTarget,
) -> TextInputState {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let current = metadata(host);
        if current.target != old {
            return current;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
}
#[test]
fn invalid_native_ime_metadata_and_object_budgets_preserve_the_peer_display() {
    for code in [114, 115, 116, 118, 124, 125] {
        let (mut host, mut bad, frame, _) = start();
        let (mut peer, _peer, _other, other) = start();
        key(&mut host, frame.target, code);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(frame) = host.dispatch().unwrap() {
                present(&mut host, &frame);
            }
            if bad.0.try_wait().unwrap().is_some() {
                break;
            }
            assert!(Instant::now() < deadline, "malformed text client survived");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(host.text_input().is_none());
        assert!(
            peer.text_edit(
                other.target,
                TextEdit {
                    commit: Some("safe".into()),
                    ..TextEdit::default()
                }
            )
            .unwrap()
        );
        assert_eq!(edited(&mut peer, 1, 0).context.unwrap().text, "ésafe漢字");
    }
}

#[test]
fn native_ime_content_types_and_missing_surrounding_text_preserve_editor_boundaries() {
    let (mut host, _client, frame, initial) = start();
    key(&mut host, frame.target, 121);
    let sensitive = metadata_changed(&mut host, initial.target);
    assert!(sensitive.sensitive);
    assert!(!sensitive.configuration.suggestions);
    assert!(!initial.target.same_editor(sensitive.target));
    key(&mut host, frame.target, 122);
    let unsupported = metadata_changed(&mut host, sensitive.target);
    assert!(unsupported.context.is_none());
    assert!(unsupported.multiline);
    assert_eq!(
        unsupported.configuration.input_action,
        vivid_sdk::gui::TextInputAction::Enter
    );
    assert!(!unsupported.sensitive);
    assert!(
        host.text_edit(
            unsupported.target,
            TextEdit {
                replacement: Some(0..0),
                commit: Some("unsupported".into()),
                ..TextEdit::default()
            }
        )
        .is_err()
    );
    assert!(
        host.text_edit(
            unsupported.target,
            TextEdit {
                commit: Some("文".into()),
                ..TextEdit::default()
            }
        )
        .unwrap()
    );
    assert!(edited(&mut host, 1, 0).context.is_none());
}

#[test]
fn destroyed_native_ime_objects_release_capacity_without_reusing_editor_authority() {
    let (mut host, _client, frame, mut state) = start();
    let stale = state.target;
    for _ in 0..20 {
        key(&mut host, frame.target, 123);
        let current = metadata_changed(&mut host, state.target);
        assert!(!state.target.same_editor(current.target));
        assert!(!host.text_edit(state.target, TextEdit::default()).unwrap());
        state = current;
    }
    assert!(!host.text_edit(stale, TextEdit::default()).unwrap());
    assert!(
        host.text_edit(
            state.target,
            TextEdit {
                commit: Some("fresh".into()),
                ..TextEdit::default()
            }
        )
        .unwrap()
    );
    assert_eq!(edited(&mut host, 1, 0).context.unwrap().text, "éfresh漢字");
}
#[test]
fn invalid_host_text_edits_change_no_native_state_and_caret_is_clipped() {
    let (mut host, _client, frame, state) = start();
    for edit in [
        TextEdit {
            commit: Some("x".repeat(4001)),
            ..TextEdit::default()
        },
        TextEdit {
            commit: Some("bad\0text".into()),
            ..TextEdit::default()
        },
        TextEdit {
            replacement: Some(3..6),
            commit: Some("wrong".into()),
            ..TextEdit::default()
        },
        TextEdit {
            preedit: Some(("😀".into(), Some(1..2))),
            ..TextEdit::default()
        },
    ] {
        assert!(host.text_edit(state.target, edit).is_err());
    }
    assert_eq!(host.text_input().unwrap(), state);
    key(&mut host, frame.target, 120);
    let moved = next(&mut host);
    present(&mut host, &moved);
    assert_eq!(host.text_input().unwrap().caret, Some([63, 47, 1, 1]));
    assert!(
        host.text_edit(
            state.target,
            TextEdit {
                commit: Some("valid".into()),
                ..TextEdit::default()
            }
        )
        .unwrap()
    );
    assert_eq!(edited(&mut host, 1, 0).context.unwrap().text, "évalid漢字");
}
#[test]
fn native_ime_resize_requires_new_presentation_and_preserves_an_independent_owner() {
    let (mut host, _client, initial, state) = start();
    let (mut peer, _peer, _other, other) = start();
    host.resize(80, 60).unwrap();
    assert!(host.text_input().is_none());
    assert!(
        !host
            .text_edit(
                state.target,
                TextEdit {
                    commit: Some("old mapping".into()),
                    ..TextEdit::default()
                }
            )
            .unwrap()
    );
    let resized = next(&mut host);
    assert_ne!(resized.target, initial.target);
    assert!(host.text_input().is_none());
    present(&mut host, &resized);
    let current = metadata(&mut host);
    assert!(!current.target.same_editor(state.target));
    assert!(
        host.text_edit(
            current.target,
            TextEdit {
                commit: Some("resized".into()),
                ..TextEdit::default()
            }
        )
        .unwrap()
    );
    assert_eq!(
        edited(&mut host, 1, 0).context.unwrap().text,
        "éresized漢字"
    );
    assert!(
        peer.text_edit(
            other.target,
            TextEdit {
                commit: Some("peer".into()),
                ..TextEdit::default()
            }
        )
        .unwrap()
    );
    assert_eq!(edited(&mut peer, 1, 0).context.unwrap().text, "épeer漢字");
}

impl Fixture {
    pub(super) fn ime_global(
        &mut self,
        registry: &wl_registry::WlRegistry,
        name: u32,
        interface: &str,
        qh: &QueueHandle<Self>,
    ) {
        if interface == "zwp_text_input_manager_v3" && std::env::var_os("VIVID_TREE_IME").is_some()
        {
            self.ime.manager = Some(registry.bind(name, 1, qh, ()));
        }
        if self.ime.input.is_none()
            && let (Some(manager), Some(seat)) = (&self.ime.manager, &self.seat)
        {
            self.ime.input = Some(manager.get_text_input(seat, qh, ()));
        }
    }
    pub(super) fn ime_key(&mut self, code: u32, qh: &QueueHandle<Self>) -> bool {
        let Some(input) = self.ime.input.clone() else {
            return false;
        };
        let code = match code {
            67 => 121,
            68 => 111,
            other => other,
        };
        match code {
            110 => {
                input.set_text_change_cause(input::ChangeCause::Other);
                self.ime_context();
                self.ime_commit();
            }
            111 => {
                input.enable();
                self.ime_context();
                self.ime_commit();
            }
            112 => input.disable(),
            113 => self.ime_commit(),
            114 => input.set_surrounding_text("x".repeat(4001), 0, 0),
            115 => input.set_surrounding_text(INITIAL.into(), 3, 2),
            116 => input.set_cursor_rectangle(i32::MIN, 0, 1, 1),
            118 => {
                for _ in 0..17 {
                    self.ime.manager.as_ref().unwrap().get_text_input(
                        self.seat.as_ref().unwrap(),
                        qh,
                        (),
                    );
                }
            }
            120 => {
                input.set_cursor_rectangle(63, 47, 200, 200);
                self.ime_commit();
            }
            121 => {
                self.ime.without_context = false;
                input.enable();
                input.set_content_type(
                    input::ContentHint::HiddenText | input::ContentHint::SensitiveData,
                    input::ContentPurpose::Password,
                );
                self.ime_context();
                self.ime_commit();
            }
            122 => {
                self.ime.without_context = true;
                input.enable();
                input.set_content_type(
                    input::ContentHint::Multiline,
                    input::ContentPurpose::Terminal,
                );
                self.ime_commit();
            }
            123 => {
                self.ime.input.take().unwrap().destroy();
                self.ime.serial = 0;
                self.ime.input = Some(self.ime.manager.as_ref().unwrap().get_text_input(
                    self.seat.as_ref().unwrap(),
                    qh,
                    (),
                ));
            }
            124 => {
                let _ = input.send_request(input::Request::SetContentType {
                    hint: WEnum::Value(input::ContentHint::None),
                    purpose: WEnum::Unknown(u32::MAX),
                });
            }
            125 => {
                let _ = input.send_request(input::Request::SetTextChangeCause {
                    cause: WEnum::Unknown(u32::MAX),
                });
            }
            _ => return false,
        }
        self.root_color = Some([code as u8, 20, 30, 255]);
        self.draw(qh);
        true
    }
    fn ime_context(&self) {
        if self.ime.without_context {
            return;
        }
        self.ime.input.as_ref().unwrap().set_surrounding_text(
            self.ime.text.clone(),
            self.ime.cursor as i32,
            self.ime.anchor as i32,
        );
    }
    fn ime_commit(&mut self) {
        self.ime.serial = self.ime.serial.wrapping_add(1);
        self.ime.input.as_ref().unwrap().commit();
    }
}
impl Dispatch<ZwpTextInputV3, ()> for Fixture {
    fn event(
        state: &mut Self,
        object: &ZwpTextInputV3,
        event: input::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            input::Event::Enter { .. } => {
                if state.ime.input.as_ref() != Some(object) {
                    return;
                }
                state.ime.without_context = false;
                state.ime.text = INITIAL.into();
                state.ime.cursor = 6;
                state.ime.anchor = 2;
                object.enable();
                state.ime_context();
                object.set_cursor_rectangle(12, 13, 2, 14);
                state.ime_commit();
            }
            input::Event::Leave { .. } => {
                state.ime.preedit = None;
                state.ime.commit = None;
                state.ime.delete = (0, 0);
            }
            input::Event::PreeditString {
                text,
                cursor_begin,
                cursor_end,
            } => {
                if let Some(text) = &text {
                    assert!(
                        cursor_begin == -1 && cursor_end == -1
                            || text.is_char_boundary(cursor_begin as usize)
                                && text.is_char_boundary(cursor_end as usize)
                    );
                }
                state.ime.preedit = text;
            }
            input::Event::CommitString { text } => state.ime.commit = text,
            input::Event::DeleteSurroundingText {
                before_length,
                after_length,
            } => state.ime.delete = (before_length, after_length),
            input::Event::Done { serial } => {
                assert_eq!(
                    serial, state.ime.serial,
                    "native done used the wrong committed client serial"
                );
                let low = state.ime.cursor.min(state.ime.anchor);
                let high = state.ime.cursor.max(state.ime.anchor);
                if state.ime.commit.is_some()
                    || state
                        .ime
                        .preedit
                        .as_ref()
                        .is_some_and(|text| !text.is_empty())
                    || state.ime.delete != (0, 0)
                {
                    let text = state.ime.commit.take().unwrap_or_default();
                    let start = low - state.ime.delete.0 as usize;
                    let end = high + state.ime.delete.1 as usize;
                    state.ime.text.replace_range(start..end, &text);
                    state.ime.cursor = start + text.len();
                    state.ime.anchor = state.ime.cursor;
                }
                state.ime.delete = (0, 0);
                state.ime.count += 1;
                state.root_color = Some([
                    240,
                    state.ime.count,
                    state.ime.preedit.as_ref().map_or(0, |text| text.len()) as u8,
                    255,
                ]);
                state.ime_context();
                state.ime_commit();
                state.draw(qh);
            }
            _ => (),
        }
    }
}
delegate_noop!(Fixture: ignore ZwpTextInputManagerV3);

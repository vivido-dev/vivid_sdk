//! Native popup clients exercise the same display and input path as subsurfaces.

// Rust guideline compliant 2026-10-07
use super::*;
use wayland_protocols::xdg::shell::client::{xdg_popup, xdg_positioner};

pub(super) struct NativePopup {
    surface: wl_surface::WlSurface,
    xdg: xdg_surface::XdgSurface,
    role: xdg_popup::XdgPopup,
    geometry: [i32; 4],
    margin: [i32; 2],
    hold: bool,
    serial: Option<u32>,
    alive: bool,
}

fn colored(host: &mut WaylandHost, point: [u32; 2], color: [u8; 4]) -> Frame {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let frame = next(host);
        if pixel(&frame, point[0], point[1]) == color {
            return frame;
        }
        present(host, &frame);
        assert!(
            Instant::now() < deadline,
            "popup pixels did not converge: {:?}",
            pixel(&frame, point[0], point[1])
        );
    }
}
fn start() -> (WaylandHost, Client, Frame) {
    let mut host = WaylandHost::new(64, 48).unwrap();
    let child = client(&host);
    let frame = next(&mut host);
    host.set_active(true);
    present(&mut host, &frame);
    (host, child, frame)
}

#[test]
fn popup_geometry_shadows_hit_testing_and_grab_focus_follow_presentation() {
    let (mut host, _child, initial) = start();
    key(&mut host, initial.target, 61);
    let popup = colored(&mut host, [22, 18], [0, 210, 0, 255]);
    assert_ne!(popup.target, initial.target);
    assert!(host.input_target().is_none());
    assert!(
        !host
            .input(
                initial.target,
                Input::Key {
                    code: 100,
                    pressed: true
                }
            )
            .unwrap()
    );
    present(&mut host, &popup);
    key(&mut host, popup.target, 100);
    let keyboard = colored(&mut host, [0, 0], [10, 10, 20, 255]);
    present(&mut host, &keyboard);
    assert!(
        host.input(keyboard.target, Input::Motion { x: 23., y: 19. })
            .unwrap()
    );
    assert!(
        host.input(
            keyboard.target,
            Input::Button {
                code: 0x110,
                pressed: true
            }
        )
        .unwrap()
    );
    let clicked = colored(&mut host, [0, 0], [110, 1, 1, 255]);
    present(&mut host, &clicked);
    assert!(
        host.input(
            clicked.target,
            Input::Button {
                code: 0x110,
                pressed: false
            }
        )
        .unwrap()
    );
    key(&mut host, clicked.target, 63);
    let nested = colored(&mut host, [31, 24], [0, 0, 220, 255]);
    present(&mut host, &nested);
    key(&mut host, nested.target, 100);
    let focused = colored(&mut host, [0, 0], [11, 10, 20, 255]);
    present(&mut host, &focused);
    key(&mut host, focused.target, 68);
    let restored = colored(&mut host, [31, 24], [0, 210, 0, 255]);
    present(&mut host, &restored);
    key(&mut host, restored.target, 100);
    let parent = colored(&mut host, [0, 0], [10, 10, 20, 255]);
    assert_ne!(parent.target, nested.target);
}

#[test]
fn nongrabbing_popup_keeps_root_keyboard_and_reposition_waits_for_ack_and_commit() {
    let (mut host, _child, initial) = start();
    key(&mut host, initial.target, 60);
    let popup = colored(&mut host, [22, 18], [0, 210, 0, 255]);
    present(&mut host, &popup);
    key(&mut host, popup.target, 100);
    let root = colored(&mut host, [0, 0], [1, 10, 20, 255]);
    present(&mut host, &root);
    key(&mut host, root.target, 65);
    let pending = next(&mut host);
    assert_eq!(pixel(&pending, 22, 18), [0, 210, 0, 255]);
    assert_eq!(pending.target, root.target);
    present(&mut host, &pending);
    key(&mut host, pending.target, 66);
    let acked = next(&mut host);
    assert_eq!(pixel(&acked, 22, 18), [0, 210, 0, 255]);
    assert_eq!(acked.target, pending.target, "ACK alone moved the popup");
    present(&mut host, &acked);
    key(&mut host, acked.target, 67);
    let moved = colored(&mut host, [32, 26], [0, 210, 0, 255]);
    assert_ne!(moved.target, acked.target);
    assert_ne!(pixel(&moved, 22, 18), [0, 210, 0, 255]);
    assert!(host.input_target().is_none());
}

#[test]
fn popup_focus_loss_dismisses_nested_roles_and_stale_serial_cannot_regrab() {
    let (mut host, _child, initial) = start();
    key(&mut host, initial.target, 74);
    let saved = next(&mut host);
    present(&mut host, &saved);
    host.set_active(false);
    host.set_active(true);
    key(&mut host, saved.target, 75);
    let denied = colored(&mut host, [0, 0], [80, 81, 82, 255]);
    assert_ne!(pixel(&denied, 22, 18), [0, 210, 0, 255]);
    present(&mut host, &denied);
    key(&mut host, denied.target, 61);
    let parent = colored(&mut host, [22, 18], [0, 210, 0, 255]);
    present(&mut host, &parent);
    key(&mut host, parent.target, 63);
    let nested = colored(&mut host, [31, 24], [0, 0, 220, 255]);
    present(&mut host, &nested);
    host.set_active(false);
    host.set_active(true);
    assert!(
        host.input_target().is_none(),
        "fast focus return restored dismissed popup authority"
    );
    assert!(
        !host
            .input(
                nested.target,
                Input::Key {
                    code: 100,
                    pressed: true
                }
            )
            .unwrap()
    );
    let dismissed = colored(&mut host, [0, 0], [80, 81, 82, 255]);
    assert_ne!(pixel(&dismissed, 22, 18), [0, 210, 0, 255]);
    assert_ne!(dismissed.target, nested.target);
    assert!(host.input_target().is_none());
}

#[test]
fn malformed_popup_positioners_and_popup_budgets_preserve_the_peer_display() {
    for command in [70, 71, 76, 83] {
        rejected_client_preserves_peer(command);
    }
}

#[test]
fn popup_unmap_requires_a_new_configure_and_noop_dismiss_keeps_static_input() {
    let (mut host, _child, initial) = start();
    assert!(host.input(initial.target, Input::DismissPopups).unwrap());
    assert_eq!(host.input_target().unwrap().0, initial.target);
    key(&mut host, initial.target, 60);
    let popup = colored(&mut host, [22, 18], [0, 210, 0, 255]);
    present(&mut host, &popup);
    key(&mut host, popup.target, 81);
    let unmapped = next(&mut host);
    assert_ne!(pixel(&unmapped, 22, 18), [0, 210, 0, 255]);
    assert_ne!(unmapped.target, popup.target);
    present(&mut host, &unmapped);
    key(&mut host, unmapped.target, 82);
    let remapped = colored(&mut host, [22, 18], [0, 210, 0, 255]);
    assert_ne!(remapped.target, unmapped.target);
}

#[test]
fn outside_press_and_host_resize_dismiss_popup_grabs_without_client_activation() {
    for resize in [false, true] {
        let (mut host, _child, initial) = start();
        key(&mut host, initial.target, 61);
        let popup = colored(&mut host, [22, 18], [0, 210, 0, 255]);
        present(&mut host, &popup);
        if resize {
            host.resize(70, 52).unwrap();
        } else {
            assert!(host.input(popup.target, Input::Leave).unwrap());
            assert!(
                !host
                    .input(
                        popup.target,
                        Input::Button {
                            code: 0x110,
                            pressed: true
                        }
                    )
                    .unwrap()
            );
        }
        assert!(host.input_target().is_none());
        let dismissed = colored(&mut host, [0, 0], [80, 81, 82, 255]);
        assert_ne!(pixel(&dismissed, 22, 18), [0, 210, 0, 255]);
        assert_ne!(dismissed.target, popup.target);
    }
}

#[test]
fn explicit_popup_dismissal_revokes_input_and_removes_pixels() {
    let (mut host, _child, initial) = start();
    key(&mut host, initial.target, 61);
    let popup = colored(&mut host, [22, 18], [0, 210, 0, 255]);
    present(&mut host, &popup);
    assert!(host.input(popup.target, Input::DismissPopups).unwrap());
    assert!(host.input_target().is_none());
    assert!(
        !host
            .input(
                popup.target,
                Input::Key {
                    code: 100,
                    pressed: true
                }
            )
            .unwrap()
    );
    let dismissed = colored(&mut host, [0, 0], [80, 81, 82, 255]);
    assert_ne!(pixel(&dismissed, 22, 18), [0, 210, 0, 255]);
}

#[test]
fn clipped_popup_callbacks_wait_for_visible_host_presentation() {
    let mut host = WaylandHost::new(64, 48).unwrap();
    let _child = client_with(
        &host,
        &[("VIVID_POPUP_CALLBACK", "1"), ("VIVID_POPUP_CLIPPED", "1")],
    );
    let initial = next(&mut host);
    host.set_active(true);
    present(&mut host, &initial);
    key(&mut host, initial.target, 60);
    for _ in 0..40 {
        if let Some(frame) = host.dispatch().unwrap() {
            assert_eq!(pixel(&frame, 0, 0), [10, 20, 30, 255]);
            present(&mut host, &frame);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    key(&mut host, initial.target, 64);
    let visible = colored(&mut host, [32, 26], [0, 210, 0, 255]);
    assert_eq!(pixel(&visible, 0, 0), [10, 20, 30, 255]);
    for _ in 0..10 {
        assert!(host.dispatch().unwrap().is_none());
        std::thread::sleep(Duration::from_millis(2));
    }
    present(&mut host, &visible);
    let callback = colored(&mut host, [0, 0], [80, 81, 82, 255]);
    assert_eq!(pixel(&callback, 32, 26), [0, 210, 0, 255]);
}

#[test]
fn sliding_and_reactive_popups_follow_the_current_root_canvas() {
    let mut host = WaylandHost::new(64, 48).unwrap();
    let _child = client_with(
        &host,
        &[
            ("VIVID_POPUP_CLIPPED", "1"),
            ("VIVID_POPUP_SLIDE", "1"),
            ("VIVID_POPUP_REACTIVE", "1"),
        ],
    );
    let initial = next(&mut host);
    host.set_active(true);
    present(&mut host, &initial);
    key(&mut host, initial.target, 60);
    let slid = colored(&mut host, [50, 37], [0, 210, 0, 255]);
    present(&mut host, &slid);
    key(&mut host, slid.target, 80);
    let reactive = colored(&mut host, [26, 21], [0, 210, 0, 255]);
    assert_eq!(reactive.logical_size, [40, 32]);
    assert_ne!(reactive.target, slid.target);
}

impl Fixture {
    fn popup_positioner(
        &self,
        qh: &QueueHandle<Self>,
        size: [i32; 2],
        point: [i32; 2],
    ) -> xdg_positioner::XdgPositioner {
        let positioner = self.shell.as_ref().unwrap().create_positioner(qh, ());
        positioner.set_size(size[0], size[1]);
        positioner.set_anchor_rect(point[0], point[1], 1, 1);
        positioner.set_anchor(xdg_positioner::Anchor::TopLeft);
        positioner.set_gravity(xdg_positioner::Gravity::BottomRight);
        if point == [i32::MAX, i32::MAX] {
            positioner.set_offset(i32::MAX, i32::MAX);
        }
        if std::env::var_os("VIVID_POPUP_SLIDE").is_some() {
            positioner.set_constraint_adjustment(
                xdg_positioner::ConstraintAdjustment::SlideX
                    | xdg_positioner::ConstraintAdjustment::SlideY,
            );
        }
        if std::env::var_os("VIVID_POPUP_REACTIVE").is_some() {
            positioner.set_reactive();
        }
        positioner
    }
    fn open_popup(
        &mut self,
        qh: &QueueHandle<Self>,
        nested: bool,
        serial: Option<u32>,
        point: [i32; 2],
    ) {
        if !nested {
            self.popup_done_tag = None;
            self.popup_order_valid = true;
        }
        let index = self.popups.len();
        let tag = 10 + index as u8;
        let parent = if nested {
            &self.popups.iter().rev().find(|p| p.alive).unwrap().xdg
        } else {
            self.xdg.as_ref().unwrap()
        };
        let size = if nested { [6, 4] } else { [12, 8] };
        let positioner = self.popup_positioner(qh, size, point);
        let surface = self.compositor.as_ref().unwrap().create_surface(qh, tag);
        let xdg = self
            .shell
            .as_ref()
            .unwrap()
            .get_xdg_surface(&surface, qh, tag);
        let role = xdg.get_popup(Some(parent), &positioner, qh, tag);
        positioner.destroy();
        if let Some(serial) = serial {
            role.grab(self.seat.as_ref().unwrap(), serial);
        }
        let margin = if nested { [1, 1] } else { [2, 3] };
        xdg.set_window_geometry(margin[0], margin[1], size[0], size[1]);
        surface.commit();
        self.popups.push(NativePopup {
            surface,
            xdg,
            role,
            geometry: [0, 0, size[0], size[1]],
            margin,
            hold: false,
            serial: None,
            alive: true,
        });
    }
    fn draw_popup(&self, index: usize, qh: &QueueHandle<Self>) {
        let popup = &self.popups[index];
        if !popup.alive {
            return;
        }
        let [_, _, width, height] = popup.geometry;
        popup
            .xdg
            .set_window_geometry(popup.margin[0], popup.margin[1], width, height);
        if std::env::var_os("VIVID_POPUP_CALLBACK").is_some() {
            popup.surface.frame(qh, ());
        }
        self.buffer(
            &popup.surface,
            width + popup.margin[0] * 2,
            height + popup.margin[1] * 2,
            1,
            if popup.margin == [1, 1] {
                [0, 0, 220, 255]
            } else {
                [0, 210, 0, 255]
            },
            qh,
        );
    }
    pub(super) fn popup_configure(&mut self, tag: u8, serial: u32, qh: &QueueHandle<Self>) {
        let index = usize::from(tag - 10);
        let popup = &mut self.popups[index];
        if !popup.alive {
            return;
        }
        if popup.hold {
            popup.serial = Some(serial);
        } else {
            popup.xdg.ack_configure(serial);
            self.draw_popup(index, qh);
            if self.saved_serial == u32::MAX - 1 {
                self.saved_serial = 0;
                self.popups[index].surface.attach(None, 0, 0);
                self.popups[index].surface.commit();
                // Reusing the previous lifecycle's ACK must not map pixels.
                self.draw_popup(index, qh);
            }
        }
    }
    pub(super) fn popup_key(&mut self, key: u32, serial: u32, qh: &QueueHandle<Self>) -> bool {
        match key {
            60 | 61 | 75 => {
                self.xdg.as_ref().unwrap().set_window_geometry(4, 5, 56, 38);
                self.root.as_ref().unwrap().commit();
                self.open_popup(
                    qh,
                    false,
                    if key == 60 {
                        None
                    } else {
                        Some(if key == 75 { self.saved_serial } else { serial })
                    },
                    if std::env::var_os("VIVID_POPUP_CLIPPED").is_some() {
                        [70, 60]
                    } else {
                        [20, 16]
                    },
                );
            }
            62 | 63 => self.open_popup(qh, true, (key == 63).then_some(serial), [8, 4]),
            64 => {
                let positioner = self.popup_positioner(qh, [12, 8], [30, 24]);
                self.popups
                    .iter()
                    .find(|p| p.alive)
                    .unwrap()
                    .role
                    .reposition(&positioner, 654);
                positioner.destroy();
            }
            65 => {
                let positioner = self.popup_positioner(qh, [12, 8], [30, 24]);
                let popup = self.popups.iter_mut().find(|p| p.alive).unwrap();
                popup.hold = true;
                popup.role.reposition(&positioner, 987);
                positioner.destroy();
                self.draw(qh);
            }
            66 => {
                let popup = self.popups.iter_mut().find(|p| p.alive).unwrap();
                popup
                    .xdg
                    .ack_configure(popup.serial.take().expect("held popup configure"));
                self.draw(qh);
            }
            67 => {
                let index = self.popups.iter().position(|p| p.alive).unwrap();
                self.draw_popup(index, qh);
            }
            68 => {
                let popup = self.popups.iter_mut().rev().find(|p| p.alive).unwrap();
                popup.role.destroy();
                popup.xdg.destroy();
                popup.surface.destroy();
                popup.alive = false;
            }
            70 => self.open_popup(qh, false, None, [i32::MAX, i32::MAX]),
            71 => {
                for _ in 0..9 {
                    self.open_popup(qh, false, None, [20, 16]);
                }
            }
            74 => {
                self.saved_serial = serial;
                self.draw(qh);
            }
            76 => {
                // First map a popup, then flood its queue from its configure handler.
                self.open_popup(qh, false, None, [20, 16]);
                self.saved_serial = u32::MAX;
            }
            80 => {
                self.root_extent = [40, 32];
                self.draw(qh);
            }
            81 => {
                let popup = self.popups.iter().find(|p| p.alive).unwrap();
                popup.surface.attach(None, 0, 0);
                popup.surface.commit();
                self.draw(qh);
            }
            82 => self
                .popups
                .iter()
                .find(|p| p.alive)
                .unwrap()
                .surface
                .commit(),
            83 => {
                self.open_popup(qh, false, None, [20, 16]);
                self.saved_serial = u32::MAX - 1;
            }
            100 => {
                self.root_color = Some([self.keyboard_focus, 10, 20, 255]);
                self.draw(qh);
            }
            _ => return false,
        }
        true
    }
}
impl Dispatch<xdg_popup::XdgPopup, u8> for Fixture {
    fn event(
        state: &mut Self,
        _: &xdg_popup::XdgPopup,
        event: xdg_popup::Event,
        tag: &u8,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let index = usize::from(*tag - 10);
        match event {
            xdg_popup::Event::Configure {
                x,
                y,
                width,
                height,
            } => {
                state.popups[index].geometry = [x, y, width, height];
                if state.saved_serial == u32::MAX {
                    state.saved_serial = 0;
                    let positioner = state.popup_positioner(qh, [12, 8], [20, 16]);
                    for token in 0..32 {
                        state.popups[index].role.reposition(&positioner, token);
                    }
                    positioner.destroy();
                }
            }
            xdg_popup::Event::PopupDone => {
                state.popup_order_valid &= state.popup_done_tag.is_none_or(|old| old > *tag);
                state.popup_done_tag = Some(*tag);
                let popup = &mut state.popups[index];
                if popup.alive {
                    popup.role.destroy();
                    popup.xdg.destroy();
                    popup.surface.destroy();
                    popup.alive = false;
                    state.root_color = Some(if state.popup_order_valid {
                        [80, 81, 82, 255]
                    } else {
                        [90, 91, 92, 255]
                    });
                    state.draw(qh);
                }
            }
            _ => (),
        }
    }
}
delegate_noop!(Fixture: ignore xdg_positioner::XdgPositioner);

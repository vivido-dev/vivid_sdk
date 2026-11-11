//! Named cursor requests with Smithay's focused-pointer and enter-serial checks.

// Rust guideline compliant 2026-10-07
use super::state::State;
use crate::gui::CursorStyle;
use smithay::{
    delegate_cursor_shape,
    input::pointer::CursorIcon,
    reexports::wayland_server::DisplayHandle,
    wayland::{cursor_shape::CursorShapeManagerState, tablet_manager::TabletSeatHandler},
};

pub(super) fn register(display: &DisplayHandle) {
    CursorShapeManagerState::new::<State>(display);
}

// No tablet global is advertised. Smithay's shared shape delegate also supports
// tablet devices, so its trait bound is satisfied without exposing another seat.
impl TabletSeatHandler for State {}
delegate_cursor_shape!(State);

pub(super) fn style(icon: CursorIcon) -> CursorStyle {
    match icon {
        CursorIcon::Default => CursorStyle::Arrow,
        CursorIcon::ContextMenu => CursorStyle::ContextualMenu,
        CursorIcon::Help => CursorStyle::Help,
        CursorIcon::Pointer => CursorStyle::PointingHand,
        CursorIcon::Progress => CursorStyle::Progress,
        CursorIcon::Wait => CursorStyle::Wait,
        CursorIcon::Cell => CursorStyle::Cell,
        CursorIcon::Crosshair => CursorStyle::Crosshair,
        CursorIcon::Text => CursorStyle::IBeam,
        CursorIcon::VerticalText => CursorStyle::IBeamCursorForVerticalLayout,
        CursorIcon::Alias => CursorStyle::DragLink,
        CursorIcon::Copy => CursorStyle::DragCopy,
        CursorIcon::Move => CursorStyle::Move,
        CursorIcon::NoDrop => CursorStyle::NoDrop,
        CursorIcon::NotAllowed => CursorStyle::OperationNotAllowed,
        CursorIcon::Grab => CursorStyle::OpenHand,
        CursorIcon::Grabbing => CursorStyle::ClosedHand,
        CursorIcon::EResize => CursorStyle::ResizeRight,
        CursorIcon::NResize => CursorStyle::ResizeUp,
        CursorIcon::NeResize => CursorStyle::ResizeUpRight,
        CursorIcon::NwResize => CursorStyle::ResizeUpLeft,
        CursorIcon::SResize => CursorStyle::ResizeDown,
        CursorIcon::SeResize => CursorStyle::ResizeDownRight,
        CursorIcon::SwResize => CursorStyle::ResizeDownLeft,
        CursorIcon::WResize => CursorStyle::ResizeLeft,
        CursorIcon::EwResize => CursorStyle::ResizeLeftRight,
        CursorIcon::NsResize => CursorStyle::ResizeUpDown,
        CursorIcon::NeswResize => CursorStyle::ResizeUpRightDownLeft,
        CursorIcon::NwseResize => CursorStyle::ResizeUpLeftDownRight,
        CursorIcon::ColResize => CursorStyle::ResizeColumn,
        CursorIcon::RowResize => CursorStyle::ResizeRow,
        CursorIcon::AllScroll => CursorStyle::AllScroll,
        CursorIcon::ZoomIn => CursorStyle::ZoomIn,
        CursorIcon::ZoomOut => CursorStyle::ZoomOut,
        CursorIcon::DndAsk => CursorStyle::DragAsk,
        CursorIcon::AllResize => CursorStyle::ResizeAll,
        _ => CursorStyle::Arrow,
    }
}

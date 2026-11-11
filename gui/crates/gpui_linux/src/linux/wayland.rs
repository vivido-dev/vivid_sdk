mod client;
mod clipboard;
mod cursor;
mod display;
mod popup;
mod serial;
mod window;

/// Contains Types for configuring layer_shell surfaces.
pub mod layer_shell;

pub(crate) use client::*;

use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::Shape;

use gpui::CursorStyle;

pub(super) fn to_shape(style: CursorStyle, version: u32) -> Shape {
    match style {
        CursorStyle::Hidden | CursorStyle::Arrow => Shape::Default,
        CursorStyle::IBeam => Shape::Text,
        CursorStyle::Crosshair => Shape::Crosshair,
        CursorStyle::ClosedHand => Shape::Grabbing,
        CursorStyle::OpenHand => Shape::Grab,
        CursorStyle::PointingHand => Shape::Pointer,
        CursorStyle::ResizeLeft => Shape::WResize,
        CursorStyle::ResizeRight => Shape::EResize,
        CursorStyle::ResizeLeftRight => Shape::EwResize,
        CursorStyle::ResizeUp => Shape::NResize,
        CursorStyle::ResizeDown => Shape::SResize,
        CursorStyle::ResizeUpDown => Shape::NsResize,
        CursorStyle::ResizeUpLeftDownRight => Shape::NwseResize,
        CursorStyle::ResizeUpRightDownLeft => Shape::NeswResize,
        CursorStyle::ResizeColumn => Shape::ColResize,
        CursorStyle::ResizeRow => Shape::RowResize,
        CursorStyle::IBeamCursorForVerticalLayout => Shape::VerticalText,
        CursorStyle::OperationNotAllowed => Shape::NotAllowed,
        CursorStyle::DragLink => Shape::Alias,
        CursorStyle::DragCopy => Shape::Copy,
        CursorStyle::ContextualMenu => Shape::ContextMenu,
        CursorStyle::Help => Shape::Help,
        CursorStyle::Progress => Shape::Progress,
        CursorStyle::Wait => Shape::Wait,
        CursorStyle::Cell => Shape::Cell,
        CursorStyle::Move => Shape::Move,
        CursorStyle::NoDrop => Shape::NoDrop,
        CursorStyle::ResizeUpRight => Shape::NeResize,
        CursorStyle::ResizeUpLeft => Shape::NwResize,
        CursorStyle::ResizeDownRight => Shape::SeResize,
        CursorStyle::ResizeDownLeft => Shape::SwResize,
        CursorStyle::AllScroll => Shape::AllScroll,
        CursorStyle::ZoomIn => Shape::ZoomIn,
        CursorStyle::ZoomOut => Shape::ZoomOut,
        CursorStyle::DragAsk if version >= 2 => Shape::DndAsk,
        CursorStyle::DragAsk => Shape::ContextMenu,
        CursorStyle::ResizeAll if version >= 2 => Shape::AllResize,
        CursorStyle::ResizeAll => Shape::Move,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_two_shapes_use_compatible_values_on_older_compositors() {
        assert_eq!(to_shape(CursorStyle::DragAsk, 1), Shape::ContextMenu);
        assert_eq!(to_shape(CursorStyle::ResizeAll, 1), Shape::Move);
        assert_eq!(to_shape(CursorStyle::DragAsk, 2), Shape::DndAsk);
        assert_eq!(to_shape(CursorStyle::ResizeAll, 2), Shape::AllResize);
        assert_eq!(
            to_shape(CursorStyle::ResizeUpLeftDownRight, 1),
            Shape::NwseResize
        );
        assert_eq!(
            to_shape(CursorStyle::ResizeUpRightDownLeft, 1),
            Shape::NeswResize
        );
    }
}

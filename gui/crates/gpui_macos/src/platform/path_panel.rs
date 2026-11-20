//! Caller-scoped lifetime of native macOS file panels.

// Rust guideline compliant 2026-10-07
use block2::RcBlock;
use futures::{
    channel::oneshot,
    future::{Either, select},
};
use objc2_app_kit::{NSModalResponse, NSSavePanel};
use std::cell::Cell;

pub(super) async fn present_path_panel<T>(
    panel: &NSSavePanel,
    caller: &mut oneshot::Sender<T>,
) -> Option<NSModalResponse> {
    panel_response(
        caller,
        |response| {
            let response = Cell::new(Some(response));
            let handler = RcBlock::new(move |value| {
                if let Some(response) = response.take() {
                    let _ = response.send(value);
                }
            });
            panel.beginWithCompletionHandler(&handler);
        },
        || {
            // SAFETY: A nil sender is valid for this programmatic AppKit action.
            unsafe { panel.cancel(None) };
        },
    )
    .await
}

async fn panel_response<T>(
    caller: &mut oneshot::Sender<T>,
    begin: impl FnOnce(oneshot::Sender<NSModalResponse>),
    cancel: impl FnOnce(),
) -> Option<NSModalResponse> {
    if caller.is_canceled() {
        return None;
    }
    let (response, received) = oneshot::channel();
    begin(response);
    match select(received, caller.cancellation()).await {
        Either::Left((result, _)) => result.ok(),
        Either::Right(_) => {
            cancel();
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::FutureExt as _;
    use objc2_app_kit::NSModalResponseOK;
    use std::pin::pin;

    #[test]
    fn dropping_one_caller_dismisses_only_its_panel() {
        let (mut first, first_rx) = oneshot::channel::<()>();
        let (mut peer, _peer_rx) = oneshot::channel::<()>();
        let retired = Cell::new(None);
        let live = Cell::new(None);
        let cancelled = Cell::new(0);
        let mut original = pin!(panel_response(
            &mut first,
            |tx| retired.set(Some(tx)),
            || cancelled.set(cancelled.get() + 1)
        ));
        let mut other = pin!(panel_response(
            &mut peer,
            |tx| live.set(Some(tx)),
            || panic!("peer panel cancelled")
        ));
        assert!(original.as_mut().now_or_never().is_none());
        assert!(other.as_mut().now_or_never().is_none());
        drop(first_rx);
        assert_eq!(original.as_mut().now_or_never(), Some(None));
        assert_eq!(cancelled.get(), 1);
        assert!(retired.take().unwrap().send(NSModalResponseOK).is_err());
        assert!(other.as_mut().now_or_never().is_none());
        live.take().unwrap().send(NSModalResponseOK).unwrap();
        assert_eq!(other.as_mut().now_or_never(), Some(Some(NSModalResponseOK)));
    }

    #[test]
    fn caller_lost_before_presentation_never_opens_a_panel() {
        let (mut caller, receiver) = oneshot::channel::<()>();
        drop(receiver);
        assert_eq!(
            panel_response(&mut caller, |_| panic!("opened"), || panic!("cancelled"))
                .now_or_never(),
            Some(None)
        );
    }
}

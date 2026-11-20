//! Render production GPUI frames and verify their authenticated Vivid transport.
//!
//! This verifies frame production, not remote input or complete app hosting.

// Rust guideline compliant 2026-10-07
use std::{
    cell::RefCell,
    io,
    rc::Rc,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
use vivid_sdk::{
    PaneSession, ProducerAuthentication, ProducerConfig, Session,
    gui::{
        self, AppContext as _, Bounds, Context, Render, Window, WindowBounds, WindowOptions, div,
        prelude::*, px, size,
    },
    presenter::{
        CaptureContent, MediaConfig, PresenterListener as _, SocketListener, VirtualVivid,
    },
};

struct Card(bool);

impl Render for Card {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(gui::rgb(if self.0 { 0x245844 } else { 0x243858 }))
            .text_color(gui::rgb(0xffffff))
            .p_4()
            .child(if self.0 {
                "Production SDK frame: updated"
            } else {
                "Production SDK frame: initial"
            })
    }
}

struct Frame {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

fn verify_frames(receiver: mpsc::Receiver<Frame>) -> io::Result<()> {
    let listener = SocketListener::bind("tcp:127.0.0.1:0")?;
    let endpoint = listener.endpoint();
    let presenter = VirtualVivid::start(listener, MediaConfig::default())?;
    presenter.update_metrics(1, 80, 24, (8, 16));
    // This capability stays in memory; it never travels through argv or a PTY.
    let capability = presenter.issue_pane_capability(1)?;
    let session = Session::connect(ProducerConfig {
        endpoint_control: Some(endpoint),
        authentication: ProducerAuthentication::root_hex(&capability)?,
        ..ProducerConfig::default()
    })?;
    let mut pane = PaneSession::from_session(session)?;
    let result = (|| {
        for _ in 0..2 {
            let frame = receiver
                .recv_timeout(Duration::from_secs(15))
                .map_err(|_| io::Error::other("GUI frame was not produced"))?;
            pane.show_rgba(frame.width, frame.height, &frame.pixels)?;
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let capture = presenter.capture_pane(1, 0);
                if capture.layers.len() == 1
                    && capture.skipped.is_empty()
                    && matches!(&capture.layers[0].content, CaptureContent::Raster(raster)
                        if raster.width == frame.width && raster.height == frame.height
                            && raster.pixels.as_ref() == frame.pixels.as_slice())
                {
                    break;
                }
                if Instant::now() >= deadline {
                    return Err(io::Error::other("transport changed or lost the GUI pixels"));
                }
                thread::sleep(Duration::from_millis(5));
            }
        }
        Ok(())
    })();
    let close = pane.close();
    result.and(close)
}

fn main() -> io::Result<()> {
    let output = std::env::args_os().nth(1).map(std::path::PathBuf::from);
    // This finite check queues at most two CPU frames; network I/O stays off the GUI thread.
    let (sender, receiver) = mpsc::sync_channel(2);
    let worker = thread::spawn(move || verify_frames(receiver));
    let gui_result = Rc::new(RefCell::new(Err(io::Error::other(
        "GUI runtime stopped before verification",
    ))));
    let result_slot = gui_result.clone();
    let platform = vivid_sdk::gui_platform::current_platform(!cfg!(target_os = "windows"));
    gui::Application::with_platform(platform).run(move |cx| {
        let opened = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    Default::default(),
                    size(px(320.), px(160.)),
                ))),
                focus: false,
                show: false,
                ..Default::default()
            },
            |_, cx| cx.new(|_| Card(false)),
        );
        let handle = match opened {
            Ok(handle) => handle,
            Err(error) => {
                eprintln!("Cannot open the offscreen window: {error}");
                std::process::exit(1);
            }
        };
        let handle: gui::AnyWindowHandle = handle.into();
        cx.spawn(async move |cx| {
            let rendering = async {
                let mut previous = None;
                for updated in [false, true] {
                    if updated {
                        handle.update(cx, |_, window, cx| {
                            window.resize(size(px(400.), px(192.)));
                            // Headless windows have no native resize callbacks.
                            window.bounds_changed(cx);
                        }).map_err(|error| io::Error::other(error.to_string()))?;
                    }
                    cx.background_executor()
                        .timer(Duration::from_millis(20))
                        .await;
                    let frame = handle
                        .update(cx, |view, window, cx| {
                            view.downcast::<Card>().expect("root is a card").update(cx, |card, cx| {
                                card.0 = updated;
                                cx.notify();
                            });
                            let arena = window.draw(cx);
                            let image = window.render_to_image();
                            arena.clear(cx);
                            image
                        })
                        .map_err(|error| io::Error::other(error.to_string()))?
                        .map_err(|error| io::Error::other(error.to_string()))?;
                    if previous
                        .as_ref()
                        .is_some_and(|old: &Vec<u8>| old == frame.as_raw())
                    {
                        return Err(io::Error::other(
                            "GUI update did not change rendered pixels",
                        ));
                    }
                    previous = Some(frame.as_raw().clone());
                    if frame.pixels().filter(|pixel| pixel.0[..3].iter().all(|channel| *channel > 235)).count() < 10 {
                        return Err(io::Error::other("GUI text did not produce foreground pixels"));
                    }
                    if updated && let Some(output) = &output {
                        frame.save(output).map_err(|_| io::Error::other("cannot save the GUI frame"))?;
                    }
                    sender
                        .try_send(Frame {
                            width: frame.width(),
                            height: frame.height(),
                            pixels: frame.into_raw(),
                        })
                        .map_err(|_| {
                            io::Error::other("frame transport is unavailable or backpressured")
                        })?;
                }
                Ok(())
            }
            .await;
            drop(sender);
            let deadline = Instant::now() + Duration::from_secs(20);
            while !worker.is_finished() && Instant::now() < deadline {
                cx.background_executor().timer(Duration::from_millis(5)).await;
            }
            let transport = if worker.is_finished() {
                worker.join().map_err(|_| io::Error::other("frame verifier stopped unexpectedly")).and_then(|result| result)
            } else {
                Err(io::Error::other("frame verifier did not finish"))
            };
            let result = rendering.and(transport);
            if let Err(error) = &result {
                eprintln!("Offscreen round trip failed: {error}");
                // Native macOS termination does not return from Application::run.
                std::process::exit(1);
            }
            println!("Verified two distinct production GPUI frames through authenticated Vivid transport.");
            *result_slot.borrow_mut() = result;
            cx.update(|cx| cx.quit());
        })
        .detach();
    });
    std::mem::replace(&mut *gui_result.borrow_mut(), Ok(()))
}

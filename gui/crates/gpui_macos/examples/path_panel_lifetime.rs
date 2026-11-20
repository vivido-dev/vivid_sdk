//! Exercises independent cancellation and configuration of real AppKit file panels.

// Rust guideline compliant 2026-10-07
#[cfg(target_os = "macos")]
mod native {
    use anyhow::{Context, Result, ensure};
    use futures::{
        channel::oneshot,
        future::{Either, select},
    };
    use gpui::{BackgroundExecutor, PathPromptOptions, Platform};
    use gpui_macos::MacPlatform;
    use objc2::{MainThreadMarker, rc::Retained};
    use objc2_app_kit::{NSApplication, NSOpenPanel, NSSavePanel, NSWindow};
    use std::{
        path::Path,
        rc::Rc,
        time::{Duration, Instant},
    };

    const TIMEOUT: Duration = Duration::from_secs(10);

    async fn wait_for<T>(
        timer: &BackgroundExecutor,
        stage: &str,
        mut observe: impl FnMut() -> Option<T>,
    ) -> Result<T> {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(value) = observe() {
                return Ok(value);
            }
            ensure!(Instant::now() < deadline, "{stage}");
            timer.timer(Duration::from_millis(25)).await;
        }
    }

    async fn cancel_one<T, U>(
        timer: &BackgroundExecutor,
        retired: oneshot::Receiver<anyhow::Result<Option<T>>>,
        old_window: Retained<NSWindow>,
        peer: oneshot::Receiver<anyhow::Result<Option<U>>>,
        peer_window: Retained<NSWindow>,
    ) -> Result<()> {
        drop(retired);
        wait_for(timer, "retired panel remained visible", || {
            (!old_window.isVisible()).then_some(())
        })
        .await?;
        ensure!(
            peer_window.isVisible(),
            "retiring one panel dismissed its peer"
        );
        let peer_panel = peer_window
            .downcast_ref::<NSSavePanel>()
            .context("peer panel")?;
        // SAFETY: Cancel this fixture’s panel with no action sender.
        unsafe { peer_panel.cancel(None) };
        let Either::Left((answer, _)) = select(peer, timer.timer(TIMEOUT)).await else {
            anyhow::bail!("peer panel failed to answer cancellation");
        };
        ensure!(answer??.is_none(), "cancel returned a path");
        Ok(())
    }

    async fn check(host: &MacPlatform) -> Result<()> {
        let app =
            NSApplication::sharedApplication(MainThreadMarker::new().expect("AppKit main thread"));
        let timer = host.background_executor();
        for retire_open in [true, false] {
            let open = host.prompt_for_paths(PathPromptOptions {
                files: true,
                directories: false,
                multiple: false,
                prompt: None,
            });
            let open_window = wait_for(&timer, "open panel did not appear", || {
                app.windows()
                    .iter()
                    .find(|w| w.downcast_ref::<NSOpenPanel>().is_some() && w.isVisible())
            })
            .await?;
            let save = host.prompt_for_new_path(Path::new("/tmp"), Some("vivid-panel-check"));
            let save_window = wait_for(&timer, "save panel did not appear", || {
                app.windows().iter().find(|w| {
                    w.downcast_ref::<NSSavePanel>().is_some()
                        && w.downcast_ref::<NSOpenPanel>().is_none()
                        && w.isVisible()
                })
            })
            .await?;
            if retire_open {
                cancel_one(&timer, open, open_window, save, save_window).await?;
            } else {
                cancel_one(&timer, save, save_window, open, open_window).await?;
            }
        }
        let directory = std::env::temp_dir().canonicalize()?;
        let filename = format!("vivid panel {}.txt", std::process::id());
        let expected = directory.join(&filename);
        ensure!(
            !expected.try_exists()?,
            "fixture destination already exists"
        );
        let save = host.prompt_for_new_path(&directory, Some(&filename));
        let window = wait_for(&timer, "configured save panel did not appear", || {
            app.windows().iter().find(|window| {
                window.downcast_ref::<NSSavePanel>().is_some_and(|panel| {
                    window.isVisible() && panel.nameFieldStringValue().to_string() == filename
                })
            })
        })
        .await?;
        let panel = window
            .downcast_ref::<NSSavePanel>()
            .context("configured save panel")?;
        let shown = panel
            .directoryURL()
            .and_then(|url| url.to_file_path())
            .context("missing initial save directory")?;
        ensure!(
            shown.canonicalize()? == directory,
            "save panel showed a different directory"
        );
        // SAFETY: Cancel the configured fixture panel with no action sender.
        unsafe { panel.cancel(None) };
        let Either::Left((answer, _)) = select(save, timer.timer(TIMEOUT)).await else {
            anyhow::bail!("configured save panel failed to close");
        };
        ensure!(
            answer??.is_none() && !expected.try_exists()?,
            "cancel returned or created a destination"
        );
        Ok(())
    }

    pub(super) fn run() {
        let platform = Rc::new(MacPlatform::new(false));
        let host = platform.clone();
        platform.run(Box::new(move || {
            host.foreground_executor().spawn(async move {
                if let Err(error) = check(&host).await {
                    eprintln!("{error:#}");
                    std::process::exit(1);
                }
                println!("Verified independent AppKit cancellation, suggested filename and initial save directory");
                host.quit();
            }).detach();
        }));
    }
}

fn main() {
    #[cfg(target_os = "macos")]
    native::run();
}

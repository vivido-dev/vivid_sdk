//! One GUI root running standalone or in an independently hosted process.

// Rust guideline compliant 2026-10-07
use vivid_sdk::gui::{
    App, AppContext as _, Context, Entity, FocusHandle, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, StatefulInteractiveElement as _, Styled as _, Window,
    WindowOptions, div, rgb,
};

struct Counter {
    count: u64,
    focus: FocusHandle,
}
impl Render for Counter {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("counter")
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgb(0x203044))
            .text_color(rgb(0xffffff))
            .track_focus(&self.focus)
            .on_key_down(
                cx.listener(|_, event: &vivid_sdk::gui::KeyDownEvent, window, cx| {
                    if event.keystroke.modifiers.control && event.keystroke.key == "q" {
                        window.prevent_default();
                        cx.stop_propagation();
                        cx.quit();
                    }
                }),
            )
            .on_click(cx.listener(|_, _, window, cx| {
                let view = cx.weak_entity();
                // Exercise the same frame-driven update path in both hosts.
                window.on_next_frame(move |_, cx| {
                    let _ = view.update(cx, |counter, cx| {
                        counter.count = counter.count.saturating_add(1);
                        cx.notify();
                    });
                });
            }))
            .child(
                div()
                    .w_full()
                    .px_2()
                    .text_center()
                    .whitespace_normal()
                    .child(format!(
                        "Independent counter: {} — click, Enter, or Space",
                        self.count
                    )),
            )
    }
}

fn build(window: &mut Window, cx: &mut App) -> Entity<Counter> {
    build_restored(None, window, cx).expect("default counter state")
}

fn build_restored(
    state: Option<&vivid_sdk::gui_host::AppState>,
    window: &mut Window,
    cx: &mut App,
) -> std::io::Result<Entity<Counter>> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Saved {
        count: u64,
    }
    let count = state
        .map(|state| state.decode::<Saved>())
        .transpose()?
        .map_or(0, |saved| saved.count);
    let view = cx.new(|cx| Counter {
        count,
        focus: cx.focus_handle(),
    });
    view.read(cx).focus.clone().focus(window, cx);
    Ok(view)
}

fn main() -> std::io::Result<()> {
    if std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == "--remote")
    {
        return vivid_sdk::gui_remote::run_persistent(
            vivid_sdk::gui_remote::config_from_env()?,
            build_restored,
            |counter, _| {
                vivid_sdk::gui_host::AppState::new(serde_json::json!({"count":counter.count}))
                    .map(Some)
            },
        );
    }
    vivid_sdk::gui_platform::application().run(|cx| {
        if cx.open_window(WindowOptions::default(), build).is_err() {
            eprintln!("Unable to open counter window");
            std::process::exit(1);
        }
        cx.activate(true);
    });
    Ok(())
}

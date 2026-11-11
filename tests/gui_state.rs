//! Saved state creates new owners and cannot bypass validation or host capacity.
#![cfg(feature = "gui-testing")]

// Rust guideline compliant 2026-10-07
use std::{cell::Cell, io, rc::Rc};
use vivid_sdk::gui::{
    self as gpui, AnyView, App, AppContext, Context, Entity, IntoElement, Render, TestAppContext,
    Window, div,
};
use vivid_sdk::gui_host::{
    AppConfig, AppDescriptor, AppFactory, AppHost, AppLaunch, AppState, HostedApp, Lifecycle,
    MAX_CONFIG_BYTES,
};

#[derive(Debug)]
struct View(u64);
impl Render for View {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}
#[derive(Debug)]
struct Instance(Entity<View>);
impl HostedApp for Instance {
    fn view(&self) -> AnyView {
        self.0.clone().into()
    }
    fn lifecycle(&mut self, _: Lifecycle, _: &mut Window, _: &mut App) {}
    fn save_state(&self, cx: &App) -> io::Result<Option<AppState>> {
        AppState::new(serde_json::json!({ "count": self.0.read(cx).0 })).map(Some)
    }
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    count: u64,
}
#[derive(Debug)]
struct Factory(Rc<Cell<usize>>);
impl AppFactory for Factory {
    fn descriptor(&self) -> AppDescriptor {
        AppDescriptor::new("saved", "Saved")
    }
    fn create(
        &self,
        _: &AppConfig,
        _: &mut Window,
        cx: &mut App,
    ) -> io::Result<Box<dyn HostedApp>> {
        self.0.set(self.0.get() + 1);
        Ok(Box::new(Instance(cx.new(|_| View(7)))))
    }
    fn validate_state(&self, _: &AppConfig, state: &AppState) -> io::Result<()> {
        state.decode::<State>().map(|_| ())
    }
    fn create_restored(
        &self,
        launch: AppLaunch<'_>,
        window: &mut Window,
        cx: &mut App,
    ) -> io::Result<Box<dyn HostedApp>> {
        let app = self.create(launch.options, window, cx)?;
        if let Some(state) = launch.state {
            let count = state.decode::<State>()?.count;
            app.view()
                .downcast::<View>()
                .unwrap()
                .update(cx, |view, _| view.0 = count);
        }
        Ok(app)
    }
}
#[gpui::test]
fn snapshots_of_hidden_apps_restore_as_fresh_owners_and_invalid_state_is_atomic(
    cx: &mut TestAppContext,
) {
    assert!(AppState::new(serde_json::json!([])).is_err());
    assert!(AppState::new(serde_json::json!({ "data": "x".repeat(MAX_CONFIG_BYTES) })).is_err());
    let invalid = AppState::new(serde_json::json!({ "count": "private-value" })).unwrap();
    assert!(!format!("{invalid:?}").contains("private-value"));
    assert!(
        !invalid
            .decode::<State>()
            .err()
            .unwrap()
            .to_string()
            .contains("private-value")
    );
    cx.add_window(|window, cx| {
        let created = Rc::new(Cell::new(0));
        let factory = Factory(created.clone());
        let options = AppConfig::default();
        let mut first = AppHost::new(1).unwrap();
        let mut second = AppHost::new(1).unwrap();
        let id = first.mount(&factory, &options, window, cx).unwrap();
        let view = first.view(id).unwrap().downcast::<View>().unwrap();
        view.update(cx, |view, _| view.0 = 42);
        first.set_visible(id, false, window, cx);
        let state = first.save_state(id, cx).unwrap().unwrap();
        assert!(second.save_state(id, cx).is_err());
        let invalid_launch = AppLaunch {
            options: &options,
            state: Some(&invalid),
        };
        assert!(
            second
                .mount_launch(&factory, invalid_launch, window, cx)
                .is_err()
        );
        assert_eq!(created.get(), 1);
        let launch = AppLaunch {
            options: &options,
            state: Some(&state),
        };
        let restored = second.mount_launch(&factory, launch, window, cx).unwrap();
        assert_ne!(restored, id);
        assert_eq!(second.is_visible(restored), Some(true));
        assert_eq!(
            second
                .view(restored)
                .unwrap()
                .downcast::<View>()
                .unwrap()
                .read(cx)
                .0,
            42
        );
        assert!(second.mount_launch(&factory, launch, window, cx).is_err());
        assert_eq!(created.get(), 2);
        assert!(
            first
                .replace_launch(id, &factory, invalid_launch, window, cx)
                .is_err()
        );
        assert_eq!(first.is_visible(id), Some(false));
        assert_eq!(view.read(cx).0, 42);
        first.shutdown(window, cx);
        second.shutdown(window, cx);
        View(0)
    });
}

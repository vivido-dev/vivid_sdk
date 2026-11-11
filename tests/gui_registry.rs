//! Public registration, launch configuration, and owner-scoped hosting contracts.
#![cfg(feature = "gui")]

// Rust guideline compliant 2026-10-07
use serde::Deserialize;
use serde_json::json;
use std::io;
use vivid_sdk::gui_host::{
    AppConfig, AppDescriptor, AppRegistry, MAX_CONFIG_BYTES, validate_app_id,
};

#[test]
fn launch_configuration_bounds_work_and_redacts_decode_failures() {
    assert!(AppConfig::new(json!([])).is_err());
    assert!(AppConfig::new(json!({ "text": "x".repeat(MAX_CONFIG_BYTES) })).is_err());
    // Escaped JSON bytes count too, even when the underlying string is smaller.
    assert!(AppConfig::new(json!({ "text": "\u{1}".repeat(MAX_CONFIG_BYTES / 4) })).is_err());
    assert!(AppConfig::new(json!({ "items": vec![0; 1024] })).is_err());
    let mut deep = json!(0);
    for _ in 0..18 {
        deep = json!({ "child": deep });
    }
    assert!(AppConfig::new(deep).is_err());
    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Options {
        count: u64,
    }
    let config = AppConfig::new(json!({ "count": 7 })).unwrap();
    assert_eq!(config.decode::<Options>().unwrap().count, 7);
    let invalid = AppConfig::new(json!({ "count": "private-configuration" })).unwrap();
    assert!(!format!("{invalid:?}").contains("private-configuration"));
    let error = invalid.decode::<Options>().unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert!(!error.to_string().contains("private-configuration"));
    assert!(serde_json::from_str::<AppConfig>("[]").is_err());
}

#[test]
fn registration_metadata_capacity_and_availability_are_atomic() {
    for id in ["", "Uppercase", "with space", "with\nnewline", "../path"] {
        assert!(validate_app_id(id).is_err());
    }
    assert!(validate_app_id(&"a".repeat(129)).is_err());
    validate_app_id("dev.example.notes-v2").unwrap();
    let mut registry = AppRegistry::new(1);
    assert!(
        registry
            .register_unavailable(AppDescriptor::new("bad", "bad\nname"), "disabled")
            .is_err()
    );
    registry
        .register_unavailable(
            AppDescriptor::new(String::from("optional"), String::from("Optional")),
            "enable its feature",
        )
        .unwrap();
    assert_eq!(registry.descriptors().len(), 1);
    assert_eq!(registry.available().count(), 0);
    assert_eq!(
        registry
            .register_unavailable(AppDescriptor::new("optional", "Replacement"), "different")
            .unwrap_err()
            .kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(registry.descriptor("optional").unwrap().name, "Optional");
    assert_eq!(
        registry
            .register_unavailable(AppDescriptor::new("other", "Other"), "disabled")
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    assert_eq!(
        registry
            .validate_launch("optional", &AppConfig::default())
            .unwrap_err()
            .kind(),
        io::ErrorKind::Unsupported
    );
    assert_eq!(
        registry
            .validate_launch("missing", &AppConfig::default())
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound
    );
}

#[cfg(feature = "gui-testing")]
mod hosting {
    use super::*;
    use gpui::{
        AnyView, App, AppContext, Context, Entity, IntoElement, Render, TestAppContext, Window, div,
    };
    use std::{cell::Cell, rc::Rc};
    use vivid_sdk::gui as gpui;
    use vivid_sdk::gui_host::{AppFactory, AppHost, HostedApp, Lifecycle};

    #[derive(Debug)]
    struct Factory {
        created: Rc<Cell<usize>>,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Options {
        value: u64,
    }
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
    }
    impl AppFactory for Factory {
        fn descriptor(&self) -> AppDescriptor {
            AppDescriptor::new(String::from("custom.alpha"), "Custom Alpha")
        }
        fn validate(&self, config: &AppConfig) -> io::Result<()> {
            config.decode::<Options>().map(|_| ())
        }
        fn create(
            &self,
            config: &AppConfig,
            _: &mut Window,
            cx: &mut App,
        ) -> io::Result<Box<dyn HostedApp>> {
            let options: Options = config.decode()?;
            self.created.set(self.created.get() + 1);
            Ok(Box::new(Instance(cx.new(|_| View(options.value)))))
        }
    }

    #[gpui::test]
    fn configuration_and_capacity_preflight_preserve_factories_and_other_owners(
        cx: &mut TestAppContext,
    ) {
        let created = Rc::new(Cell::new(0));
        let replacement = Rc::new(Cell::new(0));
        let mut registry = AppRegistry::new(2);
        registry
            .register(Box::new(Factory {
                created: created.clone(),
            }))
            .unwrap();
        assert!(
            registry
                .register(Box::new(Factory {
                    created: replacement.clone()
                }))
                .is_err()
        );
        let config = AppConfig::new(json!({ "value": 7 })).unwrap();
        registry.validate_launch("custom.alpha", &config).unwrap();
        assert_eq!(
            created.get(),
            0,
            "registration/validation must not create runtimes"
        );
        cx.add_window(|window, cx| {
            let mut first = AppHost::new(1).unwrap();
            let mut second = AppHost::new(1).unwrap();
            assert!(
                registry
                    .mount(
                        &mut first,
                        "custom.alpha",
                        &AppConfig::default(),
                        window,
                        cx
                    )
                    .is_err()
            );
            assert!(
                registry
                    .mount(&mut first, "unknown", &config, window, cx)
                    .is_err()
            );
            assert_eq!(created.get(), 0);
            let left = registry
                .mount(&mut first, "custom.alpha", &config, window, cx)
                .unwrap();
            assert!(
                registry
                    .mount(&mut first, "custom.alpha", &config, window, cx)
                    .is_err()
            );
            assert_eq!(created.get(), 1);
            let right = registry
                .mount(
                    &mut second,
                    "custom.alpha",
                    &AppConfig::new(json!({ "value": 9 })).unwrap(),
                    window,
                    cx,
                )
                .unwrap();
            assert!(!second.close(left, window, cx));
            assert!(first.close(left, window, cx));
            let view = second.view(right).unwrap().downcast::<View>().unwrap();
            assert_eq!(view.read(cx).0, 9);
            let next = registry
                .mount(&mut first, "custom.alpha", &config, window, cx)
                .unwrap();
            assert_ne!(left, next);
            assert_eq!(created.get(), 3);
            assert_eq!(replacement.get(), 0);
            first.shutdown(window, cx);
            second.shutdown(window, cx);
            View(0)
        });
    }
}

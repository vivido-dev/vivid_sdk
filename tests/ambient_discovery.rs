//! A producer that names its control endpoint must not take lanes from the ambient discovery
//! environment, which describes whichever presenter the process happens to run under.
//!
//! This binary holds one test because it mutates the process environment.
#![cfg(feature = "presenter")]

use std::time::Duration;
use vivid_sdk::presenter::*;
use vivid_sdk::*;

#[test]
fn an_explicit_control_endpoint_ignores_ambient_lane_endpoints() {
    // SAFETY: this is the only test in the binary and it runs before any thread of its own exists.
    unsafe {
        for variable in [
            "VIVID_ENDPOINT_INTERACTIVE",
            "VIVID_ENDPOINT_REALTIME",
            "VIVID_ENDPOINT_BULK",
        ] {
            std::env::set_var(variable, "tcp:127.0.0.1:1");
        }
    }
    let listener = SocketListener::bind("tcp:127.0.0.1:0").unwrap();
    let endpoint = listener.endpoint();
    let presenter = VirtualVivid::start_configured(
        listener,
        PresenterConfig::terminal(MediaConfig::default()),
        None,
    )
    .unwrap();
    presenter.update_metrics(1, 80, 24, (8, 16));
    let secret = presenter.issue_pane_capability(1).unwrap();
    let session = Session::connect(ProducerConfig {
        endpoint_control: Some(endpoint),
        authentication: ProducerAuthentication::root_hex(&secret).unwrap(),
        ..ProducerConfig::default()
    })
    .unwrap();
    let mut pane = PaneSession::from_session(session).unwrap();
    pane.show_rgba(1, 1, &[255; 4]).unwrap();
    assert!(presenter.wait_for_retained_media(1, Duration::from_secs(2)));
    presenter.revoke_pane(1);
}

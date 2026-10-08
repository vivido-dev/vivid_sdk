//! `Debug` output of secret-bearing producer types must never carry capability material.

use std::time::Duration;

use vivid_protocol::auth::Secret32;
use vivid_sdk::{EstablishmentAttempt, ProducerAuthentication, ProducerConfig};

// Distinctive byte values, so a leak shows up in either the hex or the decimal rendering. The
// checks look for a repeated byte, as a leaked array renders, so an unrelated number such as the
// attempt deadline cannot match by chance.
const SECRET: u8 = 0xa5;
const ATTEMPT: u8 = 0xab;
const PROOF: u8 = 0xcd;

fn assert_redacted(rendered: &str) {
    for byte in [SECRET, ATTEMPT, PROOF] {
        assert!(
            !rendered.contains(&format!("{byte}, {byte}")),
            "{rendered} leaks byte {byte}"
        );
        assert!(
            !rendered.contains(&format!("{byte:02x}{byte:02x}")),
            "{rendered} leaks byte {byte:#x}"
        );
    }
}

fn variants() -> [ProducerAuthentication; 4] {
    [
        ProducerAuthentication::RootFromEnvironment,
        ProducerAuthentication::Root {
            root_secret: Secret32::new([SECRET; 32]),
        },
        ProducerAuthentication::LeaseActivation {
            context_id: 7,
            lease_id: 8,
            activation_secret: Secret32::new([SECRET; 32]),
            attempt_id: [ATTEMPT; 16],
            proof_of_possession: Some(vec![PROOF; 8]),
        },
        ProducerAuthentication::Resume {
            context_id: 7,
            lease_id: 8,
            session_id: 9,
            resume_generation: 10,
            attempt_id: [ATTEMPT; 16],
            prior_resume_key: Secret32::new([SECRET; 32]),
        },
    ]
}

#[test]
fn authentication_debug_shows_identity_without_secrets() {
    let rendered: Vec<String> = variants().iter().map(|auth| format!("{auth:?}")).collect();
    for text in &rendered {
        assert_redacted(text);
    }
    assert_eq!(rendered[0], "RootFromEnvironment");
    assert!(rendered[1].starts_with("Root"));
    assert!(rendered[2].contains("context_id: 7") && rendered[2].contains("lease_id: 8"));
    assert!(rendered[3].contains("session_id: 9") && rendered[3].contains("resume_generation: 10"));
}

#[test]
fn config_and_attempt_debug_redact_authentication() {
    for authentication in variants() {
        let config = ProducerConfig {
            authentication,
            ..ProducerConfig::default()
        };
        assert_redacted(&format!("{config:?}"));
    }

    let [_, _, _, resume] = variants();
    let attempt = EstablishmentAttempt::new(
        ProducerConfig {
            authentication: resume,
            ..ProducerConfig::default()
        },
        Duration::from_secs(5),
    )
    .unwrap();
    let rendered = format!("{attempt:?}");
    assert_redacted(&rendered);
    assert!(rendered.starts_with("EstablishmentAttempt"));
}

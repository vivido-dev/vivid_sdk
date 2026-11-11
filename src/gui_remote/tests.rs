//! Launch authority parsing without mutating the process environment.

// Rust guideline compliant 2026-10-07
use super::*;
use crate::ProducerAuthentication;

fn parse(values: [Option<&str>; 3]) -> io::Result<ProducerConfig> {
    let mut values = values.into_iter();
    config_from_launch_environment(|_| {
        Ok(values
            .next()
            .unwrap()
            .map(|value| zeroize::Zeroizing::new(value.to_owned())))
    })
}

#[test]
fn private_launch_credentials_select_exact_authority_without_root_fallback() {
    assert!(matches!(
        parse([None; 3]).unwrap().authentication,
        ProducerAuthentication::RootFromEnvironment
    ));
    let secret = "ab".repeat(32);
    let config = parse([Some("1"), Some("2"), Some(&secret)]).unwrap();
    assert!(matches!(
        config.authentication,
        ProducerAuthentication::LeaseActivation {
            context_id: 1,
            lease_id: 2,
            ..
        }
    ));
    for values in [
        [None, Some("2"), Some(secret.as_str())],
        [Some("1"), None, Some(secret.as_str())],
        [Some("1"), Some("2"), None],
        [Some("0"), Some("2"), Some(secret.as_str())],
        [Some("-1"), Some("2"), Some(secret.as_str())],
        [
            Some("18446744073709551616"),
            Some("2"),
            Some(secret.as_str()),
        ],
        [Some("1"), Some("2"), Some("malformed-private-credential")],
    ] {
        let error = parse(values).unwrap_err();
        assert!(!error.to_string().contains(&secret));
        assert!(!error.to_string().contains("malformed-private-credential"));
    }
    assert!(
        config_from_launch_environment(|_| {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid GUI lease environment",
            ))
        })
        .is_err()
    );
}

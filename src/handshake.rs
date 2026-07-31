//! HELLO construction and the lease identity it carries.
//!
//! Authentication material is assembled here and nowhere else, so the rules about what may appear
//! in a HELLO — and what must never be logged or reconstructed from one — have a single home.

use std::{env, io};

use vivid_protocol::auth::{self, Secret32};
use vivid_protocol::messages::{Hello, HelloAuthentication};

use crate::*;

pub(crate) fn producer_lease_identity(
    authentication: &ProducerAuthentication,
) -> Option<(u64, u64)> {
    match authentication {
        ProducerAuthentication::LeaseActivation {
            context_id,
            lease_id,
            ..
        }
        | ProducerAuthentication::Resume {
            context_id,
            lease_id,
            ..
        } => Some((*context_id, *lease_id)),
        ProducerAuthentication::RootFromEnvironment | ProducerAuthentication::Root { .. } => None,
    }
}

pub(crate) fn hello_lease_identity(hello: &Hello) -> Option<(u64, u64)> {
    match &hello.authentication {
        HelloAuthentication::LeaseActivation {
            context_id,
            lease_id,
            ..
        }
        | HelloAuthentication::Resume {
            context_id,
            lease_id,
            ..
        } => Some((*context_id, *lease_id)),
        HelloAuthentication::Root { .. } => None,
    }
}

pub(crate) fn build_hello(
    config: &ProducerConfig,
    preface: &[u8; 16],
) -> io::Result<(Hello, Secret32)> {
    let mut client_nonce = [0; auth::NONCE_BYTES];
    random_bytes(&mut client_nonce)?;
    let (authentication, session_secret) = match &config.authentication {
        ProducerAuthentication::RootFromEnvironment => {
            let value = env::var(vivid_protocol::discovery::ROOT_SECRET).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "VIVID_ROOT_SECRET is required for root authentication",
                )
            })?;
            let secret =
                Secret32::from_hex(&value).map_err(|error| invalid_input(error.to_string()))?;
            (HelloAuthentication::Root { proof: [0; 32] }, secret)
        }
        ProducerAuthentication::Root { root_secret } => (
            HelloAuthentication::Root { proof: [0; 32] },
            Secret32::new(*root_secret.expose()),
        ),
        ProducerAuthentication::LeaseActivation {
            context_id,
            lease_id,
            activation_secret,
            attempt_id,
            proof_of_possession,
        } => (
            HelloAuthentication::LeaseActivation {
                context_id: *context_id,
                lease_id: *lease_id,
                activation_secret: Secret32::new(*activation_secret.expose()),
                attempt_id: *attempt_id,
                proof_of_possession: proof_of_possession.clone(),
            },
            Secret32::new(*activation_secret.expose()),
        ),
        ProducerAuthentication::Resume {
            context_id,
            lease_id,
            session_id,
            resume_generation,
            attempt_id,
            prior_resume_key,
        } => (
            HelloAuthentication::Resume {
                context_id: *context_id,
                lease_id: *lease_id,
                session_id: *session_id,
                resume_generation: *resume_generation,
                attempt_id: *attempt_id,
                proof: [0; 32],
            },
            Secret32::new(*prior_resume_key.expose()),
        ),
    };
    let mut hello = Hello {
        producer_name: config.producer_name.clone(),
        producer_version: config.producer_version.clone(),
        required_profiles: config.required_profiles.clone(),
        optional_profiles: config.optional_profiles.clone(),
        maximum_control_body: config.maximum_control_body,
        client_nonce,
        authentication,
        target_profile: config.target_profile.clone(),
        extensions: vec![],
    };
    match &config.authentication {
        ProducerAuthentication::RootFromEnvironment | ProducerAuthentication::Root { .. } => {
            hello.authenticate_root(&session_secret, preface)?;
        }
        ProducerAuthentication::Resume { .. } => {
            hello.authenticate_resume(session_secret.expose(), preface)?;
        }
        ProducerAuthentication::LeaseActivation { .. } => {
            hello.validate()?;
        }
    }
    Ok((hello, session_secret))
}

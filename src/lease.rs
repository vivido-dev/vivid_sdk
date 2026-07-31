//! Contexts and bounded session leases: the delegation half of the authority model.

use std::io;

use vivid_protocol::cbor::Value;
use vivid_protocol::messages;
use vivid_protocol::messages::PayloadMap;
use vivid_protocol::resource::ResourceContract;
use vivid_protocol::revision::{ChannelGeneration, TrackRevision};

use crate::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextReady {
    pub context_id: u64,
    pub operation_classes: u64,
    pub contract: ResourceContract,
    pub lifetime_us: u64,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionLeaseReady {
    pub context_id: u64,
    pub lease_id: u64,
    pub state: u64,
    pub activation_timeout_us: u64,
    pub disconnect_grace_us: u64,
    pub cleanup_policy: u64,
    pub permitted_profiles: Vec<String>,
    pub contract: ResourceContract,
    pub revision: u64,
}

pub(crate) struct TrackReadyValues {
    pub(crate) revision: TrackRevision,
    pub(crate) generation: ChannelGeneration,
    pub(crate) open_deadline_us: u64,
    pub(crate) maximum_record_body: u32,
    pub(crate) effective_claims: PayloadMap,
    pub(crate) connection_required: bool,
    pub(crate) delta_operation_limit: u32,
}

impl Session {
    pub fn create_context(
        &mut self,
        definition: &ContextDefinition,
        metadata: &RequestMetadata,
    ) -> io::Result<ContextReady> {
        definition.validate(definition.context_id)?;
        let reply = self.request(
            messages::CREATE_CONTEXT,
            definition.context_id,
            definition.payload()?,
            metadata,
            None,
            None,
        )?;
        if let Some(record) = reply {
            expect_record(&record, messages::CONTEXT_READY, definition.context_id)?;
            let payload = decoded_payload(&record)?;
            validate_exact_payload_keys("CONTEXT_READY", &payload, 0..=4)?;
            let ready = ContextReady {
                context_id: required_u64(&payload, 0)?,
                operation_classes: required_u64(&payload, 1)?,
                contract: required_contract(&payload, 2)?,
                lifetime_us: required_u64(&payload, 3)?,
                revision: required_u64(&payload, 4)?,
            };
            if ready.context_id != definition.context_id
                || ready.operation_classes & !definition.operation_classes != 0
                || ready.lifetime_us > definition.lifetime_us
                || ready.revision == 0
            {
                return Err(invalid_data(
                    "CONTEXT_READY contains invalid effective identity or authority",
                ));
            }
            Ok(ready)
        } else {
            Ok(ContextReady {
                context_id: definition.context_id,
                operation_classes: definition.operation_classes,
                contract: definition.requested_contract.clone(),
                lifetime_us: definition.lifetime_us,
                revision: 1,
            })
        }
    }

    pub fn create_session_lease(
        &mut self,
        definition: &SessionLeaseDefinition,
        metadata: &RequestMetadata,
    ) -> io::Result<SessionLeaseReady> {
        definition.validate()?;
        let reply = self.request(
            messages::CREATE_SESSION_LEASE,
            definition.lease_id,
            definition.payload()?,
            metadata,
            None,
            None,
        )?;
        if let Some(record) = reply {
            expect_record(&record, messages::SESSION_LEASE_READY, definition.lease_id)?;
            let payload = decoded_payload(&record)?;
            validate_exact_payload_keys("SESSION_LEASE_READY", &payload, 0..=8)?;
            let ready = SessionLeaseReady {
                context_id: required_u64(&payload, 0)?,
                lease_id: required_u64(&payload, 1)?,
                state: required_u64(&payload, 2)?,
                activation_timeout_us: required_u64(&payload, 3)?,
                disconnect_grace_us: required_u64(&payload, 4)?,
                cleanup_policy: required_u64(&payload, 5)?,
                permitted_profiles: required_text_array(&payload, 6)?,
                contract: required_contract(&payload, 7)?,
                revision: required_u64(&payload, 8)?,
            };
            if ready.context_id != definition.context_id
                || ready.lease_id != definition.lease_id
                || ready.state != 1
                || ready.activation_timeout_us == 0
                || ready.activation_timeout_us > definition.activation_timeout_us
                || ready.disconnect_grace_us > definition.requested_disconnect_grace_us
                || ready.cleanup_policy != definition.cleanup_policy as u64
                || ready.revision == 0
                || validate_profiles(&ready.permitted_profiles).is_err()
                || ready
                    .permitted_profiles
                    .iter()
                    .any(|profile| !definition.permitted_profiles.contains(profile))
            {
                return Err(invalid_data(
                    "SESSION_LEASE_READY contains invalid effective lease state",
                ));
            }
            Ok(ready)
        } else {
            Ok(SessionLeaseReady {
                context_id: definition.context_id,
                lease_id: definition.lease_id,
                state: 1,
                activation_timeout_us: definition.activation_timeout_us,
                disconnect_grace_us: definition.requested_disconnect_grace_us,
                cleanup_policy: definition.cleanup_policy as u64,
                permitted_profiles: definition.permitted_profiles.clone(),
                contract: definition.requested_contract.clone(),
                revision: 1,
            })
        }
    }

    /// Revoke a lease this session issued.
    ///
    /// Security §8: cleanup is synchronous, so once this returns the child session, its objects,
    /// and its reserved capacity are gone.
    pub fn revoke_session_lease(
        &self,
        context_id: u64,
        lease_id: u64,
        metadata: &RequestMetadata,
    ) -> io::Result<()> {
        let reply = self.request(
            messages::REVOKE_SESSION_LEASE,
            lease_id,
            vec![
                (0, Value::Unsigned(context_id)),
                (1, Value::Unsigned(lease_id)),
            ],
            metadata,
            None,
            None,
        )?;
        if let Some(record) = reply {
            expect_record(&record, messages::OK, lease_id)?;
        }
        Ok(())
    }

    /// Replace the session observation mask, core §10.
    ///
    /// Observations are non-actionable: they may be coalesced latest-wins and dropped under
    /// bounded writer pressure, and `OBSERVATION_GAP` names what was lost so current truth is
    /// recovered by query rather than assumed.
    pub fn set_observation(&self, mask: u64) -> io::Result<()> {
        let reply = self.request(
            messages::SET_OBSERVATION,
            0,
            vec![(0, Value::Unsigned(mask))],
            &RequestMetadata::default(),
            None,
            None,
        )?;
        if let Some(record) = reply {
            expect_record(&record, messages::OK, 0)?;
        }
        Ok(())
    }

    /// The session's reconciliation root, core §10.
    ///
    /// After an authenticated resume a producer compares these revisions against what it retained
    /// rather than replaying requests, so the payload is returned unparsed for the caller to walk.
    pub fn query_session(&self) -> io::Result<PayloadMap> {
        let reply = self.request(
            messages::QUERY_SESSION,
            0,
            Vec::new(),
            &RequestMetadata::default(),
            None,
            None,
        )?;
        let Some(record) = reply else {
            return Ok(Vec::new());
        };
        expect_record(&record, messages::SESSION_STATUS, 0)?;
        decoded_payload(&record)
    }
}

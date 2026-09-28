//! Contexts, bounded session leases, and resumable authentication.
//!
//! A lease hands a bounded piece of this producer's authority to another: a context, a profile
//! set, a resource contract, and a deadline. Its activation secret is the lease's whole capability,
//! so it is returned once, as a second value, and never stored in the table describing the lease.

use mlua::prelude::*;
use vivid_protocol::cbor::Value;
use vivid_protocol::context::ContextDefinition;
use vivid_protocol::lease::CleanupPolicy;
use vivid_protocol::resource::{RESOURCE_COUNT, ResourceContract};
use vivid_sdk::{ProducerAuthentication, RequestMetadata, SessionLeaseBuilder};

use crate::convert::{arg, check_keys, get, hex, need};
use crate::error::{IoResultExt, invalid, vivid};
use crate::session::LuaSession;

fn contract_to_array(contract: &ResourceContract) -> Vec<u64> {
    // The CBOR map's keys are the resource indices, which is exactly the array order.
    let mut values = vec![0_u64; RESOURCE_COUNT];
    if let Value::Map(entries) = contract.to_value() {
        for (key, value) in entries {
            if let (Ok(index), Some(number)) = (usize::try_from(key), value.as_u64())
                && index < RESOURCE_COUNT
            {
                values[index] = number;
            }
        }
    }
    values
}

fn contract_from(config: &LuaTable, session: &LuaSession) -> LuaResult<ResourceContract> {
    match get::<Vec<u64>>(config, "contract")? {
        // Omitted, the session's own contract is inherited, which is what a worker wants.
        None => Ok(session.get()?.info().resource_contract.clone()),
        Some(values) => {
            let values: [u64; RESOURCE_COUNT] = values.try_into().map_err(|_| {
                invalid(format!(
                    "a contract must name all {RESOURCE_COUNT} resources"
                ))
            })?;
            Ok(ResourceContract::new(values))
        }
    }
}

pub fn add_session_methods<M: LuaUserDataMethods<LuaSession>>(methods: &mut M) {
    methods.add_method_mut("create_context", |lua, this, config: LuaValue| {
        let config = arg::<LuaTable>(config, "context definition")?;
        check_keys(
            &config,
            &[
                "context_id",
                "parent_context_id",
                "operation_classes",
                "label",
                "lifetime_us",
                "contract",
            ],
            "context definition",
        )?;
        let definition = ContextDefinition {
            context_id: need(&config, "context_id")?,
            parent_context_id: need(&config, "parent_context_id")?,
            operation_classes: need(&config, "operation_classes")?,
            label: get(&config, "label")?.unwrap_or_default(),
            lifetime_us: get(&config, "lifetime_us")?.unwrap_or(0),
            requested_contract: contract_from(&config, this)?,
        };
        let ready = this
            .get_mut()?
            .create_context(&definition, &RequestMetadata::default())
            .lua()?;
        let table = lua.create_table()?;
        table.set("context_id", ready.context_id)?;
        table.set("operation_classes", ready.operation_classes)?;
        table.set("contract", contract_to_array(&ready.contract))?;
        table.set("lifetime_us", ready.lifetime_us)?;
        table.set("revision", ready.revision)?;
        Ok(table)
    });

    // Returns the lease and, as a second value, the activation secret as hex. The secret is
    // capability material: hand it over through an authenticated channel, never a command line.
    methods.add_method_mut("create_session_lease", |lua, this, config: LuaValue| {
        let config = arg::<LuaTable>(config, "session lease")?;
        check_keys(
            &config,
            &[
                "context_id",
                "lease_id",
                "permitted_profiles",
                "activation_timeout_us",
                "disconnect_grace_us",
                "cleanup_policy",
                "contract",
            ],
            "session lease",
        )?;
        let mut profiles = need::<Vec<String>>(&config, "permitted_profiles")?;
        profiles.sort();
        profiles.dedup();
        let cleanup = CleanupPolicy::try_from(
            get::<u64>(&config, "cleanup_policy")?
                .unwrap_or(CleanupPolicy::SuspendOnUncleanLoss as u64),
        )
        .map_err(|error| invalid(error.to_string()))?;
        let mut builder =
            SessionLeaseBuilder::new(need(&config, "context_id")?, need(&config, "lease_id")?)
                .permitted_profiles(profiles)
                .disconnect_grace_us(get(&config, "disconnect_grace_us")?.unwrap_or(0))
                .cleanup_policy(cleanup)
                .contract(contract_from(&config, this)?);
        if let Some(timeout) = get(&config, "activation_timeout_us")? {
            builder = builder.activation_timeout_us(timeout);
        }
        let (definition, mut secret) = builder.build().lua()?;
        let ready = this
            .get_mut()?
            .create_session_lease(&definition, &RequestMetadata::default())
            .lua()?;
        let table = lua.create_table()?;
        table.set("context_id", ready.context_id)?;
        table.set("lease_id", ready.lease_id)?;
        table.set("state", ready.state)?;
        table.set("activation_timeout_us", ready.activation_timeout_us)?;
        table.set("disconnect_grace_us", ready.disconnect_grace_us)?;
        table.set("cleanup_policy", ready.cleanup_policy)?;
        table.set("permitted_profiles", ready.permitted_profiles)?;
        table.set("contract", contract_to_array(&ready.contract))?;
        table.set("revision", ready.revision)?;
        let secret = match secret.take() {
            Some(value) => {
                let text = zeroize::Zeroizing::new(hex(value.expose()));
                LuaValue::String(lua.create_string(text.as_bytes())?)
            }
            None => LuaValue::Nil,
        };
        Ok((table, secret))
    });

    methods.add_method(
        "revoke_session_lease",
        |_, this, (context_id, lease_id): (LuaValue, LuaValue)| {
            this.get()?
                .revoke_session_lease(
                    arg(context_id, "context_id")?,
                    arg(lease_id, "lease_id")?,
                    &RequestMetadata::default(),
                )
                .lua()
        },
    );
    methods.add_method("set_observation", |_, this, mask: LuaValue| {
        this.get()?.set_observation(arg(mask, "mask")?).lua()
    });
    // The identity a resuming producer needs. Root sessions are deliberately non-resumable and
    // say so, rather than yielding an identity that could never be used.
    methods.add_method("prepare_resume", |lua, this, ()| {
        match this.get()?.resume_authentication().lua()? {
            ProducerAuthentication::Resume {
                context_id,
                lease_id,
                session_id,
                resume_generation,
                ..
            } => {
                let table = lua.create_table()?;
                table.set("context_id", context_id)?;
                table.set("lease_id", lease_id)?;
                table.set("session_id", session_id)?;
                table.set("resume_generation", resume_generation)?;
                Ok(table)
            }
            _ => Err(vivid(
                "resume authentication is only defined for resumed sessions",
            )),
        }
    });
}

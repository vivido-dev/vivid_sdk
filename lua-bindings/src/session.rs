//! The producer core: sessions, surfaces, tracks, and track channels.
//!
//! Handles are userdata with methods, the shape a Lua caller expects: `session:create_surface{..}`.
//! Configuration arrives as tables and is resolved by the SDK's own builders, so identity
//! defaults, resource claims, and container parsing come from `vivid_sdk`, never from here.

use std::time::Duration;

use mlua::prelude::*;
use vivid_protocol::media::{AudioPacket, RasterDeltaOperation, VideoPacket};
use vivid_protocol::messages::{LaneClass, TrackKind};
use vivid_protocol::scene::{Fit, SceneNode};
use vivid_protocol::track::{KindConfiguration, TrackConfiguration, TrackMode};
use vivid_sdk::{
    ChannelEvent, CoordinateModel, OutputDescriptor, ProducerAuthentication, ProducerConfig,
    RequestMetadata, Rotation, SceneCommit, SendPressure, Session, SessionEvent, SlotBinding,
    Surface, SurfaceDefinition, SurfaceDescriptor, SurfaceRole, SurfaceStatus, Track, TrackChannel,
    TrackStatus, TrackWaitCondition,
};

use crate::convert::{
    Bytes, Field, arg, check_keys, geometry, get, hex, micros, need, opt, options, payload_to_lua,
    sequence, timeout,
};
use crate::error::{IoResultExt, closed, invalid};

/// The protocol's bound on one track wait, and the default for every event wait.
pub const MAX_WAIT: Duration = Duration::from_micros(vivid_protocol::MAX_TRACK_WAIT_TIMEOUT_US);

// ---------------------------------------------------------------------------
// Connect
// ---------------------------------------------------------------------------

const CONNECT_KEYS: &[&str] = &[
    "dry_run",
    "desktop",
    "trace_dir",
    "endpoint_control",
    "endpoint_interactive",
    "endpoint_realtime",
    "endpoint_bulk",
    "root_secret",
    "producer_name",
    "producer_version",
    "target_profile",
    "required_profiles",
    "optional_profiles",
];

/// Producer configuration from a connect options table.
///
/// Profile lists are normalized the way the Python package normalizes them — sorted, deduplicated,
/// and with required profiles removed from the optional set — because the SDK validates the
/// canonical form and a Lua caller should not have to produce it by hand. Naming a target without
/// naming the required set swaps the target into the preset's required set.
pub fn producer_config(config: &LuaTable) -> LuaResult<ProducerConfig> {
    check_keys(config, CONNECT_KEYS, "connect option")?;
    let mut base = if get::<bool>(config, "desktop")?.unwrap_or(false) {
        ProducerConfig::desktop()
    } else {
        ProducerConfig::default()
    };
    base.producer_name = "vivid-sdk-lua".into();
    base.producer_version = env!("CARGO_PKG_VERSION").into();
    let preset_target = base.target_profile.clone();
    if let Some(target) = get::<String>(config, "target_profile")? {
        base.target_profile = target;
    }
    let mut required = match get::<Vec<String>>(config, "required_profiles")? {
        Some(profiles) => profiles,
        None => base
            .required_profiles
            .iter()
            .map(|profile| {
                if *profile == preset_target {
                    base.target_profile.clone()
                } else {
                    profile.clone()
                }
            })
            .collect(),
    };
    required.sort();
    required.dedup();
    let mut optional = get::<Vec<String>>(config, "optional_profiles")?
        .unwrap_or_else(|| base.optional_profiles.clone());
    optional.sort();
    optional.dedup();
    optional.retain(|profile| !required.contains(profile));
    base.required_profiles = required;
    base.optional_profiles = optional;

    base.endpoint_control = get(config, "endpoint_control")?;
    base.endpoint_interactive = get(config, "endpoint_interactive")?;
    base.endpoint_realtime = get(config, "endpoint_realtime")?;
    base.endpoint_bulk = get(config, "endpoint_bulk")?;
    if let Some(name) = get(config, "producer_name")? {
        base.producer_name = name;
    }
    if let Some(version) = get(config, "producer_version")? {
        base.producer_version = version;
    }
    if let Some(dry_run) = get(config, "dry_run")? {
        base.dry_run = dry_run;
    }
    base.trace_dir = get::<String>(config, "trace_dir")?.map(Into::into);
    if let Some(secret) = get::<String>(config, "root_secret")? {
        let secret = zeroize::Zeroizing::new(secret);
        base.authentication = ProducerAuthentication::root_hex(&secret).lua()?;
    }
    Ok(base)
}

/// `vivid.connect(options)`.
pub fn connect(lua: &Lua, value: LuaValue) -> LuaResult<LuaSession> {
    let config = producer_config(&options(lua, value, "connect options")?)?;
    Ok(LuaSession::new(Session::connect(config).lua()?))
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

pub struct LuaSession {
    inner: Option<Session>,
}

impl LuaSession {
    pub fn new(session: Session) -> Self {
        Self {
            inner: Some(session),
        }
    }

    pub fn closed(&self) -> bool {
        self.inner.is_none()
    }

    pub fn get(&self) -> LuaResult<&Session> {
        self.inner.as_ref().ok_or_else(|| closed("session"))
    }

    pub fn get_mut(&mut self) -> LuaResult<&mut Session> {
        self.inner.as_mut().ok_or_else(|| closed("session"))
    }

    /// Take the session out, for an owner that adopts it: a pane, a desktop, an overlay.
    pub fn take(&mut self) -> LuaResult<Session> {
        self.inner.take().ok_or_else(|| closed("session"))
    }
}

fn metadata() -> RequestMetadata {
    RequestMetadata::default()
}

impl LuaUserData for LuaSession {
    fn add_fields<F: LuaUserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("closed", |_, this| Ok(this.inner.is_none()));
    }

    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(LuaMetaMethod::ToString, |_, this, ()| {
            Ok(match &this.inner {
                Some(session) => format!(
                    "vivid_sdk.Session(id={}, target_profile={:?})",
                    session.info().session_id,
                    session.info().target_profile
                ),
                None => "vivid_sdk.Session(closed)".into(),
            })
        });
        #[cfg(any(feature = "lua54", feature = "lua55"))]
        methods.add_meta_method_mut(LuaMetaMethod::Close, |_, this, _: LuaMultiValue| match this
            .inner
            .take()
        {
            Some(session) => session.close().lua(),
            None => Ok(()),
        });

        // -- Lifecycle -------------------------------------------------------
        methods.add_method_mut("close", |_, this, ()| this.take()?.close().lua());
        methods.add_method_mut("abort", |_, this, ()| this.get_mut()?.abort().lua());
        methods.add_method("info", |lua, this, ()| session_info(lua, this.get()?));
        methods.add_method("supports", |_, this, profile: LuaValue| {
            Ok(this.get()?.supports(&arg::<String>(profile, "profile")?))
        });
        methods.add_method("allocate_id", |_, this, ()| this.get()?.allocate_id().lua());

        // -- Events ----------------------------------------------------------
        methods.add_method("take_event", |lua, this, ()| {
            match this.get()?.take_event().lua()? {
                Some(event) => session_event(lua, event).map(LuaValue::Table),
                None => Ok(LuaValue::Nil),
            }
        });
        methods.add_method("wait_event", |lua, this, value: LuaValue| {
            let timeout = timeout(value, MAX_WAIT, "timeout")?;
            match this.get()?.wait_event(timeout).lua()? {
                Some(event) => session_event(lua, event).map(LuaValue::Table),
                None => Ok(LuaValue::Nil),
            }
        });
        methods.add_function("events", |lua, (ud, value): (LuaAnyUserData, LuaValue)| {
            let timeout = timeout(value, MAX_WAIT, "timeout")?;
            ud.borrow::<LuaSession>()?;
            lua.create_function(move |lua, _: LuaMultiValue| {
                // The iteration ends at `connection_closed`, the last event a session produces, at
                // a quiet `timeout`, or when the session was closed underneath it.
                let session = ud.borrow::<LuaSession>()?;
                let Some(session) = session.inner.as_ref() else {
                    return Ok(LuaValue::Nil);
                };
                match session.wait_event(timeout).lua()? {
                    Some(SessionEvent::ConnectionClosed { .. }) | None => Ok(LuaValue::Nil),
                    Some(event) => session_event(lua, event).map(LuaValue::Table),
                }
            })
        });

        // -- Surfaces --------------------------------------------------------
        methods.add_method("build_surface_config", |lua, this, config: LuaValue| {
            let config = arg::<LuaTable>(config, "surface configuration")?;
            let definition = surface_definition(this.get()?, &config, None)?;
            surface_definition_table(lua, &definition)
        });
        methods.add_method_mut("create_surface", |_, this, config: LuaValue| {
            let config = arg::<LuaTable>(config, "surface configuration")?;
            let session = this.get_mut()?;
            let definition = surface_definition(session, &config, None)?;
            Ok(LuaSurface {
                inner: session.create_surface(definition, &metadata()).lua()?,
            })
        });
        methods.add_method_mut(
            "update_surface",
            |_, this, (surface, config): (LuaUserDataRef<LuaSurface>, LuaValue)| {
                let config = arg::<LuaTable>(config, "surface configuration")?;
                let session = this.get_mut()?;
                let identity = (surface.inner.context_id(), surface.inner.id());
                let definition = surface_definition(session, &config, Some(identity))?;
                session
                    .update_surface(&surface.inner, definition, &metadata())
                    .lua()
            },
        );
        methods.add_method_mut(
            "destroy_surface",
            |_, this, surface: LuaUserDataRef<LuaSurface>| {
                this.get_mut()?
                    .destroy_surface(&surface.inner, &metadata())
                    .lua()
            },
        );
        methods.add_method(
            "query_surface",
            |lua, this, surface: LuaUserDataRef<LuaSurface>| {
                let status = this.get()?.query_surface(&surface.inner).lua()?;
                surface_status(lua, &status)
            },
        );

        // -- Tracks ----------------------------------------------------------
        methods.add_method(
            "build_track_config",
            |lua, this, (surface, config): (LuaUserDataRef<LuaSurface>, LuaValue)| {
                let config = arg::<LuaTable>(config, "track configuration")?;
                let configuration = track_configuration(
                    this.get()?,
                    surface.inner.context_id(),
                    surface.inner.id(),
                    &config,
                )?;
                track_configuration_table(lua, &configuration)
            },
        );
        methods.add_method_mut(
            "create_track",
            |_, this, (surface, config): (LuaUserDataRef<LuaSurface>, LuaValue)| {
                let config = arg::<LuaTable>(config, "track configuration")?;
                let session = this.get_mut()?;
                let configuration = track_configuration(
                    session,
                    surface.inner.context_id(),
                    surface.inner.id(),
                    &config,
                )?;
                Ok(LuaTrack {
                    inner: session.create_track(configuration, &metadata()).lua()?,
                })
            },
        );
        methods.add_method_mut(
            "probe_track",
            |lua, this, (surface, config): (LuaUserDataRef<LuaSurface>, LuaValue)| {
                let config = arg::<LuaTable>(config, "track configuration")?;
                let session = this.get_mut()?;
                let mut configuration = track_configuration(
                    session,
                    surface.inner.context_id(),
                    surface.inner.id(),
                    &config,
                )?;
                // A probe names no track: the protocol requires key 2 to be zero.
                configuration.track_id = 0;
                let support = session.probe_track(&configuration).lua()?;
                let table = lua.create_table()?;
                table.set("supported", support.supported)?;
                table.set("selected_decoder", support.selected_decoder)?;
                table.set("capability_generation", support.capability_generation)?;
                table.set(
                    "effective_claims",
                    payload_to_lua(lua, &support.effective_claims)?,
                )?;
                Ok(table)
            },
        );
        methods.add_method_mut(
            "destroy_track",
            |_, this, track: LuaUserDataRef<LuaTrack>| {
                this.get_mut()?
                    .destroy_track(&track.inner, &metadata())
                    .lua()
            },
        );
        methods.add_method(
            "query_track",
            |lua, this, track: LuaUserDataRef<LuaTrack>| {
                let status = this.get()?.query_track(&track.inner).lua()?;
                track_status(lua, &status)
            },
        );
        methods.add_method(
            "wait_track",
            |lua,
             this,
             (track, condition, value, wait): (
                LuaUserDataRef<LuaTrack>,
                LuaValue,
                LuaValue,
                LuaValue,
            )| {
                let condition = TrackWaitCondition::try_from(arg::<u64>(condition, "condition")?)
                    .map_err(|error| invalid(error.to_string()))?;
                let value = opt::<u64>(value, "value")?;
                let wait = timeout(wait, MAX_WAIT, "timeout")?;
                let result = this
                    .get()?
                    .wait_track(&track.inner, condition, value, micros(wait))
                    .lua()?;
                let table = lua.create_table()?;
                table.set("context_id", result.context_id)?;
                table.set("surface_id", result.surface_id)?;
                table.set("track_id", result.track_id)?;
                table.set("revision", result.revision.get())?;
                table.set("channel_generation", result.channel_generation.get())?;
                table.set("condition", result.condition as u64)?;
                table.set("observed_value", result.observed_value)?;
                Ok(table)
            },
        );
        methods.add_method_mut(
            "activate_track",
            |_,
             this,
             (surface, track, milestone): (
                LuaUserDataRef<LuaSurface>,
                LuaUserDataRef<LuaTrack>,
                LuaValue,
            )| {
                let configuration = track.inner.configuration().lua()?;
                let binding = SlotBinding {
                    slot: configuration.slot,
                    track_id: track.inner.id(),
                    expected_channel_generation: track.inner.channel_generation(),
                    required_milestone: opt::<u64>(milestone, "required_milestone")?
                        .unwrap_or(vivid_sdk::MILESTONE_OUTPUT_READY),
                };
                this.get_mut()?
                    .activate_tracks(&surface.inner, &[binding], &metadata())
                    .lua()
            },
        );
        methods.add_method_mut(
            "activate_tracks",
            |_, this, (surface, bindings): (LuaUserDataRef<LuaSurface>, LuaValue)| {
                let bindings = slot_bindings(&arg::<LuaTable>(bindings, "bindings")?)?;
                this.get_mut()?
                    .activate_tracks(&surface.inner, &bindings, &metadata())
                    .lua()
            },
        );

        // -- Channels --------------------------------------------------------
        methods.add_method(
            "open_track_channel",
            |_, this, track: LuaUserDataRef<LuaTrack>| {
                let channel = this.get()?.open_track_channel(&track.inner).lua()?;
                Ok(LuaTrackChannel::new(&track.inner, channel))
            },
        );
        methods.add_method_mut(
            "advance_channel",
            |_, this, (track, reason): (LuaUserDataRef<LuaTrack>, LuaValue)| {
                let reason = arg::<u64>(reason, "reason")?;
                let session = this.get_mut()?;
                let generation = session
                    .advance_channel(&track.inner, reason, &metadata())
                    .lua()?;
                let channel = session.open_track_channel(&track.inner).lua()?;
                if channel.generation() != generation {
                    return Err(crate::error::vivid(
                        "channel advance did not produce the requested generation",
                    ));
                }
                Ok(LuaTrackChannel::new(&track.inner, channel))
            },
        );

        // -- Scene -----------------------------------------------------------
        methods.add_method_mut(
            "place_terminal_surface",
            |lua, this, (surface, placement): (LuaUserDataRef<LuaSurface>, LuaValue)| {
                let placement = arg::<LuaTable>(placement, "placement")?;
                check_keys(
                    &placement,
                    &["node_id", "x", "y", "width", "height", "text_layer"],
                    "placement",
                )?;
                let session = this.get_mut()?;
                let node_id = match get::<u64>(&placement, "node_id")? {
                    Some(node_id) => node_id,
                    None => session.allocate_id().lua()?,
                };
                let x = fixed(get::<f64>(&placement, "x")?.unwrap_or(0.0), "x")?;
                let y = fixed(get::<f64>(&placement, "y")?.unwrap_or(0.0), "y")?;
                let width = fixed(need::<f64>(&placement, "width")?, "width")?;
                let height = fixed(need::<f64>(&placement, "height")?, "height")?;
                let text_layer = get::<u64>(&placement, "text_layer")?.unwrap_or(1);
                let commit = session
                    .place_terminal_surface(
                        &surface.inner,
                        node_id,
                        x,
                        y,
                        width,
                        height,
                        text_layer,
                    )
                    .lua()?;
                scene_commit(lua, commit)
            },
        );
        methods.add_method_mut(
            "create_node",
            |lua, this, (surface, node): (LuaUserDataRef<LuaSurface>, LuaValue)| {
                let session = this.get_mut()?;
                let node = scene_node(session, &surface.inner, &arg::<LuaTable>(node, "node")?)?;
                scene_commit(lua, session.create_node(&node, &metadata()).lua()?)
            },
        );
        methods.add_method_mut(
            "update_node",
            |lua, this, (surface, node): (LuaUserDataRef<LuaSurface>, LuaValue)| {
                let session = this.get_mut()?;
                let node = scene_node(session, &surface.inner, &arg::<LuaTable>(node, "node")?)?;
                scene_commit(lua, session.update_node(&node, &metadata()).lua()?)
            },
        );
        methods.add_method_mut(
            "delete_node",
            |lua, this, (context_id, node_id): (LuaValue, LuaValue)| {
                let context_id = arg::<u64>(context_id, "context_id")?;
                let node_id = arg::<u64>(node_id, "node_id")?;
                let commit = this
                    .get_mut()?
                    .delete_node(context_id, node_id, &metadata())
                    .lua()?;
                scene_commit(lua, commit)
            },
        );

        // -- Anchors ---------------------------------------------------------
        methods.add_method(
            "anchor_marker",
            |_, this, (context_id, anchor_id): (LuaValue, LuaValue)| {
                let session = this.get()?;
                let context_id =
                    opt::<u64>(context_id, "context_id")?.unwrap_or(session.info().root_context_id);
                let anchor_id = match opt::<u64>(anchor_id, "anchor_id")? {
                    Some(anchor_id) => anchor_id,
                    None => session.allocate_id().lua()?,
                };
                session.anchor_marker(context_id, anchor_id).lua()
            },
        );
        methods.add_method(
            "conpty_anchor_marker",
            |_, this, (context_id, anchor_id): (LuaValue, LuaValue)| {
                this.get()?
                    .conpty_anchor_marker(
                        arg::<u64>(context_id, "context_id")?,
                        arg::<u64>(anchor_id, "anchor_id")?,
                    )
                    .lua()
            },
        );
        methods.add_method(
            "query_anchor",
            |lua, this, (context_id, anchor_id): (LuaValue, LuaValue)| {
                let context_id = arg::<u64>(context_id, "context_id")?;
                let anchor_id = arg::<u64>(anchor_id, "anchor_id")?;
                let status = match this.get()?.query_anchor(context_id, anchor_id) {
                    Ok(status) => status,
                    // An anchor the presenter never saw is an answer, not a failure.
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        return Ok(LuaValue::Nil);
                    }
                    Err(error) => return Err(crate::error::io(error)),
                };
                let table = lua.create_table()?;
                table.set("context_id", status.context_id)?;
                table.set("anchor_id", status.anchor_id)?;
                table.set("state", status.state)?;
                table.set(
                    "target_generation",
                    status.target_generation.map(|generation| generation.get()),
                )?;
                table.set("payload", payload_to_lua(lua, &status.payload)?)?;
                Ok(LuaValue::Table(table))
            },
        );
        methods.add_method("query_session", |lua, this, ()| {
            let payload = this.get()?.query_session().lua()?;
            payload_to_lua(lua, &payload)
        });

        // -- Timed playback --------------------------------------------------
        methods.add_method_mut(
            "play",
            |lua, this, (track, value): (LuaUserDataRef<LuaTrack>, LuaValue)| {
                let config = options(lua, value, "play options")?;
                check_keys(
                    &config,
                    &[
                        "start_pts_us",
                        "minimum_buffer_us",
                        "maximum_latency_us",
                        "synchronized",
                        "hold_serial",
                    ],
                    "play option",
                )?;
                let options = vivid_sdk::PlayOptions {
                    start_pts_us: get(&config, "start_pts_us")?.unwrap_or(0),
                    minimum_buffer_us: get(&config, "minimum_buffer_us")?.unwrap_or(0),
                    maximum_latency_us: get(&config, "maximum_latency_us")?.unwrap_or(0),
                    hold_serial: get(&config, "hold_serial")?,
                    start_policy: if get(&config, "synchronized")?.unwrap_or(false) {
                        vivid_sdk::StartPolicy::Synchronized
                    } else {
                        vivid_sdk::StartPolicy::AfterMinimumBuffer
                    },
                };
                this.get_mut()?.play_with(&track.inner, options).lua()
            },
        );
        methods.add_method_mut("pause", |_, this, track: LuaUserDataRef<LuaTrack>| {
            this.get_mut()?.pause(&track.inner).lua()
        });
        methods.add_method_mut(
            "set_audio_gain",
            |_, this, (track, raw): (LuaUserDataRef<LuaTrack>, LuaValue)| {
                let gain = vivid_sdk::AudioGain::new(arg::<u64>(raw, "raw")?)
                    .ok_or_else(|| invalid("gain must be within 0..=2 * 2^32"))?;
                this.get_mut()?.set_audio_gain(&track.inner, gain).lua()
            },
        );
        methods.add_method_mut(
            "flush",
            |_, this, (track, epoch): (LuaUserDataRef<LuaTrack>, LuaValue)| {
                let epoch = arg::<u32>(epoch, "new_epoch")?;
                this.get_mut()?.flush(&track.inner, epoch).lua()
            },
        );
        methods.add_method_mut("drain", |_, this, track: LuaUserDataRef<LuaTrack>| {
            this.get_mut()?.drain(&track.inner).lua()
        });

        crate::input::add_session_methods(methods);
        crate::file_drop::add_session_methods(methods);
        crate::lease::add_session_methods(methods);
        crate::pipeline::add_session_methods(methods);
    }
}

pub fn session_info(lua: &Lua, session: &Session) -> LuaResult<LuaTable> {
    let info = session.info();
    let table = lua.create_table()?;
    table.set("session_id", info.session_id)?;
    table.set("session_tag", hex(&info.session_tag))?;
    table.set("root_context_id", info.root_context_id)?;
    table.set("target_generation", info.target_generation.get())?;
    table.set("target_profile", info.target_profile.as_str())?;
    table.set("accepted_profiles", info.accepted_profiles.clone())?;
    table.set("session_revision", info.session_revision)?;
    table.set("scene_revision", info.scene_revision.get())?;
    table.set("establishment_state", info.establishment_state)?;
    table.set("resume_generation", info.resume_generation)?;
    Ok(table)
}

// ---------------------------------------------------------------------------
// Surface and Track handles
// ---------------------------------------------------------------------------

pub struct LuaSurface {
    pub inner: Surface,
}

impl LuaUserData for LuaSurface {
    fn add_fields<F: LuaUserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("context_id", |_, this| Ok(this.inner.context_id()));
        fields.add_field_method_get("id", |_, this| Ok(this.inner.id()));
        fields.add_field_method_get("revision", |_, this| Ok(this.inner.revision().get()));
        fields.add_field_method_get("generation", |_, this| Ok(this.inner.generation().get()));
    }

    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(LuaMetaMethod::ToString, |_, this, ()| {
            Ok(format!(
                "vivid_sdk.Surface(context_id={}, id={}, revision={}, generation={})",
                this.inner.context_id(),
                this.inner.id(),
                this.inner.revision().get(),
                this.inner.generation().get()
            ))
        });
    }
}

pub struct LuaTrack {
    pub inner: Track,
}

impl LuaUserData for LuaTrack {
    fn add_fields<F: LuaUserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("context_id", |_, this| Ok(this.inner.context_id()));
        fields.add_field_method_get("surface_id", |_, this| Ok(this.inner.surface_id()));
        fields.add_field_method_get("id", |_, this| Ok(this.inner.id()));
        fields.add_field_method_get("kind", |_, this| Ok(kind_name(this.inner.kind())));
        fields.add_field_method_get("revision", |_, this| Ok(this.inner.revision().get()));
        fields.add_field_method_get("channel_generation", |_, this| {
            Ok(this.inner.channel_generation().get())
        });
    }

    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(LuaMetaMethod::ToString, |_, this, ()| {
            Ok(format!(
                "vivid_sdk.Track(context_id={}, surface_id={}, id={}, kind={:?}, generation={})",
                this.inner.context_id(),
                this.inner.surface_id(),
                this.inner.id(),
                kind_name(this.inner.kind()),
                this.inner.channel_generation().get()
            ))
        });
    }
}

pub fn kind_name(kind: TrackKind) -> &'static str {
    match kind {
        TrackKind::Video => "video",
        TrackKind::Audio => "audio",
        TrackKind::Raster => "raster",
        TrackKind::EncodedImage => "image",
        TrackKind::VectorScene => "vector",
    }
}

// ---------------------------------------------------------------------------
// Track channel
// ---------------------------------------------------------------------------

pub struct LuaTrackChannel {
    pub inner: Option<TrackChannel>,
    context_id: u64,
    surface_id: u64,
    track_id: u64,
    kind: TrackKind,
    generation: u64,
    /// The next raster frame ID. The SDK requires frame IDs to be nonzero and strictly increasing
    /// within a channel generation, so the channel keeps the sequence rather than every caller.
    next_frame_id: u64,
}

impl LuaTrackChannel {
    pub fn new(track: &Track, channel: TrackChannel) -> Self {
        Self {
            context_id: track.context_id(),
            surface_id: track.surface_id(),
            track_id: track.id(),
            kind: track.kind(),
            generation: channel.generation().get(),
            inner: Some(channel),
            next_frame_id: 1,
        }
    }

    pub fn get(&self) -> LuaResult<&TrackChannel> {
        self.inner.as_ref().ok_or_else(|| closed("track channel"))
    }

    /// The frame ID for this send: the caller's, which then continues the sequence, or the next.
    fn frame_id(&mut self, explicit: Option<u64>) -> LuaResult<u64> {
        let frame_id = explicit.unwrap_or(self.next_frame_id);
        self.next_frame_id = frame_id
            .checked_add(1)
            .ok_or_else(|| invalid("frame_id is out of range"))?;
        Ok(frame_id)
    }
}

const RASTER_KEYS: &[&str] = &["epoch", "frame_id", "compress"];
const DELTA_KEYS: &[&str] = &[
    "epoch",
    "frame_id",
    "base_frame_id",
    "pts_us",
    "duration_us",
    "compress",
];

impl LuaUserData for LuaTrackChannel {
    fn add_fields<F: LuaUserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("context_id", |_, this| Ok(this.context_id));
        fields.add_field_method_get("surface_id", |_, this| Ok(this.surface_id));
        fields.add_field_method_get("track_id", |_, this| Ok(this.track_id));
        fields.add_field_method_get("kind", |_, this| Ok(kind_name(this.kind)));
        fields.add_field_method_get("generation", |_, this| Ok(this.generation));
        fields.add_field_method_get("closed", |_, this| Ok(this.inner.is_none()));
    }

    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(LuaMetaMethod::ToString, |_, this, ()| {
            Ok(format!(
                "vivid_sdk.TrackChannel(track_id={}, kind={:?}, generation={}, closed={})",
                this.track_id,
                kind_name(this.kind),
                this.generation,
                this.inner.is_none()
            ))
        });
        #[cfg(any(feature = "lua54", feature = "lua55"))]
        methods.add_meta_method_mut(LuaMetaMethod::Close, |_, this, _: LuaMultiValue| match this
            .inner
            .take()
        {
            Some(channel) => channel.close().lua(),
            None => Ok(()),
        });

        methods.add_method_mut(
            "send_raster",
            |lua, this, (rgba, value): (LuaString, LuaValue)| {
                let config = options(lua, value, "raster options")?;
                check_keys(&config, RASTER_KEYS, "raster option")?;
                let epoch = get::<u32>(&config, "epoch")?.unwrap_or(0);
                let compress = get::<bool>(&config, "compress")?.unwrap_or(false);
                let frame_id = this.frame_id(get(&config, "frame_id")?)?;
                this.get()?
                    .send_raster(epoch, frame_id, &rgba.as_bytes(), compress)
                    .lua()
            },
        );
        methods.add_method_mut(
            "send_raster_adaptive",
            |lua, this, (rgba, value): (LuaString, LuaValue)| {
                let config = options(lua, value, "raster options")?;
                check_keys(&config, &["epoch", "frame_id"], "raster option")?;
                let epoch = get::<u32>(&config, "epoch")?.unwrap_or(0);
                let frame_id = this.frame_id(get(&config, "frame_id")?)?;
                this.get()?
                    .send_raster_adaptive(epoch, frame_id, &rgba.as_bytes())
                    .lua()
            },
        );
        methods.add_method_mut(
            "send_raster_delta",
            |_, this, (operations, value): (LuaValue, LuaValue)| {
                send_delta(this, operations, value, false)
            },
        );
        methods.add_method_mut(
            "send_raster_delta_adaptive",
            |_, this, (operations, value): (LuaValue, LuaValue)| {
                send_delta(this, operations, value, true)
            },
        );
        methods.add_method("send_image", |_, this, encoded: LuaString| {
            this.get()?.send_image(&encoded.as_bytes()).lua()
        });
        methods.add_method(
            "send_video",
            |_, this, (data, value): (LuaString, LuaValue)| {
                let config = arg::<LuaTable>(value, "video packet")?;
                check_keys(
                    &config,
                    &[
                        "packet_id",
                        "pts_us",
                        "dts_us",
                        "duration_us",
                        "key",
                        "epoch",
                    ],
                    "video packet",
                )?;
                let pts_us = need::<i64>(&config, "pts_us")?;
                let data = data.as_bytes();
                this.get()?
                    .send_video(VideoPacket {
                        epoch: get(&config, "epoch")?.unwrap_or(0),
                        packet_id: need(&config, "packet_id")?,
                        pts_us,
                        dts_us: get(&config, "dts_us")?.unwrap_or(pts_us),
                        duration_us: get(&config, "duration_us")?.unwrap_or(0),
                        key: get(&config, "key")?.unwrap_or(false),
                        data: &data,
                    })
                    .lua()
            },
        );
        methods.add_method(
            "send_audio",
            |_, this, (data, value): (LuaString, LuaValue)| {
                let config = arg::<LuaTable>(value, "audio packet")?;
                check_keys(
                    &config,
                    &[
                        "packet_id",
                        "pts_us",
                        "dts_us",
                        "duration_us",
                        "epoch",
                        "trim_start_samples",
                        "trim_end_samples",
                    ],
                    "audio packet",
                )?;
                let pts_us = need::<i64>(&config, "pts_us")?;
                let data = data.as_bytes();
                this.get()?
                    .send_audio(AudioPacket {
                        epoch: get(&config, "epoch")?.unwrap_or(0),
                        packet_id: need(&config, "packet_id")?,
                        pts_us,
                        dts_us: get(&config, "dts_us")?.unwrap_or(pts_us),
                        duration_us: need(&config, "duration_us")?,
                        trim_start_samples: get(&config, "trim_start_samples")?.unwrap_or(0),
                        trim_end_samples: get(&config, "trim_end_samples")?.unwrap_or(0),
                        data: &data,
                    })
                    .lua()
            },
        );
        methods.add_method("eos", |_, this, ()| this.get()?.eos().lua());
        methods.add_method_mut("close", |_, this, ()| {
            this.inner
                .take()
                .ok_or_else(|| closed("track channel"))?
                .close()
                .lua()
        });

        methods.add_method("take_event", |lua, this, ()| {
            match this.get()?.take_event().lua()? {
                Some(event) => channel_event(lua, event).map(LuaValue::Table),
                None => Ok(LuaValue::Nil),
            }
        });
        methods.add_method("wait_event", |lua, this, value: LuaValue| {
            let timeout = timeout(value, MAX_WAIT, "timeout")?;
            match this.get()?.wait_event(timeout).lua()? {
                Some(event) => channel_event(lua, event).map(LuaValue::Table),
                None => Ok(LuaValue::Nil),
            }
        });
        methods.add_function("events", |lua, (ud, value): (LuaAnyUserData, LuaValue)| {
            let timeout = timeout(value, MAX_WAIT, "timeout")?;
            ud.borrow::<LuaTrackChannel>()?;
            lua.create_function(move |lua, _: LuaMultiValue| {
                let channel = ud.borrow::<LuaTrackChannel>()?;
                let Some(channel) = channel.inner.as_ref() else {
                    return Ok(LuaValue::Nil);
                };
                match channel.wait_event(timeout).lua()? {
                    Some(event) => channel_event(lua, event).map(LuaValue::Table),
                    None => Ok(LuaValue::Nil),
                }
            })
        });
        methods.add_method("take_send_pressure", |lua, this, ()| {
            send_pressure(lua, this.get()?.take_send_pressure())
        });
        methods.add_method("media_credit_available", |_, this, length: LuaValue| {
            Ok(this
                .get()?
                .media_credit_available(arg::<u32>(length, "body_length")?))
        });

        crate::pipeline::add_channel_methods(methods);
    }
}

pub fn send_pressure(lua: &Lua, pressure: SendPressure) -> LuaResult<LuaTable> {
    let table = lua.create_table()?;
    table.set("rate_limited_us", micros(pressure.rate_limited))?;
    table.set("flow_limited_us", micros(pressure.flow_limited))?;
    table.set("transport_us", micros(pressure.transport))?;
    table.set("records", pressure.records)?;
    Ok(table)
}

/// One raster delta operation, owned for the duration of one send.
enum DeltaSpec {
    Overwrite {
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    },
    Copy {
        destination_x: u32,
        destination_y: u32,
        width: u32,
        height: u32,
        source_x: u32,
        source_y: u32,
    },
}

fn delta_spec(table: &LuaTable, name: &str) -> LuaResult<DeltaSpec> {
    match get::<String>(table, "op")?.as_deref() {
        Some("overwrite") => {
            check_keys(table, &["op", "x", "y", "width", "height", "rgba"], name)?;
            Ok(DeltaSpec::Overwrite {
                x: need(table, "x")?,
                y: need(table, "y")?,
                width: need(table, "width")?,
                height: need(table, "height")?,
                rgba: need::<Bytes>(table, "rgba")?.0,
            })
        }
        Some("copy") => {
            check_keys(
                table,
                &[
                    "op",
                    "destination_x",
                    "destination_y",
                    "width",
                    "height",
                    "source_x",
                    "source_y",
                ],
                name,
            )?;
            Ok(DeltaSpec::Copy {
                destination_x: need(table, "destination_x")?,
                destination_y: need(table, "destination_y")?,
                width: need(table, "width")?,
                height: need(table, "height")?,
                source_x: need(table, "source_x")?,
                source_y: need(table, "source_y")?,
            })
        }
        _ => Err(invalid(
            "a delta operation needs op = \"overwrite\" or \"copy\"",
        )),
    }
}

fn send_delta(
    this: &mut LuaTrackChannel,
    operations: LuaValue,
    value: LuaValue,
    adaptive: bool,
) -> LuaResult<u64> {
    let list = arg::<LuaTable>(operations, "operations")?;
    let specs = sequence(&list, "operations")?
        .into_iter()
        .enumerate()
        .map(|(index, item)| {
            let name = format!("delta operation {}", index + 1);
            delta_spec(&LuaTable::from_value(item, &name)?, &name)
        })
        .collect::<LuaResult<Vec<_>>>()?;
    let config = arg::<LuaTable>(value, "delta options")?;
    check_keys(&config, DELTA_KEYS, "delta option")?;
    let epoch = get::<u32>(&config, "epoch")?.unwrap_or(0);
    let base_frame_id = need::<u64>(&config, "base_frame_id")?;
    let pts_us = get::<i64>(&config, "pts_us")?.unwrap_or(0);
    let duration_us = get::<u64>(&config, "duration_us")?.unwrap_or(0);
    let compress = get::<bool>(&config, "compress")?.unwrap_or(false);
    if adaptive && config.contains_key("compress")? {
        return Err(invalid(
            "an adaptive delta chooses compression itself; compress is not an option",
        ));
    }
    let frame_id = this.frame_id(get(&config, "frame_id")?)?;
    let operations = specs
        .iter()
        .map(|spec| match spec {
            DeltaSpec::Overwrite {
                x,
                y,
                width,
                height,
                rgba,
            } => RasterDeltaOperation::Overwrite {
                x: *x,
                y: *y,
                width: *width,
                height: *height,
                rgba,
            },
            DeltaSpec::Copy {
                destination_x,
                destination_y,
                width,
                height,
                source_x,
                source_y,
            } => RasterDeltaOperation::Copy {
                destination_x: *destination_x,
                destination_y: *destination_y,
                width: *width,
                height: *height,
                source_x: *source_x,
                source_y: *source_y,
            },
        })
        .collect::<Vec<_>>();
    let channel = this.get()?;
    if adaptive {
        channel
            .send_raster_delta_adaptive(
                epoch,
                frame_id,
                base_frame_id,
                pts_us,
                duration_us,
                &operations,
            )
            .lua()
    } else {
        channel
            .send_raster_delta(
                epoch,
                frame_id,
                base_frame_id,
                pts_us,
                duration_us,
                &operations,
                compress,
            )
            .lua()
    }
}

// ---------------------------------------------------------------------------
// Configuration: surfaces, tracks, scene nodes, slot bindings
// ---------------------------------------------------------------------------

const SURFACE_KEYS: &[&str] = &[
    "logical_width",
    "logical_height",
    "semantic_profile",
    "coordinate_model",
    "role",
    "title",
    "semantic_content_revision",
    "semantic_availability",
    "locator_hint",
    "policy",
    "scale_numerator",
    "scale_denominator",
    "rotation",
    "context_id",
    "surface_id",
    "desktop_parameters",
];

/// A surface definition through `vivid_sdk::SurfaceBuilder`, which owns the identity and geometry
/// defaults. `identity` pins the context and surface of an existing surface for an update.
pub fn surface_definition(
    session: &Session,
    config: &LuaTable,
    identity: Option<(u64, u64)>,
) -> LuaResult<SurfaceDefinition> {
    check_keys(config, SURFACE_KEYS, "surface configuration")?;
    let mut builder = vivid_sdk::SurfaceBuilder::new(
        session,
        need(config, "logical_width")?,
        need(config, "logical_height")?,
    )
    .lua()?;
    if let Some(context_id) = get(config, "context_id")? {
        builder = builder.context(context_id);
    }
    if let Some(surface_id) = get(config, "surface_id")? {
        builder = builder.surface_id(surface_id);
    }
    if let Some((context_id, surface_id)) = identity {
        builder = builder.context(context_id).surface_id(surface_id);
    }
    let coordinate_model = CoordinateModel::try_from(
        get::<u64>(config, "coordinate_model")?
            .unwrap_or(CoordinateModel::DesktopLogicalPixels as u64),
    )
    .map_err(|error| invalid(error.to_string()))?;
    let semantic_profile = get::<String>(config, "semantic_profile")?
        .unwrap_or_else(|| vivid_sdk::GENERIC_CONTENT.into());
    let role = SurfaceRole::try_from(get::<u64>(config, "role")?.unwrap_or(0))
        .map_err(|error| invalid(error.to_string()))?;
    builder = builder
        .semantic(&semantic_profile, coordinate_model)
        .descriptor(SurfaceDescriptor {
            role,
            title: get(config, "title")?.unwrap_or_default(),
            semantic_content_revision: get(config, "semantic_content_revision")?.unwrap_or(0),
            semantic_availability: get(config, "semantic_availability")?.unwrap_or(0),
            locator_hint: get(config, "locator_hint")?.unwrap_or_default(),
        })
        .scale(
            get(config, "scale_numerator")?.unwrap_or(1),
            get(config, "scale_denominator")?.unwrap_or(1),
            get(config, "rotation")?.unwrap_or(0),
        );
    if let Some(policy) = get(config, "policy")? {
        builder = builder.policy(policy);
    }
    if let Some(parameters) = get::<LuaTable>(config, "desktop_parameters")? {
        builder = builder.desktop(&desktop_parameters(&parameters)?);
    }
    builder.build().lua()
}

fn desktop_parameters(config: &LuaTable) -> LuaResult<vivid_sdk::DesktopSurfaceParameters> {
    check_keys(
        config,
        &[
            "captured_origin_x",
            "captured_origin_y",
            "topology",
            "semantic_generation",
            "input_capabilities",
        ],
        "desktop parameter",
    )?;
    let topology = sequence(&need::<LuaTable>(config, "topology")?, "topology")?
        .into_iter()
        .map(|item| {
            let output = LuaTable::from_value(item, "topology entry")?;
            check_keys(
                &output,
                &[
                    "output_id",
                    "origin_x",
                    "origin_y",
                    "width",
                    "height",
                    "scale_numerator",
                    "scale_denominator",
                    "rotation",
                    "primary",
                ],
                "output",
            )?;
            Ok(OutputDescriptor {
                output_id: need(&output, "output_id")?,
                origin_x: need(&output, "origin_x")?,
                origin_y: need(&output, "origin_y")?,
                width: need(&output, "width")?,
                height: need(&output, "height")?,
                scale_numerator: get(&output, "scale_numerator")?.unwrap_or(1),
                scale_denominator: get(&output, "scale_denominator")?.unwrap_or(1),
                rotation: Rotation::try_from(get::<u64>(&output, "rotation")?.unwrap_or(0))
                    .map_err(|error| invalid(error.to_string()))?,
                primary: get(&output, "primary")?.unwrap_or(false),
            })
        })
        .collect::<LuaResult<Vec<_>>>()?;
    Ok(vivid_sdk::DesktopSurfaceParameters {
        captured_origin_x: need(config, "captured_origin_x")?,
        captured_origin_y: need(config, "captured_origin_y")?,
        topology,
        semantic_generation: need(config, "semantic_generation")?,
        input_capabilities: get(config, "input_capabilities")?.unwrap_or(0),
    })
}

fn surface_definition_table(lua: &Lua, definition: &SurfaceDefinition) -> LuaResult<LuaTable> {
    let table = lua.create_table()?;
    table.set("context_id", definition.context_id)?;
    table.set("surface_id", definition.surface_id)?;
    table.set("semantic_profile", definition.semantic_profile.as_str())?;
    table.set("coordinate_model", definition.coordinate_model as u64)?;
    table.set("logical_width", definition.logical_width)?;
    table.set("logical_height", definition.logical_height)?;
    table.set("scale_numerator", definition.scale_numerator)?;
    table.set("scale_denominator", definition.scale_denominator)?;
    table.set("rotation", definition.rotation)?;
    table.set("role", definition.descriptor.role as u64)?;
    table.set("title", definition.descriptor.title.as_str())?;
    table.set(
        "semantic_content_revision",
        definition.descriptor.semantic_content_revision,
    )?;
    table.set(
        "semantic_availability",
        definition.descriptor.semantic_availability,
    )?;
    table.set("locator_hint", definition.descriptor.locator_hint.as_str())?;
    table.set("policy", definition.policy)?;
    Ok(table)
}

const TRACK_COMMON_KEYS: &[&str] = &[
    "kind",
    "slot",
    "mode",
    "lane",
    "track_id",
    "maximum_rate_millihertz",
    "maximum_encoded_bits_per_second",
];

fn track_keys(kind: &str) -> &'static [&'static str] {
    match kind {
        "raster" => &[
            "width",
            "height",
            "alpha_mode",
            "delta_enabled",
            "maximum_delta_operations",
            "zstd_enabled",
        ],
        "image" => &["encoded", "sha256", "cache_lookup"],
        "video" => &[
            "codec",
            "width",
            "height",
            "packetization",
            "maximum_access_unit_bytes",
            "extradata",
            "profile",
            "level",
            "maximum_reorder_depth",
            "color_primaries",
            "transfer",
            "matrix",
            "signal_range",
            "aspect_numerator",
            "aspect_denominator",
            "codec_string",
            "decoder_configuration",
        ],
        "audio" => &[
            "sample_rate",
            "channels",
            "codec",
            "packetization",
            "maximum_access_unit_bytes",
            "extradata",
            "channel_mask",
            "codec_string",
            "uplink",
        ],
        _ => &[],
    }
}

/// A track configuration through `vivid_sdk::TrackBuilder`.
///
/// The claims the protocol bounds — record body, in-flight bytes, retained pixels, decoded pixels,
/// and the rate defaults that follow from them — are computed in Rust with checked arithmetic.
/// This states only what the caller asked for, so an unset claim keeps the builder's default for
/// that kind rather than a number written here.
pub fn track_configuration(
    session: &Session,
    context_id: u64,
    surface_id: u64,
    config: &LuaTable,
) -> LuaResult<TrackConfiguration> {
    let kind = need::<String>(config, "kind")?;
    let mut allowed = TRACK_COMMON_KEYS.to_vec();
    let specific = track_keys(&kind);
    if specific.is_empty() {
        return Err(invalid(format!(
            "track kind must be video, audio, raster, or image, not {kind:?}"
        )));
    }
    allowed.extend_from_slice(specific);
    check_keys(config, &allowed, &format!("{kind} track"))?;

    let slot = get::<u64>(config, "slot")?.unwrap_or(match kind.as_str() {
        "video" => vivid_sdk::SLOT_PRIMARY_VIDEO,
        "audio" => vivid_sdk::SLOT_AUDIO,
        "image" => vivid_sdk::SLOT_POSTER,
        _ => vivid_sdk::SLOT_RASTER,
    });
    let mode = match get::<u64>(config, "mode")? {
        None => TrackMode::Live,
        Some(value) => TrackMode::try_from(value).map_err(|error| invalid(error.to_string()))?,
    };
    let lane = match get::<u64>(config, "lane")? {
        None if kind == "audio" => LaneClass::Realtime,
        None => LaneClass::Bulk,
        Some(value) => LaneClass::try_from(value).map_err(|error| invalid(error.to_string()))?,
    };

    let contract = session.info().resource_contract.clone();
    let mut builder = vivid_sdk::TrackBuilder::detached(context_id, surface_id, slot, mode, lane);
    builder = match kind.as_str() {
        "video" => builder.video(
            need(config, "width")?,
            need(config, "height")?,
            &need::<String>(config, "codec")?,
        ),
        "audio" => builder.audio(need(config, "sample_rate")?, need(config, "channels")?),
        "raster" => builder
            .raster(need(config, "width")?, need(config, "height")?)
            .lua()?,
        _ => {
            let encoded = need::<Bytes>(config, "encoded")?.0;
            let mut image = vivid_sdk::probe_encoded_image(&encoded).lua()?;
            image.sha256 = get::<Bytes>(config, "sha256")?
                .map(|digest| {
                    <[u8; 32]>::try_from(digest.0)
                        .map_err(|_| invalid("sha256 must contain 32 bytes"))
                })
                .transpose()?;
            image.cache_lookup = get(config, "cache_lookup")?.unwrap_or(false);
            builder.image(image).lua()?
        }
    };
    if get::<bool>(config, "uplink")?.unwrap_or(false) {
        builder = builder.uplink();
    }
    if let Some(value) = get(config, "maximum_rate_millihertz")? {
        builder = builder.max_rate_millihertz(value);
    }
    if let Some(value) = get(config, "maximum_encoded_bits_per_second")? {
        builder = builder.max_encoded_bps(value);
    }
    let track_id = match get(config, "track_id")? {
        Some(track_id) => track_id,
        None => session.allocate_id().lua()?,
    };
    let mut configuration = builder.build(&contract, track_id).lua()?;

    // The builder owns the claims; these are the codec details a caller may state explicitly.
    match &mut configuration.kind {
        KindConfiguration::VectorScene(_) => {}
        KindConfiguration::Video(video) => {
            if let Some(value) = get(config, "packetization")? {
                video.packetization = value;
            }
            if let Some(value) = get::<Bytes>(config, "extradata")? {
                video.extradata = value.0;
            }
            if let Some(value) = get(config, "profile")? {
                video.profile = value;
            }
            if let Some(value) = get(config, "level")? {
                video.level = value;
            }
            if let Some(value) = get(config, "maximum_reorder_depth")? {
                video.maximum_reorder_depth = value;
            }
            if let Some(value) = get(config, "color_primaries")? {
                video.color_primaries = value;
            }
            if let Some(value) = get(config, "transfer")? {
                video.transfer = value;
            }
            if let Some(value) = get(config, "matrix")? {
                video.matrix = value;
            }
            if let Some(value) = get(config, "signal_range")? {
                video.signal_range = value;
            }
            if let Some(value) = get(config, "aspect_numerator")? {
                video.aspect_numerator = value;
            }
            if let Some(value) = get(config, "aspect_denominator")? {
                video.aspect_denominator = value;
            }
            if let Some(value) = get(config, "maximum_access_unit_bytes")? {
                video.maximum_access_unit_bytes = value;
            }
            if let Some(value) = get(config, "codec_string")? {
                video.codec_string = Some(value);
            }
            if let Some(value) = get::<Bytes>(config, "decoder_configuration")? {
                video.decoder_configuration = Some(value.0);
            }
        }
        KindConfiguration::Audio(audio) => {
            if let Some(value) = get(config, "codec")? {
                audio.codec = value;
            }
            if let Some(value) = get(config, "packetization")? {
                audio.packetization = value;
            }
            if let Some(value) = get::<Bytes>(config, "extradata")? {
                audio.extradata = value.0;
            }
            if let Some(value) = get(config, "channel_mask")? {
                audio.channel_mask = value;
            }
            if let Some(value) = get(config, "maximum_access_unit_bytes")? {
                audio.maximum_access_unit_bytes = value;
            }
            if let Some(value) = get(config, "codec_string")? {
                audio.codec_string = Some(value);
            }
        }
        KindConfiguration::Raster(raster) => {
            if let Some(value) = get(config, "alpha_mode")? {
                raster.alpha_mode = value;
            }
            if let Some(value) = get(config, "delta_enabled")? {
                raster.delta_enabled = value;
            }
            if let Some(value) = get(config, "maximum_delta_operations")? {
                raster.maximum_delta_operations = value;
            }
            if let Some(value) = get(config, "zstd_enabled")? {
                raster.zstd_enabled = value;
            }
        }
        KindConfiguration::EncodedImage(_) => {}
    }
    Ok(configuration)
}

fn track_configuration_table(lua: &Lua, configuration: &TrackConfiguration) -> LuaResult<LuaTable> {
    let table = lua.create_table()?;
    table.set("context_id", configuration.context_id)?;
    table.set("surface_id", configuration.surface_id)?;
    table.set("track_id", configuration.track_id)?;
    table.set("slot", configuration.slot)?;
    table.set("mode", configuration.mode as u64)?;
    table.set("lane", configuration.lane as u64)?;
    table.set("direction", configuration.direction as u64)?;
    table.set("maximum_record_body", configuration.maximum_record_body)?;
    table.set(
        "maximum_rate_millihertz",
        configuration.maximum_rate_millihertz,
    )?;
    table.set(
        "maximum_encoded_bits_per_second",
        configuration.maximum_encoded_bits_per_second,
    )?;
    table.set(
        "maximum_records_per_second",
        configuration.maximum_records_per_second,
    )?;
    table.set(
        "maximum_inflight_body_bytes",
        configuration.maximum_inflight_body_bytes,
    )?;
    table.set("target_latency_us", configuration.target_latency_us)?;
    table.set("maximum_latency_us", configuration.maximum_latency_us)?;
    table.set("retained_pixel_charge", configuration.retained_pixel_charge)?;
    match &configuration.kind {
        KindConfiguration::VectorScene(vector) => {
            table.set("kind", "vector")?;
            table.set("width", vector.width)?;
            table.set("height", vector.height)?;
            table.set("maximum_scene_bytes", vector.maximum_scene_bytes)?;
        }
        KindConfiguration::Video(video) => {
            table.set("kind", "video")?;
            table.set("codec", video.codec.as_str())?;
            table.set("packetization", video.packetization.as_str())?;
            table.set("extradata", lua.create_string(&video.extradata)?)?;
            table.set("width", video.coded_width)?;
            table.set("height", video.coded_height)?;
            table.set("profile", video.profile)?;
            table.set("level", video.level)?;
            table.set("maximum_reorder_depth", video.maximum_reorder_depth)?;
            table.set("color_primaries", video.color_primaries)?;
            table.set("transfer", video.transfer)?;
            table.set("matrix", video.matrix)?;
            table.set("signal_range", video.signal_range)?;
            table.set("aspect_numerator", video.aspect_numerator)?;
            table.set("aspect_denominator", video.aspect_denominator)?;
            table.set("maximum_access_unit_bytes", video.maximum_access_unit_bytes)?;
            table.set("codec_string", video.codec_string.clone())?;
            table.set(
                "decoder_configuration",
                video
                    .decoder_configuration
                    .as_ref()
                    .map(|bytes| lua.create_string(bytes))
                    .transpose()?,
            )?;
        }
        KindConfiguration::Audio(audio) => {
            table.set("kind", "audio")?;
            table.set("codec", audio.codec.as_str())?;
            table.set("packetization", audio.packetization.as_str())?;
            table.set("extradata", lua.create_string(&audio.extradata)?)?;
            table.set("sample_rate", audio.sample_rate)?;
            table.set("channels", audio.channels)?;
            table.set("channel_mask", audio.channel_mask)?;
            table.set("maximum_access_unit_bytes", audio.maximum_access_unit_bytes)?;
            table.set("codec_string", audio.codec_string.clone())?;
        }
        KindConfiguration::Raster(raster) => {
            table.set("kind", "raster")?;
            table.set("width", raster.width)?;
            table.set("height", raster.height)?;
            table.set("alpha_mode", raster.alpha_mode)?;
            table.set("delta_enabled", raster.delta_enabled)?;
            table.set("maximum_delta_operations", raster.maximum_delta_operations)?;
            table.set("zstd_enabled", raster.zstd_enabled)?;
        }
        KindConfiguration::EncodedImage(image) => {
            table.set("kind", "image")?;
            table.set("encoding", image.encoding)?;
            table.set("width", image.width)?;
            table.set("height", image.height)?;
            table.set("encoded_length", image.encoded_length)?;
            table.set(
                "sha256",
                image
                    .sha256
                    .as_ref()
                    .map(|digest| lua.create_string(digest))
                    .transpose()?,
            )?;
            table.set("cache_lookup", image.cache_lookup)?;
        }
    }
    Ok(table)
}

fn scene_node(session: &Session, surface: &Surface, config: &LuaTable) -> LuaResult<SceneNode> {
    check_keys(
        config,
        &[
            "node_id",
            "geometry",
            "fit",
            "linear_sampling",
            "z_index",
            "visible",
            "opacity",
        ],
        "scene node",
    )?;
    let node_id = match get(config, "node_id")? {
        Some(node_id) => node_id,
        None => session.allocate_id().lua()?,
    };
    let fit = match get::<u64>(config, "fit")? {
        None => Fit::Contain,
        Some(value) => Fit::try_from(value).map_err(|error| invalid(error.to_string()))?,
    };
    Ok(SceneNode {
        owning_context_id: surface.context_id(),
        node_id,
        surface_context_id: surface.context_id(),
        surface_id: surface.id(),
        geometry: match get::<LuaTable>(config, "geometry")? {
            Some(table) => geometry(&table)?,
            None => Vec::new(),
        },
        fit,
        linear_sampling: get(config, "linear_sampling")?.unwrap_or(true),
        z_index: get(config, "z_index")?.unwrap_or(0),
        visible: get(config, "visible")?.unwrap_or(true),
        // 0..=65535, so opaque is the full range rather than a byte's 255.
        opacity: get(config, "opacity")?.unwrap_or(u16::MAX),
        clip: None,
    })
}

fn slot_bindings(list: &LuaTable) -> LuaResult<Vec<SlotBinding>> {
    let bindings = sequence(list, "bindings")?
        .into_iter()
        .map(|item| {
            let binding = LuaTable::from_value(item, "slot binding")?;
            check_keys(
                &binding,
                &[
                    "slot",
                    "track_id",
                    "expected_channel_generation",
                    "required_milestone",
                ],
                "slot binding",
            )?;
            Ok(SlotBinding {
                slot: need(&binding, "slot")?,
                track_id: need(&binding, "track_id")?,
                expected_channel_generation: vivid_protocol::revision::ChannelGeneration::new(
                    need(&binding, "expected_channel_generation")?,
                ),
                required_milestone: get(&binding, "required_milestone")?
                    .unwrap_or(vivid_sdk::MILESTONE_OUTPUT_READY),
            })
        })
        .collect::<LuaResult<Vec<_>>>()?;
    if bindings.is_empty() {
        return Err(invalid("at least one slot binding is required"));
    }
    Ok(bindings)
}

/// Cells as the wire's signed 32.32 fixed point. Lua 5.1 and LuaJIT have no 64-bit shifts, so a
/// Lua caller passes ordinary cell numbers — `16`, or `1.5` — and the scaling happens here.
fn fixed(value: f64, name: &str) -> LuaResult<i64> {
    const ONE: f64 = 4_294_967_296.0;
    if !value.is_finite() {
        return Err(invalid(format!("{name} must be finite")));
    }
    let scaled = value * ONE;
    if scaled >= i64::MIN as f64 && scaled < -(i64::MIN as f64) {
        Ok(scaled as i64)
    } else {
        Err(invalid(format!(
            "{name} is outside the fixed-point cell range"
        )))
    }
}

fn scene_commit(lua: &Lua, commit: SceneCommit) -> LuaResult<LuaTable> {
    let table = lua.create_table()?;
    table.set("scene_revision", commit.scene_revision.get())?;
    table.set("target_generation", commit.target_generation.get())?;
    Ok(table)
}

// ---------------------------------------------------------------------------
// Events and status
// ---------------------------------------------------------------------------

pub fn playback_hold(lua: &Lua, hold: &vivid_sdk::PlaybackHold) -> LuaResult<LuaTable> {
    let table = lua.create_table()?;
    table.set("context_id", hold.context_id)?;
    table.set("surface_id", hold.surface_id)?;
    table.set("serial", hold.serial)?;
    table.set("held", hold.held)?;
    table.set("reasons", hold.reasons)?;
    table.set("playing_intent", hold.playing_intent)?;
    table.set("recovery_required", hold.recovery_required)?;
    if let Some(position) = &hold.position {
        let held = lua.create_table()?;
        held.set("track_id", position.track_id)?;
        held.set("channel_generation", position.channel_generation)?;
        held.set("epoch", position.epoch)?;
        held.set("pts_us", position.pts_us)?;
        held.set("estimated", position.estimated)?;
        table.set("position", held)?;
    }
    Ok(table)
}

pub fn session_event(lua: &Lua, event: SessionEvent) -> LuaResult<LuaTable> {
    let table = lua.create_table()?;
    match event {
        SessionEvent::PlaybackHold(hold) => {
            table.set("kind", "playback_hold")?;
            table.set("hold", playback_hold(lua, &hold)?)?;
            table.set("payload", payload_to_lua(lua, &hold.payload().lua()?)?)?;
        }
        SessionEvent::TargetChanged(payload) => {
            table.set("kind", "target_changed")?;
            table.set("payload", payload_to_lua(lua, &payload)?)?;
        }
        SessionEvent::AnchorReady {
            context_id,
            anchor_id,
            payload,
        } => {
            table.set("kind", "anchor_ready")?;
            table.set("context_id", context_id)?;
            table.set("anchor_id", anchor_id)?;
            table.set("payload", payload_to_lua(lua, &payload)?)?;
        }
        SessionEvent::AnchorGone {
            context_id,
            anchor_id,
            payload,
        } => {
            table.set("kind", "anchor_gone")?;
            table.set("context_id", context_id)?;
            table.set("anchor_id", anchor_id)?;
            table.set("payload", payload_to_lua(lua, &payload)?)?;
        }
        SessionEvent::TrackLost { object_id, payload } => {
            table.set("kind", "track_lost")?;
            table.set("object_id", object_id)?;
            table.set("payload", payload_to_lua(lua, &payload)?)?;
        }
        SessionEvent::ContextChanged { object_id, payload } => {
            table.set("kind", "context_changed")?;
            table.set("object_id", object_id)?;
            table.set("payload", payload_to_lua(lua, &payload)?)?;
        }
        SessionEvent::FileDropOffered(offer) => {
            table.set("kind", "file_drop_offered")?;
            table.set(
                "binding",
                crate::file_drop::drop_tuple(lua, &offer.binding)?,
            )?;
            table.set("suggested_name", offer.suggested_name)?;
            table.set("declared_length", offer.declared_length)?;
        }
        SessionEvent::FileDropCancelled(cancel) => {
            table.set("kind", "file_drop_cancelled")?;
            table.set(
                "binding",
                crate::file_drop::drop_tuple(lua, &cancel.binding)?,
            )?;
            table.set("drop_id", cancel.binding.drop_id)?;
            table.set("reason", cancel.reason)?;
        }
        SessionEvent::Other {
            record_type,
            object_id,
            payload,
        } => {
            table.set("kind", "other")?;
            table.set("record_type", record_type)?;
            table.set("object_id", object_id)?;
            table.set("payload", payload_to_lua(lua, &payload)?)?;
        }
        SessionEvent::ConnectionClosed { diagnostic } => {
            table.set("kind", "connection_closed")?;
            table.set("diagnostic", diagnostic)?;
        }
    }
    Ok(table)
}

fn channel_event(lua: &Lua, event: ChannelEvent) -> LuaResult<LuaTable> {
    let table = lua.create_table()?;
    match event {
        ChannelEvent::NeedKeyframe(payload) => {
            table.set("kind", "need_keyframe")?;
            table.set("payload", payload_to_lua(lua, &payload)?)?;
        }
        ChannelEvent::NeedFullFrame(payload) => {
            table.set("kind", "need_full_frame")?;
            table.set("payload", payload_to_lua(lua, &payload)?)?;
        }
        ChannelEvent::Error(error) => {
            table.set("kind", "error")?;
            table.set("code", error.code)?;
            table.set("message", error.to_string())?;
        }
    }
    Ok(table)
}

fn surface_status(lua: &Lua, status: &SurfaceStatus) -> LuaResult<LuaTable> {
    let table = lua.create_table()?;
    table.set("context_id", status.context_id)?;
    table.set("surface_id", status.surface_id)?;
    table.set("revision", status.revision.get())?;
    table.set("generation", status.generation.get())?;
    table.set("semantic_profile", status.semantic_profile.as_str())?;
    table.set("coordinate_model", status.coordinate_model as u64)?;
    table.set("logical_width", status.logical_width)?;
    table.set("logical_height", status.logical_height)?;
    table.set("scale_numerator", status.scale_numerator)?;
    table.set("scale_denominator", status.scale_denominator)?;
    table.set("rotation", status.rotation)?;
    table.set("role", status.descriptor.role as u64)?;
    table.set("title", status.descriptor.title.as_str())?;
    table.set(
        "semantic_content_revision",
        status.descriptor.semantic_content_revision,
    )?;
    table.set(
        "semantic_availability",
        status.descriptor.semantic_availability,
    )?;
    table.set("locator_hint", status.descriptor.locator_hint.as_str())?;
    table.set("effective_policy", status.effective_policy)?;
    table.set("active_slots", payload_to_lua(lua, &status.active_slots)?)?;
    table.set("lifecycle", status.lifecycle)?;
    table.set(
        "profile_status",
        payload_to_lua(lua, &status.profile_status)?,
    )?;
    Ok(table)
}

fn track_status(lua: &Lua, status: &TrackStatus) -> LuaResult<LuaTable> {
    let table = lua.create_table()?;
    if let Some(hold) = &status.playback_hold {
        table.set("playback_hold", playback_hold(lua, hold)?)?;
    }
    table.set("context_id", status.context_id)?;
    table.set("surface_id", status.surface_id)?;
    table.set("track_id", status.track_id)?;
    table.set("kind", kind_name(status.kind))?;
    table.set("mode", status.mode as u64)?;
    table.set("revision", status.revision.get())?;
    table.set("channel_generation", status.channel_generation.get())?;
    table.set("lifecycle", status.lifecycle)?;
    table.set("attachment_state", status.attachment_state)?;
    table.set("milestones", status.milestones)?;
    table.set("media_epoch", status.media_epoch)?;
    table.set("last_media_id", status.last_media_id)?;
    table.set(
        "last_media_record_sequence",
        status.last_media_record_sequence,
    )?;
    table.set("last_decoded_pts_us", status.last_decoded_pts_us)?;
    table.set("last_presented_pts_us", status.last_presented_pts_us)?;
    table.set("last_presentation_id", status.last_presentation_id)?;
    table.set("cumulative_body_bytes", status.cumulative_body_bytes)?;
    table.set("cumulative_media_records", status.cumulative_media_records)?;
    table.set("maximum_body_bytes", status.maximum_body_bytes)?;
    table.set("maximum_media_records", status.maximum_media_records)?;
    table.set("ingress_depth_bucket", status.ingress_depth_bucket)?;
    if let Some(playback) = &status.playback_state {
        table.set("playback_state", payload_to_lua(lua, playback)?)?;
    }
    table.set("terminal_loss_code", status.terminal_loss_code)?;
    if let Some(gain) = &status.audio_gain {
        let gain_table = lua.create_table()?;
        gain_table.set("raw", gain.raw())?;
        table.set("audio_gain", gain_table)?;
    }
    Ok(table)
}

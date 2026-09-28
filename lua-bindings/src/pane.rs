//! The shortest paths from a frame to a screen: `PaneSession` and `display_image`.

use std::io::IsTerminal;
use std::time::{Duration, Instant};

use mlua::prelude::*;
use sha2::{Digest, Sha256};
use vivid_protocol::messages::LaneClass;
use vivid_protocol::scene::{Fit, SceneNode};
use vivid_protocol::track::TrackMode;
use vivid_sdk::{
    PaneImageOptions, PaneSession, RequestMetadata, Session, SlotBinding, SurfaceRole,
    TrackWaitCondition,
};

use crate::convert::{arg, check_keys, get, options};
use crate::error::{IoResultExt, closed, vivid};
use crate::session::{
    LuaSession, LuaSurface, LuaTrack, LuaTrackChannel, MAX_WAIT, producer_config,
};

// ---------------------------------------------------------------------------
// PaneSession
// ---------------------------------------------------------------------------

/// One image in one terminal pane, over the SDK's own pane state machine. The node, surface, and
/// track lifecycle, the cell geometry, and the 80x24 defaults all live in `vivid_sdk::PaneSession`.
pub struct LuaPaneSession {
    inner: Option<PaneSession>,
}

impl LuaPaneSession {
    fn get_mut(&mut self) -> LuaResult<&mut PaneSession> {
        self.inner.as_mut().ok_or_else(|| closed("pane session"))
    }
}

/// `vivid.PaneSession.connect(options)`: connect with the same options as `vivid.connect`.
pub fn pane_connect(lua: &Lua, value: LuaValue) -> LuaResult<LuaPaneSession> {
    let config = producer_config(&options(lua, value, "connect options")?)?;
    let session = Session::connect(config).lua()?;
    Ok(LuaPaneSession {
        inner: Some(PaneSession::from_session(session).lua()?),
    })
}

/// `vivid.PaneSession.from_session(session)`: adopt an established session, which the pane owns.
pub fn pane_from_session(
    _: &Lua,
    session: LuaUserDataRefMut<LuaSession>,
) -> LuaResult<LuaPaneSession> {
    let mut session = session;
    let owned = session.take()?;
    Ok(LuaPaneSession {
        inner: Some(PaneSession::from_session(owned).lua()?),
    })
}

fn pane_options(lua: &Lua, value: LuaValue) -> LuaResult<PaneImageOptions> {
    let config = options(lua, value, "pane image options")?;
    check_keys(
        &config,
        &["title", "columns", "rows", "text_layer"],
        "pane image option",
    )?;
    // Unset entries are left to the SDK, which is how it reads "use the default".
    let defaults = PaneImageOptions::default();
    Ok(PaneImageOptions {
        title: get(&config, "title")?.unwrap_or(defaults.title),
        columns: get(&config, "columns")?.or(defaults.columns),
        rows: get(&config, "rows")?.or(defaults.rows),
        text_layer: get(&config, "text_layer")?.unwrap_or(defaults.text_layer),
    })
}

impl LuaUserData for LuaPaneSession {
    fn add_fields<F: LuaUserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("closed", |_, this| Ok(this.inner.is_none()));
        fields.add_field_method_get("has_presentation", |_, this| {
            Ok(this
                .inner
                .as_ref()
                .is_some_and(PaneSession::has_presentation))
        });
    }

    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        // Says whether a presentation exists and nothing about the endpoint or its capability.
        methods.add_meta_method(LuaMetaMethod::ToString, |_, this, ()| {
            Ok(match &this.inner {
                Some(pane) => format!(
                    "vivid_sdk.PaneSession(has_presentation={})",
                    pane.has_presentation()
                ),
                None => "vivid_sdk.PaneSession(closed)".into(),
            })
        });
        #[cfg(any(feature = "lua54", feature = "lua55"))]
        methods.add_meta_method_mut(LuaMetaMethod::Close, |_, this, _: LuaMultiValue| match this
            .inner
            .take()
        {
            Some(pane) => pane.close().lua(),
            None => Ok(()),
        });
        methods.add_method_mut(
            "show_encoded_image",
            |lua, this, (encoded, value): (LuaString, LuaValue)| {
                let options = pane_options(lua, value)?;
                this.get_mut()?
                    .show_encoded_image_with_options(&encoded.as_bytes(), &options)
                    .lua()
            },
        );
        methods.add_method_mut(
            "show_rgba",
            |lua, this, (width, height, rgba, value): (LuaValue, LuaValue, LuaString, LuaValue)| {
                let options = pane_options(lua, value)?;
                this.get_mut()?
                    .show_rgba_with_options(
                        arg(width, "width")?,
                        arg(height, "height")?,
                        &rgba.as_bytes(),
                        &options,
                    )
                    .lua()
            },
        );
        methods.add_method_mut("clear", |_, this, ()| this.get_mut()?.clear().lua());
        methods.add_method_mut("close", |_, this, ()| {
            this.inner
                .take()
                .ok_or_else(|| closed("pane session"))?
                .close()
                .lua()
        });
    }
}

// ---------------------------------------------------------------------------
// display_image
// ---------------------------------------------------------------------------

/// Live handles for a retained image presentation. Keep it alive for as long as the image should
/// stay; `close()` removes it, and `presentation.session:close()` instead ends the session cleanly
/// so the presenter may keep the anchored image as a poster after this process exits.
pub struct LuaImagePresentation {
    session: LuaAnyUserData,
    surface: LuaAnyUserData,
    track: LuaAnyUserData,
    channel: LuaAnyUserData,
}

impl LuaUserData for LuaImagePresentation {
    fn add_fields<F: LuaUserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("session", |_, this| Ok(this.session.clone()));
        fields.add_field_method_get("surface", |_, this| Ok(this.surface.clone()));
        fields.add_field_method_get("track", |_, this| Ok(this.track.clone()));
        fields.add_field_method_get("channel", |_, this| Ok(this.channel.clone()));
    }

    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(LuaMetaMethod::ToString, |_, _, ()| {
            Ok("vivid_sdk.ImagePresentation")
        });
        methods.add_method("close", |_, this, ()| {
            let mut channel = this.channel.borrow_mut::<LuaTrackChannel>()?;
            if let Some(channel) = channel.inner.take() {
                channel.close().lua()?;
            }
            let mut session = this.session.borrow_mut::<LuaSession>()?;
            if session.closed() {
                return Ok(());
            }
            let surface = this.surface.borrow::<LuaSurface>()?;
            let track = this.track.borrow::<LuaTrack>()?;
            let live = session.get_mut()?;
            live.destroy_track(&track.inner, &RequestMetadata::default())
                .lua()?;
            live.destroy_surface(&surface.inner, &RequestMetadata::default())
                .lua()?;
            session.take()?.close().lua()
        });
    }
}

const DISPLAY_KEYS: &[&str] = &["columns", "rows"];

/// `vivid.display_image(path, options)`: create, activate, and retain one PNG or JPEG.
///
/// Returns once the presenter has accepted the image into the surface's active slot. The image is
/// anchored to the cursor cell where it is written and the text cursor is moved past it; a target
/// with no text plane to anchor to falls back to the terminal grid. Options other than `columns`
/// and `rows` are connect options.
pub fn display_image(
    lua: &Lua,
    (path, value): (LuaValue, LuaValue),
) -> LuaResult<LuaImagePresentation> {
    let path = arg::<String>(path, "path")?;
    let config = options(lua, value, "display options")?;
    let connect = lua.create_table()?;
    for pair in config.pairs::<LuaValue, LuaValue>() {
        let (key, value) = pair?;
        let display = matches!(&key, LuaValue::String(name) if name.to_str().is_ok_and(|name| DISPLAY_KEYS.contains(&&*name)));
        if !display {
            connect.raw_set(key, value)?;
        }
    }
    let columns = get::<u32>(&config, "columns")?;
    let rows = get::<u32>(&config, "rows")?;

    // Fail on a missing file before connecting. The container is read once, so the surface
    // geometry, the track's declared dimensions, and the bytes sent all come from one parse.
    let encoded =
        std::fs::read(&path).map_err(|error| vivid(format!("cannot read {path}: {error}")))?;
    let mut image = vivid_sdk::probe_encoded_image(&encoded).lua()?;
    image.sha256 = Some(Sha256::digest(&encoded).into());
    let columns = columns.unwrap_or(image.width.min(80));
    let rows = rows.unwrap_or(image.height.min(24));
    let title = std::path::Path::new(&path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();

    let mut session = Session::connect(producer_config(&connect)?).lua()?;
    let presented = (|| -> LuaResult<_> {
        let definition = vivid_sdk::SurfaceBuilder::new(
            &session,
            u64::from(image.width),
            u64::from(image.height),
        )
        .lua()?
        .titled(SurfaceRole::Figure, title)
        .build()
        .lua()?;
        let surface = session
            .create_surface(definition, &RequestMetadata::default())
            .lua()?;
        place_image_node(lua, &mut session, &surface, columns, rows)?;
        let contract = session.info().resource_contract.clone();
        let configuration = vivid_sdk::TrackBuilder::new(
            &surface,
            vivid_sdk::SLOT_POSTER,
            TrackMode::Live,
            LaneClass::Bulk,
        )
        .image(image)
        .lua()?
        .build(&contract, session.allocate_id().lua()?)
        .lua()?;
        let track = session
            .create_track(configuration, &RequestMetadata::default())
            .lua()?;
        let channel = session.open_track_channel(&track).lua()?;
        channel.send_image(&encoded).lua()?;
        // Media and control are independent connections, so submitting the bytes does not make
        // the track ready: it must reach OUTPUT_READY before it can take the surface's slot.
        session
            .wait_track(
                &track,
                TrackWaitCondition::MilestoneSet,
                Some(vivid_sdk::MILESTONE_OUTPUT_READY),
                crate::convert::micros(MAX_WAIT),
            )
            .lua()?;
        let binding = SlotBinding {
            slot: vivid_sdk::SLOT_POSTER,
            track_id: track.id(),
            expected_channel_generation: track.channel_generation(),
            required_milestone: vivid_sdk::MILESTONE_OUTPUT_READY,
        };
        session
            .activate_tracks(&surface, &[binding], &RequestMetadata::default())
            .lua()?;
        Ok((surface, track, channel))
    })();
    let (surface, track, channel) = match presented {
        Ok(parts) => parts,
        Err(error) => {
            let _ = session.close();
            return Err(error);
        }
    };
    let channel = LuaTrackChannel::new(&track, channel);
    Ok(LuaImagePresentation {
        session: lua.create_userdata(LuaSession::new(session))?,
        surface: lua.create_userdata(LuaSurface { inner: surface })?,
        track: lua.create_userdata(LuaTrack { inner: track })?,
        channel: lua.create_userdata(channel)?,
    })
}

/// Write through Lua's own `io.stdout`, so the marker lands in order with whatever the script
/// printed before it rather than overtaking a buffered line.
fn write_stdout(lua: &Lua, text: &str) -> LuaResult<()> {
    let io: LuaTable = lua.globals().get("io")?;
    let stdout: LuaAnyUserData = io.get("stdout")?;
    stdout.call_method::<LuaValue>("write", text)?;
    stdout.call_method::<LuaValue>("flush", ())?;
    Ok(())
}

/// Create an authenticated anchor at the terminal's current cursor cell.
///
/// The zero-width marker is the one thing a producer may write to the PTY. Anchored nodes follow
/// scroll and reflow, and only they can become a retained poster after a clean `GOODBYE`. `None`
/// means there is no text plane to anchor to, and the caller positions against the grid instead.
fn anchor_at_cursor(lua: &Lua, session: &Session) -> Option<u64> {
    if session.info().target_profile != vivid_sdk::TERMINAL_SURFACE {
        return None;
    }
    let set = |name: &str| std::env::var_os(name).is_some_and(|value| !value.is_empty());
    if set("TMUX") || set("STY") || !std::io::stdout().is_terminal() {
        // A foreign multiplexer owns the text stream and drops the marker; a redirected stdout
        // is not a text plane at all.
        return None;
    }
    let context_id = session.info().root_context_id;
    // The anchor ID authenticates the marker, so it comes from the CSPRNG rather than from the
    // session's sequential IDs. It never leaves Rust, so it keeps all 64 bits on every Lua.
    let anchor_id = loop {
        let mut bytes = [0; 8];
        getrandom::fill(&mut bytes).ok()?;
        let value = u64::from_ne_bytes(bytes);
        if value != 0 {
            break value;
        }
    };
    let marker = if std::env::var("VIVID_ANCHOR_TRANSPORT").as_deref() == Ok("conpty") {
        session.conpty_anchor_marker(context_id, anchor_id)
    } else {
        session.anchor_marker(context_id, anchor_id)
    }
    .ok()?;
    write_stdout(lua, &marker).ok()?;
    // A node may only name an anchor the presenter has already created. Polling leaves the
    // session's event queue to its owner.
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if session.query_anchor(context_id, anchor_id).ok()?.state == 1 {
            return Some(anchor_id);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Place `surface` at the cursor, or against the grid when there is no anchor to use.
fn place_image_node(
    lua: &Lua,
    session: &mut Session,
    surface: &vivid_sdk::Surface,
    columns: u32,
    rows: u32,
) -> LuaResult<()> {
    let width = i64::from(columns) << 32;
    let height = i64::from(rows) << 32;
    let node_id = session.allocate_id().lua()?;
    let Some(anchor_id) = anchor_at_cursor(lua, session) else {
        session
            .place_terminal_surface(surface, node_id, 0, 0, width, height, 1)
            .lua()?;
        return Ok(());
    };
    use vivid_protocol::cbor::Value;
    let root = session.info().root_context_id;
    let node = SceneNode {
        owning_context_id: surface.context_id(),
        node_id,
        surface_context_id: surface.context_id(),
        surface_id: surface.id(),
        geometry: vec![
            // Anchor-cell space: (0, 0) is the anchor's own cell.
            (0, Value::Unsigned(2)),
            (1, Value::Unsigned(0)),
            (2, Value::Unsigned(0)),
            (3, Value::Unsigned(width as u64)),
            (4, Value::Unsigned(height as u64)),
            (5, Value::Unsigned(1)),
            (6, Value::Unsigned(root)),
            (7, Value::Unsigned(anchor_id)),
        ],
        fit: Fit::Contain,
        linear_sampling: true,
        z_index: 0,
        visible: true,
        opacity: u16::MAX,
        clip: None,
    };
    session
        .create_node(&node, &RequestMetadata::default())
        .lua()?;
    // Move the text cursor past the image so the prompt lands below it. Only this ordinary
    // whitespace crosses the PTY, and the anchor carries the image along when these lines scroll.
    write_stdout(lua, &"\n".repeat(rows as usize))
}

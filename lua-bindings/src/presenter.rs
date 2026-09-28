//! The terminating presenter: the other half of the module.
//!
//! A presenter binds an endpoint, issues a capability per pane, and holds the retained scene a
//! producer sends. Reading a pane is pull-based: there are no callbacks and no queue to drain, so a
//! slow reader cannot stall the presenter. The presenter's threads are pure Rust and never enter
//! Lua; nothing here takes a Lua callback, so a Lua error can never surface inside an accept loop.

use mlua::prelude::*;
use vivid_sdk::presenter::{
    Binding, BridgePositionSnapshot, CaptureContent, KeyframeRequestOutcome, MediaConfig,
    PresenterConfig, PresenterListener, SocketListener, SourceKey, VirtualVivid,
};
use vivid_sdk::{DesktopTarget, OutputDescriptor, Rotation};

use crate::convert::{Bytes, Field, arg, check_keys, get, need, opt, options, sequence, timeout};
use crate::error::{IoResultExt, closed, vivid};

/// `vivid.presenter.start(endpoint, options)`.
///
/// `endpoint` is spelled as producers spell it: `unix:/absolute/path` or `tcp:127.0.0.1:PORT`.
/// Port 0 binds an ephemeral port, which `presenter.endpoint` then reports; TCP is loopback only.
/// Terminal is the default target; `desktop = { width = w, height = h }` serves a desktop target.
pub fn start(lua: &Lua, (endpoint, value): (LuaValue, LuaValue)) -> LuaResult<LuaPresenter> {
    let endpoint = arg::<String>(endpoint, "endpoint")?;
    let config = options(lua, value, "presenter options")?;
    check_keys(&config, &["desktop", "retained_bytes"], "presenter option")?;
    let mut media = MediaConfig::default();
    if let Some(bytes) = get(&config, "retained_bytes")? {
        media.aggregate_retained_bytes = bytes;
    }
    let config = match get::<LuaTable>(&config, "desktop")? {
        Some(desktop) => {
            check_keys(&desktop, &["width", "height"], "desktop target")?;
            PresenterConfig::desktop(
                media,
                desktop_target(need(&desktop, "width")?, need(&desktop, "height")?),
            )
        }
        None => PresenterConfig::terminal(media),
    };
    let listener = SocketListener::bind(&endpoint).lua()?;
    let resolved = listener.endpoint();
    let presenter = VirtualVivid::start_configured(listener, config, None).lua()?;
    Ok(LuaPresenter {
        inner: Some(presenter),
        endpoint: resolved,
    })
}

fn desktop_target(width: u32, height: u32) -> DesktopTarget {
    DesktopTarget {
        origin_x: 0,
        origin_y: 0,
        width,
        height,
        settled: true,
        topology_revision: 1,
        outputs: vec![OutputDescriptor {
            output_id: 1,
            origin_x: 0,
            origin_y: 0,
            width,
            height,
            scale_numerator: 1,
            scale_denominator: 1,
            rotation: Rotation::None,
            primary: true,
        }],
    }
}

pub struct LuaPresenter {
    inner: Option<VirtualVivid>,
    endpoint: String,
}

impl LuaPresenter {
    fn get(&self) -> LuaResult<&VirtualVivid> {
        self.inner.as_ref().ok_or_else(|| closed("presenter"))
    }
}

/// The complete owner tuple of one track, as the presenter sees it.
fn source_key(lua: &Lua, source: SourceKey) -> LuaResult<LuaTable> {
    let table = lua.create_table()?;
    table.set("producer", source.producer)?;
    table.set("context", source.context)?;
    table.set("surface", source.surface)?;
    table.set("track", source.track)?;
    Ok(table)
}

fn source_from(value: LuaValue, name: &str) -> LuaResult<SourceKey> {
    let table = LuaTable::from_value(value, name)?;
    check_keys(
        &table,
        &["producer", "context", "surface", "track"],
        "source",
    )?;
    Ok(SourceKey {
        producer: need(&table, "producer")?,
        context: need(&table, "context")?,
        surface: need(&table, "surface")?,
        track: need(&table, "track")?,
    })
}

impl LuaUserData for LuaPresenter {
    fn add_fields<F: LuaUserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("closed", |_, this| Ok(this.inner.is_none()));
        // Addressing, not capability material; pane secrets are returned, never held here.
        fields.add_field_method_get("endpoint", |_, this| Ok(this.endpoint.clone()));
    }

    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(LuaMetaMethod::ToString, |_, this, ()| {
            Ok(format!(
                "vivid_sdk.Presenter(endpoint={:?}, closed={})",
                this.endpoint,
                this.inner.is_none()
            ))
        });
        #[cfg(any(feature = "lua54", feature = "lua55"))]
        methods.add_meta_method_mut(LuaMetaMethod::Close, |_, this, _: LuaMultiValue| {
            drop(this.inner.take());
            Ok(())
        });
        // Idempotent. Dropping signals shutdown and wakes the accept and delivery waiters.
        methods.add_method_mut("close", |_, this, ()| {
            drop(this.inner.take());
            Ok(())
        });
        // Capability material: hand it to exactly one producer, never through a command line.
        methods.add_method("issue_pane_capability", |_, this, pane: LuaValue| {
            this.get()?.issue_pane_capability(arg(pane, "pane")?).lua()
        });
        methods.add_method("revoke_pane", |_, this, pane: LuaValue| {
            this.get()?.revoke_pane(arg(pane, "pane")?);
            Ok(())
        });
        methods.add_method(
            "update_metrics",
            |_, this, (pane, metrics): (LuaValue, LuaValue)| {
                let pane = arg(pane, "pane")?;
                let metrics = arg::<LuaTable>(metrics, "metrics")?;
                check_keys(
                    &metrics,
                    &["columns", "rows", "cell_width", "cell_height"],
                    "pane metric",
                )?;
                this.get()?.update_metrics(
                    pane,
                    need(&metrics, "columns")?,
                    need(&metrics, "rows")?,
                    (
                        get(&metrics, "cell_width")?.unwrap_or(8),
                        get(&metrics, "cell_height")?.unwrap_or(16),
                    ),
                );
                Ok(())
            },
        );
        // Block until the pane holds something a capture could compose, or `timeout` seconds pass.
        methods.add_method(
            "wait_for_media",
            |_, this, (pane, wait): (LuaValue, LuaValue)| {
                let pane = arg(pane, "pane")?;
                let wait = timeout(wait, std::time::Duration::ZERO, "timeout")?;
                Ok(this.get()?.wait_for_retained_media(pane, wait))
            },
        );
        // Composes the producer's retained surfaces. Not a screenshot: terminal text belongs to a
        // renderer, and this presenter has none. A capture that produced nothing says why.
        methods.add_method(
            "capture_pane",
            |lua, this, (pane, offset): (LuaValue, LuaValue)| {
                let pane = arg(pane, "pane")?;
                let offset = opt::<usize>(offset, "viewport_offset")?.unwrap_or(0);
                let capture = this.get()?.capture_pane(pane, offset);
                let layers = lua.create_table()?;
                for layer in &capture.layers {
                    let entry = lua.create_table()?;
                    entry.set("source", source_key(lua, layer.source)?)?;
                    entry.set("node_id", layer.node_id)?;
                    entry.set("z_index", layer.z_index)?;
                    entry.set("x", layer.x)?;
                    entry.set("y", layer.y)?;
                    entry.set("width", layer.width)?;
                    entry.set("height", layer.height)?;
                    if let Some(clip) = layer.clip {
                        let rect = lua.create_table()?;
                        rect.set("x", clip.x)?;
                        rect.set("y", clip.y)?;
                        rect.set("width", clip.width)?;
                        rect.set("height", clip.height)?;
                        entry.set("clip", rect)?;
                    }
                    let content = lua.create_table()?;
                    match &layer.content {
                        CaptureContent::Raster(raster) => {
                            content.set("kind", "raster")?;
                            content.set("epoch", raster.epoch)?;
                            content.set("frame_id", raster.frame_id)?;
                            content.set("width", raster.width)?;
                            content.set("height", raster.height)?;
                            // A copy: the retained buffer is mutated by the presenter's threads.
                            content.set("rgba", lua.create_string(&raster.pixels)?)?;
                        }
                        CaptureContent::EncodedImage(bytes) => {
                            content.set("kind", "encoded_image")?;
                            content.set("data", lua.create_string(bytes)?)?;
                        }
                    }
                    entry.set("content", content)?;
                    layers.push(entry)?;
                }
                let skipped = lua.create_table()?;
                for skip in &capture.skipped {
                    let entry = lua.create_table()?;
                    entry.set("source", source_key(lua, skip.source)?)?;
                    entry.set("node_id", skip.node_id)?;
                    entry.set("reason", skip.reason.as_str())?;
                    skipped.push(entry)?;
                }
                let result = lua.create_table()?;
                result.set("layers", layers)?;
                result.set("skipped", skipped)?;
                Ok(result)
            },
        );
        methods.add_method("pane_media_summary", |lua, this, pane: LuaValue| {
            let summary = this.get()?.pane_media_summary(arg(pane, "pane")?);
            let tracks = lua.create_table()?;
            for track in &summary.tracks {
                let entry = lua.create_table()?;
                entry.set("source", source_key(lua, track.source)?)?;
                entry.set("kind", track.kind)?;
                entry.set("capturable", track.capturable)?;
                tracks.push(entry)?;
            }
            let result = lua.create_table()?;
            result.set("surfaces", summary.surfaces)?;
            result.set("tracks", tracks)?;
            Ok(result)
        });

        // -- Microphones, targets, anchors -------------------------------------
        methods.add_method(
            "queue_microphone",
            |_, this, (source, generation, pcm): (LuaValue, LuaValue, LuaValue)| {
                let source = source_from(source, "source")?;
                let generation = arg(generation, "generation")?;
                let pcm = arg::<Bytes>(pcm, "pcm")?.0;
                this.get()?.queue_microphone(source, generation, &pcm).lua()
            },
        );
        methods.add_method("revoke_microphones", |_, this, ()| {
            this.get()?.revoke_microphones();
            Ok(())
        });
        methods.add_method("notify_capabilities_changed", |_, this, mask: LuaValue| {
            this.get()?
                .notify_capabilities_changed(arg(mask, "reason_mask")?)
                .lua()
        });
        methods.add_method(
            "update_desktop_target",
            |_, this, (pane, width, height, mask): (LuaValue, LuaValue, LuaValue, LuaValue)| {
                let target = desktop_target(arg(width, "width")?, arg(height, "height")?);
                this.get()?
                    .update_desktop_target(arg(pane, "pane")?, target, arg(mask, "reason_mask")?)
                    .lua()
            },
        );
        methods.add_method(
            "observe_marker",
            |_,
             this,
             (pane, value, row, column, alternate): (
                LuaValue,
                LuaValue,
                LuaValue,
                LuaValue,
                LuaValue,
            )| {
                this.get()?.observe_marker(
                    arg(pane, "pane")?,
                    &arg::<String>(value, "value")?,
                    arg(row, "row")?,
                    arg(column, "column")?,
                    arg(alternate, "alternate")?,
                );
                Ok(())
            },
        );
        methods.add_method(
            "scroll_anchors",
            |_, this, (pane, lines, alternate): (LuaValue, LuaValue, LuaValue)| {
                this.get()?.scroll_anchors(
                    arg(pane, "pane")?,
                    arg(lines, "lines")?,
                    arg(alternate, "alternate")?,
                );
                Ok(())
            },
        );
        methods.add_method(
            "clear_anchors",
            |_, this, (pane, alternate): (LuaValue, LuaValue)| {
                this.get()?
                    .clear_anchors(arg(pane, "pane")?, arg(alternate, "alternate")?);
                Ok(())
            },
        );
        methods.add_method(
            "set_alternate_screen",
            |_, this, (pane, alternate): (LuaValue, LuaValue)| {
                this.get()?
                    .set_alternate_screen(arg(pane, "pane")?, arg(alternate, "alternate")?);
                Ok(())
            },
        );
        methods.add_method("pane_for_source", |_, this, source: LuaValue| {
            Ok(this.get()?.pane_for_source(source_from(source, "source")?))
        });
        methods.add_method("projection_revision", |_, this, ()| {
            Ok(this.get()?.revision())
        });

        // -- Keyframes and outer playback --------------------------------------
        methods.add_method(
            "request_keyframe",
            |_, this, (source, minimum_epoch, reason): (LuaValue, LuaValue, LuaValue)| {
                let outcome = this.get()?.request_keyframe(
                    source_from(source, "source")?,
                    opt(minimum_epoch, "minimum_epoch")?,
                    arg(reason, "reason")?,
                );
                Ok(match outcome {
                    KeyframeRequestOutcome::Forwarded => "forwarded",
                    KeyframeRequestOutcome::Damped => "damped",
                    KeyframeRequestOutcome::Ignored => "ignored",
                })
            },
        );
        methods.add_method(
            "request_full_frames",
            |_, this, (sources, reason): (LuaValue, LuaValue)| {
                let sources = sequence(&arg::<LuaTable>(sources, "sources")?, "sources")?
                    .into_iter()
                    .map(|source| source_from(source, "source"))
                    .collect::<LuaResult<Vec<_>>>()?;
                this.get()?
                    .request_full_frames(&sources, arg(reason, "reason")?);
                Ok(())
            },
        );
        methods.add_method(
            "apply_outer_position",
            |_, this, (source, position): (LuaValue, LuaValue)| {
                let position = arg::<LuaTable>(position, "position")?;
                check_keys(
                    &position,
                    &[
                        "decoder_reset_serial",
                        "playing",
                        "start_pts_us",
                        "state",
                        "clock_pts_us",
                        "decoded_pts_us",
                        "presented_pts_us",
                        "presentation_id",
                    ],
                    "outer position",
                )?;
                let snapshot = BridgePositionSnapshot {
                    decoder_reset_serial: need(&position, "decoder_reset_serial")?,
                    playing: need(&position, "playing")?,
                    start_pts_us: need(&position, "start_pts_us")?,
                    state: need(&position, "state")?,
                    clock_pts_us: get(&position, "clock_pts_us")?,
                    decoded_pts_us: need(&position, "decoded_pts_us")?,
                    presented_pts_us: need(&position, "presented_pts_us")?,
                    presentation_id: need(&position, "presentation_id")?,
                };
                this.get()?
                    .apply_outer_position(source_from(source, "source")?, snapshot);
                Ok(())
            },
        );
        methods.add_method(
            "apply_outer_playback",
            |_, this, (source, serial, state, eos): (LuaValue, LuaValue, LuaValue, LuaValue)| {
                this.get()?.apply_outer_playback(
                    source_from(source, "source")?,
                    arg(serial, "decoder_reset_serial")?,
                    arg(state, "state")?,
                    arg(eos, "eos_state")?,
                );
                Ok(())
            },
        );

        // -- Media resources ---------------------------------------------------
        // `pinned = true` freezes the content; otherwise the resource follows the surface.
        methods.add_method(
            "announce_media_resource",
            |_, this, (source, pinned): (LuaValue, LuaValue)| {
                let binding = if arg::<bool>(pinned, "pinned")? {
                    Binding::Pinned
                } else {
                    Binding::Live
                };
                this.get()?
                    .announce_media_resource(source_from(source, "source")?, binding)
                    .map_err(|error| vivid(error.to_string()))
            },
        );
        methods.add_method("describe_media_resource", |lua, this, id: LuaValue| {
            let description = this
                .get()?
                .describe_media_resource(&arg::<String>(id, "id")?)
                .map_err(|error| vivid(error.to_string()))?;
            let table = lua.create_table()?;
            table.set("pinned", matches!(description.binding, Binding::Pinned))?;
            table.set("producer", description.producer)?;
            table.set("context", description.context_id)?;
            table.set("surface", description.surface_id)?;
            table.set("surface_revision", description.surface_revision)?;
            table.set("surface_generation", description.surface_generation)?;
            if let Some(facts) = &description.track {
                let track = lua.create_table()?;
                track.set("track_id", facts.track_id)?;
                track.set("revision", facts.revision)?;
                track.set("channel_generation", facts.channel_generation)?;
                track.set("media_epoch", facts.media_epoch)?;
                track.set("capturable", facts.capturable)?;
                table.set("track", track)?;
            }
            Ok(table)
        });
        methods.add_method("release_media_resource", |_, this, id: LuaValue| {
            Ok(this
                .get()?
                .release_media_resource(&arg::<String>(id, "id")?))
        });
    }
}

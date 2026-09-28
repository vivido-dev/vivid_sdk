//! Senders that keep media identity continuous across channel recovery, encoder pacing, and the
//! microphone uplink.

use std::time::Duration;

use mlua::prelude::*;
use vivid_sdk::{
    AudioPacketData, EncodedPacket, SendPressure, TrackSender, VideoPacketData, VideoRateControl,
};

use crate::convert::{Bytes, arg, check_keys, get, need};
use crate::error::IoResultExt;
use crate::session::{LuaSession, LuaTrack, LuaTrackChannel};

pub fn add_session_methods<M: LuaUserDataMethods<LuaSession>>(methods: &mut M) {
    // Recover a lost channel: advance, reopen, and send the key unit. Only this track is touched,
    // and the returned sender continues its packet IDs and epoch.
    methods.add_method_mut(
        "recover_channel",
        |_, this, (track, key_unit): (LuaUserDataRef<LuaTrack>, LuaString)| {
            let sender =
                vivid_sdk::recover_channel(this.get_mut()?, &track.inner, &key_unit.as_bytes())
                    .lua()?;
            Ok(LuaTrackSender { inner: sender })
        },
    );
}

pub fn add_channel_methods<M: LuaUserDataMethods<LuaTrackChannel>>(methods: &mut M) {
    // Microphone audio travels the reverse direction on the uplink track's channel; a grant opens
    // the next window of packets.
    methods.add_method("grant_audio_input", |_, this, ()| {
        this.get()?.grant_audio_input().lua()
    });
    methods.add_method("take_audio_input", |lua, this, ()| {
        let Some(packet) = this.get()?.take_audio_input().lua()? else {
            return Ok(LuaValue::Nil);
        };
        let table = lua.create_table()?;
        table.set("epoch", packet.epoch)?;
        table.set("packet_id", packet.packet_id)?;
        table.set("pts_us", packet.pts_us)?;
        table.set("pcm", lua.create_string(packet.pcm)?)?;
        Ok(LuaValue::Table(table))
    });
}

const VIDEO_KEYS: &[&str] = &[
    "packet_id",
    "pts_us",
    "dts_us",
    "duration_us",
    "key",
    "epoch",
];
const AUDIO_KEYS: &[&str] = &["packet_id", "pts_us", "duration_us", "epoch"];

/// One video access unit for a sender: packet ID and epoch default to the sender's continuity.
pub fn video_packet(
    sender: &TrackSender,
    data: Vec<u8>,
    config: &LuaTable,
) -> LuaResult<EncodedPacket> {
    check_keys(config, VIDEO_KEYS, "video packet")?;
    let pts_us = need::<i64>(config, "pts_us")?;
    Ok(EncodedPacket::Video(VideoPacketData {
        epoch: match get(config, "epoch")? {
            Some(epoch) => epoch,
            None => sender.current_epoch(),
        },
        packet_id: match get(config, "packet_id")? {
            Some(packet_id) => packet_id,
            None => sender.next_packet_id(),
        },
        pts_us,
        dts_us: get(config, "dts_us")?.unwrap_or(pts_us),
        duration_us: get(config, "duration_us")?.unwrap_or(0),
        key: get(config, "key")?.unwrap_or(false),
        data,
    }))
}

pub fn audio_packet(
    sender: &TrackSender,
    data: Vec<u8>,
    config: &LuaTable,
) -> LuaResult<EncodedPacket> {
    check_keys(config, AUDIO_KEYS, "audio packet")?;
    let pts_us = need::<i64>(config, "pts_us")?;
    Ok(EncodedPacket::Audio(AudioPacketData {
        epoch: match get(config, "epoch")? {
            Some(epoch) => epoch,
            None => sender.current_epoch(),
        },
        packet_id: match get(config, "packet_id")? {
            Some(packet_id) => packet_id,
            None => sender.next_packet_id(),
        },
        pts_us,
        dts_us: pts_us,
        duration_us: need(config, "duration_us")?,
        data,
    }))
}

/// A sender continuing a recovered track's media sequence.
pub struct LuaTrackSender {
    inner: TrackSender,
}

impl LuaUserData for LuaTrackSender {
    fn add_fields<F: LuaUserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("generation", |_, this| Ok(this.inner.generation().get()));
        fields.add_field_method_get("detached", |_, this| Ok(this.inner.is_detached()));
    }

    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(LuaMetaMethod::ToString, |_, this, ()| {
            Ok(format!(
                "vivid_sdk.TrackSender(generation={}, epoch={})",
                this.inner.generation().get(),
                this.inner.current_epoch()
            ))
        });
        methods.add_method("next_packet_id", |_, this, ()| {
            Ok(this.inner.next_packet_id())
        });
        methods.add_method("current_epoch", |_, this, ()| {
            Ok(this.inner.current_epoch())
        });
        methods.add_method("bump_epoch", |_, this, ()| Ok(this.inner.bump_epoch()));
        methods.add_method("detach", |_, this, ()| {
            this.inner.detach();
            Ok(())
        });
        methods.add_method(
            "send_video",
            |_, this, (data, config): (LuaValue, LuaValue)| {
                let data = arg::<Bytes>(data, "data")?.0;
                let config = arg::<LuaTable>(config, "video packet")?;
                let packet = video_packet(&this.inner, data, &config)?;
                this.inner.send(&packet).lua()
            },
        );
        methods.add_method(
            "send_audio",
            |_, this, (data, config): (LuaValue, LuaValue)| {
                let data = arg::<Bytes>(data, "data")?.0;
                let config = arg::<LuaTable>(config, "audio packet")?;
                let packet = audio_packet(&this.inner, data, &config)?;
                this.inner.send(&packet).lua()
            },
        );
    }
}

/// Producer-side encoder pacing, fed by `take_send_pressure` observations.
///
/// Send pressure has three causes with opposite remedies: a rate limit means the encoder should
/// produce less, a flow limit means the presenter is behind, and transport time means the writes
/// themselves are slow. The controller keeps them apart.
pub struct LuaVideoRateControl {
    inner: VideoRateControl,
}

pub fn video_rate_control(_: &Lua, bits_per_second: LuaValue) -> LuaResult<LuaVideoRateControl> {
    Ok(LuaVideoRateControl {
        inner: VideoRateControl::new(arg(bits_per_second, "configured_bits_per_second")?),
    })
}

impl LuaUserData for LuaVideoRateControl {
    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(LuaMetaMethod::ToString, |_, this, ()| {
            Ok(format!(
                "vivid_sdk.VideoRateControl(target={})",
                this.inner.target()
            ))
        });
        methods.add_method(
            "observe_send",
            |_, this, (bytes, pressure): (LuaValue, LuaValue)| {
                let pressure = arg::<LuaTable>(pressure, "pressure")?;
                check_keys(
                    &pressure,
                    &[
                        "rate_limited_us",
                        "flow_limited_us",
                        "transport_us",
                        "records",
                    ],
                    "send pressure",
                )?;
                this.inner.observe_send(
                    arg(bytes, "bytes")?,
                    SendPressure {
                        rate_limited: Duration::from_micros(need(&pressure, "rate_limited_us")?),
                        flow_limited: Duration::from_micros(need(&pressure, "flow_limited_us")?),
                        transport: Duration::from_micros(need(&pressure, "transport_us")?),
                        records: need(&pressure, "records")?,
                    },
                );
                Ok(())
            },
        );
        methods.add_method("observe_audio_backlog", |_, this, backlog: LuaValue| {
            this.inner
                .observe_audio_backlog(arg(backlog, "backlog_us")?);
            Ok(())
        });
        methods.add_method("poll", |_, this, ()| Ok(this.inner.poll()));
        methods.add_method("snapshot", |lua, this, ()| {
            let snapshot = this.inner.snapshot();
            let table = lua.create_table()?;
            table.set(
                "configured_bits_per_second",
                snapshot.configured_bits_per_second,
            )?;
            table.set("target_bits_per_second", snapshot.target_bits_per_second)?;
            table.set("adjustments", snapshot.adjustments)?;
            table.set(
                "rate_limited_us",
                crate::convert::micros(snapshot.rate_limited),
            )?;
            table.set(
                "flow_limited_us",
                crate::convert::micros(snapshot.flow_limited),
            )?;
            table.set("transport_us", crate::convert::micros(snapshot.transport))?;
            Ok(table)
        });
    }
}

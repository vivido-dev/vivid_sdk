//! Desktop presentation orchestration.
//!
//! Establishing a desktop presentation is one shape every time: a desktop surface carrying typed
//! parameters, a full-target node, a video track with its sender, optional audio, and an input
//! lane when the presenter accepts injection. `vivid_sdk::DesktopSession` owns that shape, and the
//! session it takes.

use mlua::prelude::*;
use vivid_sdk::DesktopSession;

use crate::convert::{Bytes, arg};
use crate::error::{IoResultExt, closed};
use crate::pipeline::{audio_packet, video_packet};
use crate::session::{LuaSession, LuaTrack, surface_definition, track_configuration};

/// `vivid.establish_desktop(session, surface, video, audio)`, taking ownership of `session`.
///
/// Both configurations are resolved against the session before it is taken, so a configuration
/// the SDK refuses leaves the session open and usable.
pub fn establish_desktop(
    _: &Lua,
    (session, surface, video, audio): (LuaUserDataRefMut<LuaSession>, LuaValue, LuaValue, LuaValue),
) -> LuaResult<LuaDesktopSession> {
    let mut session = session;
    let live = session.get()?;
    let definition = surface_definition(
        live,
        &arg::<LuaTable>(surface, "surface configuration")?,
        None,
    )?;
    let (context_id, surface_id) = (definition.context_id, definition.surface_id);
    let video = track_configuration(
        live,
        context_id,
        surface_id,
        &arg::<LuaTable>(video, "video configuration")?,
    )?;
    let audio = match audio {
        LuaValue::Nil => None,
        audio => Some(track_configuration(
            live,
            context_id,
            surface_id,
            &arg::<LuaTable>(audio, "audio configuration")?,
        )?),
    };
    let owned = session.take()?;
    Ok(LuaDesktopSession {
        inner: Some(DesktopSession::establish(owned, definition, video, audio).lua()?),
    })
}

pub struct LuaDesktopSession {
    inner: Option<DesktopSession>,
}

impl LuaDesktopSession {
    fn get(&self) -> LuaResult<&DesktopSession> {
        self.inner.as_ref().ok_or_else(|| closed("desktop session"))
    }

    fn get_mut(&mut self) -> LuaResult<&mut DesktopSession> {
        self.inner.as_mut().ok_or_else(|| closed("desktop session"))
    }
}

impl LuaUserData for LuaDesktopSession {
    fn add_fields<F: LuaUserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("closed", |_, this| Ok(this.inner.is_none()));
    }

    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(LuaMetaMethod::ToString, |_, this, ()| {
            Ok(if this.inner.is_some() {
                "vivid_sdk.DesktopSession"
            } else {
                "vivid_sdk.DesktopSession(closed)"
            })
        });
        #[cfg(any(feature = "lua54", feature = "lua55"))]
        methods.add_meta_method_mut(LuaMetaMethod::Close, |_, this, _: LuaMultiValue| match this
            .inner
            .take()
        {
            Some(desktop) => desktop.close().lua(),
            None => Ok(()),
        });
        methods.add_method("video_track", |_, this, ()| {
            Ok(LuaTrack {
                inner: this.get()?.video_track().clone(),
            })
        });
        methods.add_method("audio_track", |_, this, ()| {
            Ok(this.get()?.audio_track().map(|track| LuaTrack {
                inner: track.clone(),
            }))
        });
        methods.add_method(
            "send_video",
            |_, this, (data, config): (LuaValue, LuaValue)| {
                let sender = this.get()?.video_sender();
                let data = arg::<Bytes>(data, "data")?.0;
                let packet = video_packet(sender, data, &arg::<LuaTable>(config, "video packet")?)?;
                sender.send(&packet).lua()
            },
        );
        methods.add_method(
            "send_audio",
            |_, this, (data, config): (LuaValue, LuaValue)| {
                let sender = this.get()?.audio_sender().ok_or_else(|| {
                    crate::error::invalid("this desktop session has no audio track")
                })?;
                let data = arg::<Bytes>(data, "data")?.0;
                let packet = audio_packet(sender, data, &arg::<LuaTable>(config, "audio packet")?)?;
                sender.send(&packet).lua()
            },
        );
        // Wait for decoded-output readiness and activate the video and audio slots atomically.
        methods.add_method_mut("activate_slots", |_, this, ()| {
            this.get_mut()?.activate_slots().lua()
        });
        methods.add_method_mut("close", |_, this, ()| {
            this.inner
                .take()
                .ok_or_else(|| closed("desktop session"))?
                .close()
                .lua()
        });
    }
}

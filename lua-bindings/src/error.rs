//! How SDK failures reach Lua.
//!
//! Every failure is raised as an ordinary Lua error, so an unhandled one stops a script with its
//! message and a traceback. A caller that wants to act on the kind passes the caught value to
//! `vivid.error_info`, which reads the structured error this module attached — the Lua analogue of
//! Python's `ClosedHandleError` subclass and the TypeScript classes, without parsing a message.

use std::fmt;
use std::io;

use mlua::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The handle was closed by this process, or closed twice.
    Closed,
    /// The request was refused before it reached a presenter: a value out of range, a missing
    /// field, a configuration the SDK would not send.
    Invalid,
    /// A presenter refused the request, the transport failed, or the protocol was violated.
    Vivid,
    /// An automation endpoint refused a request or could not be resolved.
    Automation,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::Invalid => "invalid",
            Self::Vivid => "vivid",
            Self::Automation => "automation",
        }
    }
}

#[derive(Debug)]
enum Code {
    /// A presenter's registered error code.
    Presenter { code: u64, fatal: Option<bool> },
    /// An automation runtime's documented error code, with its optional structured data.
    Automation {
        code: String,
        data: Option<serde_json::Value>,
    },
}

#[derive(Debug)]
pub struct SdkError {
    kind: Kind,
    message: String,
    code: Option<Code>,
}

impl fmt::Display for SdkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.code {
            Some(Code::Automation { code, .. }) => write!(formatter, "{code}: {}", self.message),
            _ => formatter.write_str(&self.message),
        }
    }
}

impl std::error::Error for SdkError {}

fn raise(kind: Kind, message: impl fmt::Display, code: Option<Code>) -> LuaError {
    LuaError::external(SdkError {
        kind,
        message: message.to_string(),
        code,
    })
}

/// A handle used after it was closed. `what` names the handle: "session", "track channel".
pub fn closed(what: &str) -> LuaError {
    raise(Kind::Closed, format!("{what} is closed"), None)
}

pub fn invalid(message: impl fmt::Display) -> LuaError {
    raise(Kind::Invalid, message, None)
}

pub fn vivid(message: impl fmt::Display) -> LuaError {
    raise(Kind::Vivid, message, None)
}

pub fn automation(
    code: impl Into<String>,
    message: impl fmt::Display,
    data: Option<serde_json::Value>,
) -> LuaError {
    raise(
        Kind::Automation,
        message,
        Some(Code::Automation {
            code: code.into(),
            data,
        }),
    )
}

/// An SDK I/O error. `InvalidInput` is the SDK's "refused before sending"; a presenter rejection
/// keeps its registered code, which is what a caller should branch on rather than the diagnostic.
pub fn io(error: io::Error) -> LuaError {
    let kind = if error.kind() == io::ErrorKind::InvalidInput {
        Kind::Invalid
    } else {
        Kind::Vivid
    };
    let code = error.get_ref().and_then(|inner| {
        if let Some(rejection) = inner.downcast_ref::<vivid_sdk::PresenterError>() {
            Some(Code::Presenter {
                code: rejection.code,
                fatal: Some(rejection.fatal),
            })
        } else {
            inner
                .downcast_ref::<vivid_sdk::TrackLostError>()
                .map(|lost| Code::Presenter {
                    code: lost.code,
                    fatal: None,
                })
        }
    });
    raise(kind, error, code)
}

/// The SDK error inside whatever mlua wrapped around it on the way through Lua.
fn find(error: &LuaError) -> Option<&SdkError> {
    match error {
        LuaError::CallbackError { cause, .. }
        | LuaError::WithContext { cause, .. }
        | LuaError::BadArgument { cause, .. } => find(cause),
        LuaError::ExternalError(inner) => inner.downcast_ref::<SdkError>(),
        _ => None,
    }
}

/// A conversion mlua refused on the way in — a string where a number was expected — which is the
/// same class of mistake as an out-of-range value, and is reported as one.
fn conversion(error: &LuaError) -> Option<String> {
    match error {
        LuaError::CallbackError { cause, .. } | LuaError::WithContext { cause, .. } => {
            conversion(cause)
        }
        LuaError::BadArgument { .. }
        | LuaError::FromLuaConversionError { .. }
        | LuaError::UserDataTypeMismatch => Some(error.to_string()),
        _ => None,
    }
}

/// `vivid.error_info(err)`: the structured form of a caught SDK error, or `nil` for anything else.
pub fn error_info(lua: &Lua, value: LuaValue) -> LuaResult<LuaValue> {
    let LuaValue::Error(error) = value else {
        return Ok(LuaValue::Nil);
    };
    let info = lua.create_table()?;
    match find(&error) {
        Some(sdk) => {
            info.set("kind", sdk.kind.name())?;
            info.set("message", sdk.message.as_str())?;
            match &sdk.code {
                Some(Code::Presenter { code, fatal }) => {
                    info.set("code", *code)?;
                    info.set("fatal", *fatal)?;
                }
                Some(Code::Automation { code, data }) => {
                    info.set("code", code.as_str())?;
                    if let Some(data) = data {
                        info.set("data", crate::convert::json_to_lua(lua, data)?)?;
                    }
                }
                None => {}
            }
        }
        None => match conversion(&error) {
            Some(message) => {
                info.set("kind", Kind::Invalid.name())?;
                info.set("message", message)?;
            }
            None => return Ok(LuaValue::Nil),
        },
    }
    Ok(LuaValue::Table(info))
}

/// The `Result` every binding function returns, with SDK I/O errors already translated.
pub trait IoResultExt<T> {
    fn lua(self) -> LuaResult<T>;
}

impl<T> IoResultExt<T> for io::Result<T> {
    fn lua(self) -> LuaResult<T> {
        self.map_err(io)
    }
}

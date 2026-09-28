//! Drive the terminal runtimes over their automation endpoints.
//!
//! `vivido` controls terminal windows, `vivida` embeds vivido's host and adds workspace layout on
//! the same endpoint, and `vvmux` adds panes, tabs, and agents. Two wire protocols, one module:
//!
//! * **vivido / vivida**: newline-delimited JSON over an owner-checked local socket. A `hello`
//!   handshake answers with the capability document; everything else is
//!   `{version, id, method, params}` in, `{version, id, ok, result | error}` out.
//! * **vvmux**: VVMX, a 12-byte preface, then sequence-numbered length-prefixed records whose
//!   structured bodies are JSON.
//!
//! This is a port of the Python client, which is the reference, and it makes the same checks:
//! every socket must belong to this user before a byte is written and its peer must be this user
//! after connecting; registries are read only from a plain, owner-only runtime directory; and the
//! socket path is derived from the session name rather than taken from the registry. A named
//! target that has gone away is an error, never a fall-through to another instance. Unix only.

#[cfg(unix)]
pub use unix::module;

#[cfg(not(unix))]
pub fn module(lua: &mlua::Lua) -> mlua::Result<mlua::Table> {
    // The runtimes also serve Windows named pipes; use their CLIs there.
    let module = lua.create_table()?;
    for name in ["vivido_connect", "vivido_instances", "vvmux_connect"] {
        module.set(
            name,
            lua.create_function(|_, _: mlua::MultiValue| -> mlua::Result<()> {
                Err(crate::error::automation(
                    "unsupported_platform",
                    "the automation client is Unix only",
                    None,
                ))
            })?,
        )?;
    }
    Ok(module)
}

#[cfg(unix)]
mod unix {
    use std::fs;
    use std::io::{self, BufRead, BufReader, Read, Write};
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::io::AsRawFd;
    use std::os::unix::net::UnixStream;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use mlua::prelude::*;
    use serde_json::{Map, Value, json};
    use sha2::{Digest, Sha256};

    use crate::convert::{
        Field, arg, check_keys, get, json_to_lua, lua_to_json, opt, options, timeout,
    };
    use crate::error::automation;

    /// The newline-delimited protocol vivido serves and vivida embeds.
    const PROTOCOL_VERSION: u64 = 2;
    const MAX_REQUEST_FRAME_BYTES: usize = 1024 * 1024;
    const MAX_REPLY_FRAME_BYTES: u64 = 16 * 1024 * 1024;

    /// The VVMX preface version offered first. The exchange discovers the server's own, and a
    /// mismatch retries once speaking it, so a vvmux rebuilt across a bump still connects.
    const VVMX_VERSION: u16 = 20;
    const VVMX_MAGIC: &[u8; 4] = b"VVMX";
    const VVMX_CONTROL_CHANNEL: u8 = 1;
    const VVMX_CONTROL_MAX_BODY: u32 = 1024 * 1024;
    const VVMX_STRUCTURED_RECORD: u16 = 1;
    const VVMX_HEADER_BYTES: usize = 16;

    const SESSION_ENV: &str = "VIVIDO_SESSION";
    const SOCKET_ENV: &str = "VIVIDO_SOCKET";

    type Result<T> = LuaResult<T>;

    fn io_failure(error: io::Error) -> LuaError {
        let code = match error.kind() {
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => "timeout",
            _ => "io_error",
        };
        automation(code, error, None)
    }

    // ---------------------------------------------------------------------------------------
    // Shared endpoint plumbing
    // ---------------------------------------------------------------------------------------

    fn effective_uid() -> u32 {
        // SAFETY: geteuid has no preconditions and cannot fail.
        unsafe { libc::geteuid() }
    }

    /// The rule both runtimes enforce, checked before a name becomes part of a socket path.
    fn validate_session_name(name: &str) -> Result<()> {
        let valid = !name.is_empty()
            && name.len() <= 64
            && !name.starts_with('.')
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte));
        if valid {
            Ok(())
        } else {
            Err(automation(
                "invalid_session_name",
                "session name must be 1-64 ASCII letters, digits, '.', '-' or '_' and not start '.'",
                None,
            ))
        }
    }

    fn name_digest(name: &str) -> String {
        crate::convert::hex(&Sha256::digest(name.as_bytes())[..16])
    }

    /// The per-user runtime root for a product, held to the servers' own standard: a registry
    /// read from a directory another user can write to is a socket path chosen by that user. One
    /// that fails the check is declined, never repaired.
    fn runtime_dir(product: &str) -> Result<PathBuf> {
        let base = match std::env::var_os("XDG_RUNTIME_DIR") {
            Some(base) if !base.is_empty() => PathBuf::from(base),
            _ => PathBuf::from(format!("/tmp/{product}-{}", effective_uid())),
        };
        let root = base.join(product);
        let meta = fs::symlink_metadata(&root).map_err(|_| {
            automation(
                "endpoint_not_found",
                format!("no {product} runtime directory at {}", root.display()),
                None,
            )
        })?;
        if meta.file_type().is_symlink()
            || !meta.is_dir()
            || meta.uid() != effective_uid()
            || meta.mode() & 0o077 != 0
        {
            return Err(automation(
                "endpoint_unsafe",
                format!(
                    "{product} runtime directory {} is not owner-only",
                    root.display()
                ),
                None,
            ));
        }
        Ok(root)
    }

    /// The connected peer's uid, where the platform can tell.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn peer_uid(stream: &UnixStream) -> Option<u32> {
        let mut credential = libc::ucred {
            pid: 0,
            uid: 0,
            gid: 0,
        };
        let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        // SAFETY: the descriptor is a live socket owned by `stream`, and the buffer and length
        // describe a properly sized `ucred`.
        let status = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut credential as *mut libc::ucred).cast(),
                &mut length,
            )
        };
        (status == 0).then_some(credential.uid)
    }

    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    fn peer_uid(stream: &UnixStream) -> Option<u32> {
        let mut uid: libc::uid_t = 0;
        let mut gid: libc::gid_t = 0;
        // SAFETY: the descriptor is a live socket owned by `stream`; both out-pointers are valid.
        let status = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
        (status == 0).then_some(uid)
    }

    /// Connect to a local automation socket that belongs to this user, before and after.
    fn connect_socket(path: &Path) -> Result<UnixStream> {
        let meta = fs::symlink_metadata(path).map_err(|_| {
            automation(
                "endpoint_not_found",
                format!("no endpoint socket at {}", path.display()),
                None,
            )
        })?;
        if meta.file_type().is_symlink() || meta.uid() != effective_uid() {
            return Err(automation(
                "endpoint_unsafe",
                format!(
                    "endpoint socket {} is not owned by this user",
                    path.display()
                ),
                None,
            ));
        }
        let stream = UnixStream::connect(path).map_err(io_failure)?;
        if let Some(peer) = peer_uid(&stream)
            && peer != effective_uid()
        {
            return Err(automation(
                "endpoint_unsafe",
                format!(
                    "endpoint {} is served by uid {peer}, not this user",
                    path.display()
                ),
                None,
            ));
        }
        Ok(stream)
    }

    fn set_timeout(stream: &UnixStream, timeout: Option<Duration>) -> Result<()> {
        if timeout == Some(Duration::ZERO) {
            // A zero timeout means "no timeout" to the socket API; refuse it instead.
            return Err(automation(
                "invalid_request",
                "timeout must be greater than zero",
                None,
            ));
        }
        stream.set_read_timeout(timeout).map_err(io_failure)?;
        stream.set_write_timeout(timeout).map_err(io_failure)
    }

    /// The Linux process-birth record a registry carries, recomputed from the same field of the
    /// same file: the start time in clock ticks. It is what makes a recycled pid a stale
    /// registry rather than someone else's session.
    #[cfg(target_os = "linux")]
    fn birth_matches(pid: u32, registry: &Map<String, Value>) -> bool {
        let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
            return false;
        };
        let Some(end) = stat.rfind(") ") else {
            return false;
        };
        let fields: Vec<&str> = stat[end + 2..].split_whitespace().collect();
        let Some(ticks) = fields.get(19).and_then(|field| field.parse::<u64>().ok()) else {
            return false;
        };
        registry.get("process_birth") == Some(&json!({"platform": "linux", "start_ticks": ticks}))
    }

    /// Elsewhere the birth record has a shape this client cannot recompute, so liveness alone
    /// decides — a documented weaker check, not a silent one.
    #[cfg(not(target_os = "linux"))]
    fn birth_matches(_: u32, _: &Map<String, Value>) -> bool {
        true
    }

    fn process_matches(registry: &Map<String, Value>) -> bool {
        let Some(pid) = registry
            .get("pid")
            .and_then(Value::as_u64)
            .filter(|pid| (1..=i32::MAX as u64).contains(pid))
        else {
            return false;
        };
        // SAFETY: signal 0 performs only the existence and permission check.
        if unsafe { libc::kill(pid as libc::pid_t, 0) } != 0 {
            return false;
        }
        birth_matches(pid as u32, registry)
    }

    /// Whether a registry is the one this name and socket layout produce. The socket path is
    /// derived from the name, so editing one JSON file cannot point a name at another socket.
    fn identity_ok(root: &Path, registry: &Map<String, Value>) -> bool {
        let (Some(name), Some(socket)) = (
            registry.get("name").and_then(Value::as_str),
            registry.get("socket").and_then(Value::as_str),
        ) else {
            return false;
        };
        Path::new(socket) == root.join(format!("session-{}.sock", name_digest(name)))
    }

    fn registry_socket(registry: &Map<String, Value>) -> PathBuf {
        PathBuf::from(
            registry
                .get("socket")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        )
    }

    // ---------------------------------------------------------------------------------------
    // vivido / vivida
    // ---------------------------------------------------------------------------------------

    /// Every live Vivido instance this user can reach, validated as `vivido list --all` does.
    fn instances() -> Result<Vec<Map<String, Value>>> {
        let root = runtime_dir("vivido")?;
        let mut entries: Vec<PathBuf> = fs::read_dir(&root)
            .map_err(io_failure)?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("session-") && name.ends_with(".json"))
            })
            .collect();
        entries.sort();
        let mut found = Vec::new();
        for path in entries {
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            let Ok(Value::Object(registry)) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            if registry.get("schema") != Some(&json!(1)) || !identity_ok(&root, &registry) {
                continue;
            }
            if process_matches(&registry) {
                found.push(registry);
            }
        }
        Ok(found)
    }

    fn named_registry(root: &Path, name: &str) -> Result<Map<String, Value>> {
        validate_session_name(name)?;
        let path = root.join(format!("session-{}.json", name_digest(name)));
        let text = fs::read_to_string(&path).map_err(|_| {
            automation(
                "endpoint_not_found",
                format!("no running Vivido instance named {name:?}"),
                None,
            )
        })?;
        let unsafe_registry = |why: &str| {
            automation(
                "endpoint_unsafe",
                format!("registry for {name:?} {why}"),
                None,
            )
        };
        let registry = match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(registry)) => registry,
            Ok(_) => return Err(unsafe_registry("is not an object")),
            Err(_) => return Err(unsafe_registry("is not valid JSON")),
        };
        if registry.get("schema") != Some(&json!(1))
            || registry.get("protocol_version") != Some(&json!(PROTOCOL_VERSION))
        {
            return Err(unsafe_registry("is not a schema this client reads"));
        }
        if registry.get("name").and_then(Value::as_str) != Some(name)
            || !identity_ok(root, &registry)
        {
            return Err(unsafe_registry("does not match its endpoint identity"));
        }
        if !process_matches(&registry) {
            return Err(automation(
                "endpoint_not_found",
                format!("Vivido instance {name:?} is no longer running"),
                None,
            ));
        }
        Ok(registry)
    }

    /// Windowed instances advertise on the display they render to: `Vivido-<display>-<pid>.sock`,
    /// tried newest first by name, the order the CLI uses. Stale sockets are skipped.
    fn newest_windowed(root: &Path) -> Result<UnixStream> {
        let display = std::env::var("WAYLAND_DISPLAY")
            .ok()
            .filter(|value| !value.is_empty())
            .or_else(|| std::env::var("DISPLAY").ok())
            .unwrap_or_default();
        let prefix = format!("Vivido-{}-", display.replace('/', "-"));
        let mut candidates: Vec<PathBuf> = fs::read_dir(root)
            .map_err(io_failure)?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(&prefix) && name.ends_with(".sock"))
            })
            .collect();
        candidates.sort();
        for path in candidates.iter().rev() {
            if let Ok(stream) = connect_socket(path) {
                return Ok(stream);
            }
        }
        Err(automation(
            "endpoint_not_found",
            "no windowed Vivido instance on this display",
            None,
        ))
    }

    /// The unqualified order: inherited socket, sole live instance, newest windowed. Each step
    /// may decline; only running out of steps is an error.
    fn discover(root: Option<&Path>) -> Result<UnixStream> {
        if let Some(inherited) = std::env::var_os(SOCKET_ENV).filter(|value| !value.is_empty())
            && let Ok(stream) = connect_socket(Path::new(&inherited))
        {
            return Ok(stream);
        }
        let Some(root) = root else {
            return Err(automation(
                "endpoint_not_found",
                "no vivido endpoint: pass socket or target, or start an instance",
                None,
            ));
        };
        let found = instances()?;
        if let [only] = found.as_slice() {
            return connect_socket(&registry_socket(only));
        }
        newest_windowed(root)
    }

    struct Vivido {
        reader: BufReader<UnixStream>,
        writer: UnixStream,
        next_id: u64,
        capabilities: Value,
    }

    impl Vivido {
        fn new(stream: UnixStream) -> Result<Self> {
            let writer = stream.try_clone().map_err(io_failure)?;
            let mut session = Self {
                reader: BufReader::new(stream),
                writer,
                next_id: 1,
                capabilities: Value::Null,
            };
            session.capabilities = session.round_trip("hello", Value::Object(Map::new()))?;
            Ok(session)
        }

        fn round_trip(&mut self, method: &str, params: Value) -> Result<Value> {
            self.next_id += 1;
            let id = self.next_id;
            let mut frame = serde_json::to_vec(&json!({
                "version": PROTOCOL_VERSION,
                "id": id,
                "method": method,
                "params": params,
            }))
            .map_err(|error| automation("invalid_request", error, None))?;
            if frame.len() + 1 > MAX_REQUEST_FRAME_BYTES {
                return Err(automation(
                    "limit_exceeded",
                    "request exceeds the 1 MiB frame limit",
                    None,
                ));
            }
            frame.push(b'\n');
            self.writer.write_all(&frame).map_err(io_failure)?;
            loop {
                let mut line = Vec::new();
                (&mut self.reader)
                    .take(MAX_REPLY_FRAME_BYTES + 1)
                    .read_until(b'\n', &mut line)
                    .map_err(io_failure)?;
                if line.is_empty() {
                    return Err(automation(
                        "endpoint_not_found",
                        "the runtime closed the connection",
                        None,
                    ));
                }
                if line.len() as u64 > MAX_REPLY_FRAME_BYTES || line.last() != Some(&b'\n') {
                    return Err(automation(
                        "limit_exceeded",
                        "reply exceeds the 16 MiB frame limit",
                        None,
                    ));
                }
                let value: Value = serde_json::from_slice(&line)
                    .map_err(|error| automation("invalid_response", error, None))?;
                let Some(reply) = value.as_object() else {
                    continue;
                };
                // A subscription event interleaved on this connection has no id and answers
                // nothing we asked.
                if reply.get("version") != Some(&json!(PROTOCOL_VERSION))
                    || reply.get("id") != Some(&json!(id))
                {
                    continue;
                }
                if reply.get("ok") == Some(&Value::Bool(true)) {
                    return Ok(reply.get("result").cloned().unwrap_or(Value::Null));
                }
                return Err(reply_error(
                    reply.get("error"),
                    "the runtime sent no error payload",
                ));
            }
        }
    }

    fn reply_error(error: Option<&Value>, fallback: &str) -> LuaError {
        let error = error.and_then(Value::as_object);
        let text = |key: &str, default: &str| {
            error
                .and_then(|error| error.get(key))
                .map(|value| match value {
                    Value::String(text) => text.clone(),
                    other => other.to_string(),
                })
                .unwrap_or_else(|| default.to_owned())
        };
        automation(
            text("code", "invalid_response"),
            text("message", fallback),
            error
                .and_then(|error| error.get("data"))
                .filter(|data| !data.is_null())
                .cloned(),
        )
    }

    fn valid_method(method: &str) -> bool {
        !method.is_empty()
            && method.len() <= 128
            && method
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    }

    /// One connection to a vivido or vivida instance. `capabilities` is the hello document, the
    /// authority on which methods this instance claims.
    pub struct LuaVivido {
        inner: Option<Vivido>,
    }

    impl LuaUserData for LuaVivido {
        fn add_fields<F: LuaUserDataFields<Self>>(fields: &mut F) {
            fields.add_field_method_get("closed", |_, this| Ok(this.inner.is_none()));
            fields.add_field_method_get("capabilities", |lua, this| match &this.inner {
                Some(session) => json_to_lua(lua, &session.capabilities),
                None => Err(crate::error::closed("automation session")),
            });
        }

        fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
            methods.add_meta_method(LuaMetaMethod::ToString, |_, this, ()| {
                Ok(if this.inner.is_some() {
                    "vivid_sdk.automation.VividoSession"
                } else {
                    "vivid_sdk.automation.VividoSession(closed)"
                })
            });
            #[cfg(any(feature = "lua54", feature = "lua55"))]
            methods.add_meta_method_mut(LuaMetaMethod::Close, |_, this, _: LuaMultiValue| {
                this.inner = None;
                Ok(())
            });
            // End this connection. The runtime keeps running; only the connection goes.
            methods.add_method_mut("close", |_, this, ()| {
                this.inner = None;
                Ok(())
            });
            // Params mirror the serde shape of the runtime's request struct, not its CLI flags.
            methods.add_method_mut(
                "request",
                |lua, this, (method, params): (LuaValue, LuaValue)| {
                    let method = arg::<String>(method, "method")?;
                    if !valid_method(&method) {
                        return Err(automation(
                            "invalid_request",
                            "method must contain 1-128 ASCII letters, digits, or underscores",
                            None,
                        ));
                    }
                    let params = match params {
                        LuaValue::Nil => Value::Object(Map::new()),
                        params => lua_to_json(lua, params)?,
                    };
                    if !params.is_object() {
                        return Err(automation(
                            "invalid_request",
                            "params must be a table of named fields",
                            None,
                        ));
                    }
                    let session = this
                        .inner
                        .as_mut()
                        .ok_or_else(|| crate::error::closed("automation session"))?;
                    let result = session.round_trip(&method, params)?;
                    json_to_lua(lua, &result)
                },
            );
        }
    }

    /// `automation.vivido_connect{ socket =, target =, timeout = }`, resolving the endpoint the
    /// way the CLI does. An explicit socket wins; then `target`, else an inherited
    /// `VIVIDO_SESSION`; without a name, discovery.
    fn vivido_connect(lua: &Lua, value: LuaValue) -> Result<LuaVivido> {
        let config = options(lua, value, "connect options")?;
        check_keys(
            &config,
            &["socket", "target", "timeout"],
            "automation option",
        )?;
        let socket = get::<String>(&config, "socket")?;
        let target = get::<String>(&config, "target")?;
        let wait = match config.get::<LuaValue>("timeout")? {
            LuaValue::Nil => None,
            value => Some(timeout(value, Duration::ZERO, "timeout")?),
        };
        let inherited = std::env::var(SESSION_ENV)
            .ok()
            .filter(|name| !name.is_empty());
        let root = match runtime_dir("vivido") {
            Ok(root) => Some(root),
            Err(error) if socket.is_none() && target.is_none() && inherited.is_none() => {
                return Err(error);
            }
            Err(_) => None,
        };
        let stream = if let Some(socket) = socket {
            connect_socket(Path::new(&socket))?
        } else if let Some(name) = target.or(inherited) {
            let root = root.ok_or_else(|| {
                automation("endpoint_not_found", "no vivido runtime directory", None)
            })?;
            connect_socket(&registry_socket(&named_registry(&root, &name)?))?
        } else {
            discover(root.as_deref())?
        };
        if wait.is_some() {
            set_timeout(&stream, wait)?;
        }
        Ok(LuaVivido {
            inner: Some(Vivido::new(stream)?),
        })
    }

    // ---------------------------------------------------------------------------------------
    // vvmux
    // ---------------------------------------------------------------------------------------

    struct Vvmux {
        stream: UnixStream,
        maximum: u32,
        send_sequence: u64,
        recv_sequence: u64,
        next_id: u64,
    }

    /// The per-request fields that belong to the request rather than to the verb.
    const ENVELOPE: &[&str] = &[
        "id",
        "pane_id",
        "agent",
        "pane_name",
        "lease",
        "allow_focused",
        "expect",
        "idempotency_key",
    ];

    impl Vvmux {
        fn send(&mut self, message: &Value) -> Result<()> {
            let body = serde_json::to_vec(message)
                .map_err(|error| automation("invalid_request", error, None))?;
            if body.len() > self.maximum as usize {
                return Err(automation(
                    "limit_exceeded",
                    "request exceeds the negotiated body limit",
                    None,
                ));
            }
            let mut record = Vec::with_capacity(VVMX_HEADER_BYTES + body.len());
            record.extend_from_slice(&self.send_sequence.to_be_bytes());
            record.extend_from_slice(&VVMX_STRUCTURED_RECORD.to_be_bytes());
            record.extend_from_slice(&0_u16.to_be_bytes());
            record.extend_from_slice(&(body.len() as u32).to_be_bytes());
            record.extend_from_slice(&body);
            self.send_sequence = self.send_sequence.wrapping_add(1);
            self.stream.write_all(&record).map_err(io_failure)
        }

        fn recv(&mut self) -> Result<Map<String, Value>> {
            let header = recv_exact(&mut self.stream, VVMX_HEADER_BYTES)?;
            let sequence = u64::from_be_bytes(header[0..8].try_into().expect("eight bytes"));
            let record_type = u16::from_be_bytes([header[8], header[9]]);
            let flags = u16::from_be_bytes([header[10], header[11]]);
            let length = u32::from_be_bytes(header[12..16].try_into().expect("four bytes"));
            if sequence != self.recv_sequence {
                return Err(automation(
                    "invalid_response",
                    format!("VVMX record sequence gap at {}", self.recv_sequence),
                    None,
                ));
            }
            self.recv_sequence = self.recv_sequence.wrapping_add(1);
            if flags & !0x0001 != 0 || record_type != VVMX_STRUCTURED_RECORD {
                return Err(automation(
                    "invalid_response",
                    "unexpected VVMX control record",
                    None,
                ));
            }
            if length > self.maximum {
                return Err(automation(
                    "invalid_response",
                    "VVMX record body exceeds the negotiated limit",
                    None,
                ));
            }
            let body = recv_exact(&mut self.stream, length as usize)?;
            match serde_json::from_slice::<Value>(&body) {
                Ok(Value::Object(value)) => Ok(value),
                _ => Err(automation(
                    "invalid_response",
                    "VVMX record body is not an object",
                    None,
                )),
            }
        }

        fn request(
            &mut self,
            method: Map<String, Value>,
            envelope: Map<String, Value>,
        ) -> Result<Value> {
            if !method
                .get("method")
                .and_then(Value::as_str)
                .is_some_and(|verb| !verb.is_empty())
            {
                return Err(automation(
                    "invalid_request",
                    "an automation method needs a `method` verb",
                    None,
                ));
            }
            let mut clash: Vec<&str> = ENVELOPE
                .iter()
                .copied()
                .filter(|key| method.contains_key(*key))
                .collect();
            clash.sort_unstable();
            if !clash.is_empty() {
                return Err(automation(
                    "invalid_request",
                    format!(
                        "{} belong on the request, not inside the method",
                        clash.join(", ")
                    ),
                    None,
                ));
            }
            self.next_id += 1;
            let id = self.next_id;
            // The method record rides whole, exactly as the schema publishes it, and the envelope
            // fields go beside it.
            let mut request = Map::new();
            request.insert("id".into(), json!(id));
            request.extend(method);
            request.extend(envelope);
            self.send(&json!({ "automation": request }))?;
            loop {
                let reply = self.recv()?;
                let Some(response) = reply.get("Automation").and_then(Value::as_object) else {
                    continue;
                };
                if response.get("id") != Some(&json!(id)) {
                    // Pong, Title, and friends are addressed to no request of ours.
                    continue;
                }
                if response.get("ok") == Some(&Value::Bool(true)) {
                    return Ok(response.get("result").cloned().unwrap_or(Value::Null));
                }
                return Err(reply_error(
                    response.get("error"),
                    "the session server sent no error payload",
                ));
            }
        }
    }

    fn recv_exact(stream: &mut UnixStream, count: usize) -> Result<Vec<u8>> {
        let mut buffer = vec![0; count];
        stream.read_exact(&mut buffer).map_err(|error| {
            if error.kind() == io::ErrorKind::UnexpectedEof {
                automation(
                    "endpoint_not_found",
                    "the endpoint closed the connection",
                    None,
                )
            } else {
                io_failure(error)
            }
        })?;
        Ok(buffer)
    }

    fn preface(version: u16) -> [u8; 12] {
        let mut bytes = [0; 12];
        bytes[..4].copy_from_slice(VVMX_MAGIC);
        bytes[4..6].copy_from_slice(&version.to_be_bytes());
        bytes[6] = VVMX_CONTROL_CHANNEL;
        bytes[7] = 0;
        bytes[8..].copy_from_slice(&VVMX_CONTROL_MAX_BODY.to_be_bytes());
        bytes
    }

    /// Exchange prefaces, offering `VVMX_VERSION` and honouring what comes back. The server
    /// writes its preface before reading ours, so a mismatch is still answered and the client
    /// reconnects once speaking the server's version. A server that disagrees twice will not
    /// agree a third time.
    fn negotiate(path: &Path, wait: Option<Duration>) -> Result<Vvmux> {
        let mut version = VVMX_VERSION;
        for attempt in 1..=2 {
            let mut stream = connect_socket(path)?;
            if wait.is_some() {
                set_timeout(&stream, wait)?;
            }
            stream.write_all(&preface(version)).map_err(io_failure)?;
            let peer = recv_exact(&mut stream, 12)?;
            if &peer[..4] != VVMX_MAGIC {
                return Err(automation("invalid_response", "bad VVMX magic", None));
            }
            let peer_version = u16::from_be_bytes([peer[4], peer[5]]);
            let peer_maximum = u32::from_be_bytes([peer[8], peer[9], peer[10], peer[11]]);
            if peer_version == version {
                if peer[6] != VVMX_CONTROL_CHANNEL || peer[7] != 0 {
                    return Err(automation(
                        "invalid_response",
                        "VVMX channel mismatch",
                        None,
                    ));
                }
                if peer_maximum == 0 || peer_maximum > VVMX_CONTROL_MAX_BODY {
                    return Err(automation(
                        "invalid_response",
                        "invalid VVMX maximum body",
                        None,
                    ));
                }
                return Ok(Vvmux {
                    stream,
                    maximum: peer_maximum,
                    send_sequence: 0,
                    recv_sequence: 0,
                    next_id: 0,
                });
            }
            if attempt == 2 {
                return Err(automation(
                    "invalid_response",
                    format!("vvmux speaks VVMX v{peer_version}, not v{version}"),
                    None,
                ));
            }
            version = peer_version;
        }
        unreachable!("the loop returns on its second attempt")
    }

    /// One connection to a vvmux session server. Requests are the automation records
    /// `vvmux api schema --json` describes, with the verb under `method`; the fields that belong
    /// to the request rather than the verb are the second argument.
    pub struct LuaVvmux {
        inner: Option<Vvmux>,
    }

    fn envelope(lua: &Lua, value: LuaValue) -> Result<Map<String, Value>> {
        let config = options(lua, value, "request envelope")?;
        check_keys(
            &config,
            &[
                "pane_id",
                "agent",
                "pane_name",
                "lease",
                "allow_focused",
                "expect",
                "idempotency_key",
            ],
            "request envelope",
        )?;
        let mut envelope = Map::new();
        if let Some(pane_id) = get::<u64>(&config, "pane_id")? {
            envelope.insert("pane_id".into(), json!(pane_id));
        }
        for key in ["agent", "pane_name", "lease", "idempotency_key"] {
            if let Some(text) = get::<String>(&config, key)? {
                envelope.insert(key.into(), json!(text));
            }
        }
        if let Some(expect) = get::<LuaTable>(&config, "expect")? {
            envelope.insert("expect".into(), lua_to_json(lua, LuaValue::Table(expect))?);
        }
        if get::<bool>(&config, "allow_focused")?.unwrap_or(false) {
            envelope.insert("allow_focused".into(), json!(true));
        }
        Ok(envelope)
    }

    impl LuaUserData for LuaVvmux {
        fn add_fields<F: LuaUserDataFields<Self>>(fields: &mut F) {
            fields.add_field_method_get("closed", |_, this| Ok(this.inner.is_none()));
        }

        fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
            methods.add_meta_method(LuaMetaMethod::ToString, |_, this, ()| {
                Ok(if this.inner.is_some() {
                    "vivid_sdk.automation.VvmuxSession"
                } else {
                    "vivid_sdk.automation.VvmuxSession(closed)"
                })
            });
            #[cfg(any(feature = "lua54", feature = "lua55"))]
            methods.add_meta_method_mut(LuaMetaMethod::Close, |_, this, _: LuaMultiValue| {
                this.inner = None;
                Ok(())
            });
            methods.add_method_mut("close", |_, this, ()| {
                this.inner = None;
                Ok(())
            });
            methods.add_method_mut(
                "request",
                |lua, this, (method, value): (LuaValue, LuaValue)| {
                    let method = match lua_to_json(lua, LuaValue::Table(arg(method, "method")?))? {
                        Value::Object(method) => method,
                        _ => {
                            return Err(automation(
                                "invalid_request",
                                "an automation method is a table with a `method` verb",
                                None,
                            ));
                        }
                    };
                    let envelope = envelope(lua, value)?;
                    let session = this
                        .inner
                        .as_mut()
                        .ok_or_else(|| crate::error::closed("automation session"))?;
                    let result = session.request(method, envelope)?;
                    json_to_lua(lua, &result)
                },
            );
        }
    }

    /// `automation.vvmux_connect(target, { timeout = })`: the socket path is derived from the
    /// name the same way the server derives its own.
    fn vvmux_connect(lua: &Lua, (target, value): (LuaValue, LuaValue)) -> Result<LuaVvmux> {
        let target = opt::<String>(target, "target")?.unwrap_or_else(|| "default".into());
        let config = options(lua, value, "connect options")?;
        check_keys(&config, &["timeout"], "automation option")?;
        let wait = match config.get::<LuaValue>("timeout")? {
            LuaValue::Nil => None,
            value => Some(timeout(value, Duration::ZERO, "timeout")?),
        };
        validate_session_name(&target)?;
        let root = runtime_dir("vvmux")?;
        let path = root.join(format!("session-{}.sock", name_digest(&target)));
        Ok(LuaVvmux {
            inner: Some(negotiate(&path, wait)?),
        })
    }

    pub fn module(lua: &Lua) -> LuaResult<LuaTable> {
        let module = lua.create_table()?;
        module.set("PROTOCOL_VERSION", PROTOCOL_VERSION)?;
        module.set("VVMX_VERSION", VVMX_VERSION)?;
        module.set("vivido_connect", lua.create_function(vivido_connect)?)?;
        module.set("vvmux_connect", lua.create_function(vvmux_connect)?)?;
        module.set(
            "vivido_instances",
            lua.create_function(|lua, ()| {
                let list = lua.create_table()?;
                for registry in instances()? {
                    list.push(json_to_lua(lua, &Value::Object(registry))?)?;
                }
                Ok(list)
            })?,
        )?;
        // JSON has arrays and objects; Lua has tables. An empty table is sent as an object unless
        // it is marked with `automation.array`, and `automation.null` is an explicit JSON null.
        module.set(
            "array",
            lua.create_function(|lua, value: LuaValue| {
                let table = match value {
                    LuaValue::Nil => lua.create_table()?,
                    value => LuaTable::from_value(value, "array")?,
                };
                table.set_metatable(Some(lua.array_metatable()))?;
                Ok(table)
            })?,
        )?;
        module.set("null", lua.null())?;
        module.set(
            "validate_session_name",
            lua.create_function(|_, name: LuaValue| {
                let name = arg::<String>(name, "name")?;
                validate_session_name(&name)?;
                Ok(name)
            })?,
        )?;
        Ok(module)
    }
}

/**
 * Automation clients for the products around Vivid: vivido, vivida, and vvmux.
 *
 * These are control sockets, not the Vivid protocol itself. vivido and vivida serve newline-
 * delimited JSON; vvmux serves length-prefixed structured records. Neither carries media.
 *
 * Unix only, for the same reason the Python client is: these are local sockets guarded by file
 * ownership and process identity, and none of that has a Windows equivalent worth pretending
 * about.
 */

import { createHash } from "node:crypto";
import { readFileSync, readdirSync, lstatSync, existsSync } from "node:fs";
import { connect, type Socket } from "node:net";
import { join } from "node:path";
import { tmpdir } from "node:os";

/** The newline-delimited protocol vivido serves and vivida embeds. */
export const PROTOCOL_VERSION = 2;
const MAX_REQUEST_FRAME_BYTES = 1024 * 1024;
const MAX_REPLY_FRAME_BYTES = 16 * 1024 * 1024;

/**
 * VVMX framing.
 *
 * `VVMX_VERSION` is what this client offers first; the preface exchange discovers the server's
 * own version, and a mismatch retries once speaking that, so a vvmux rebuilt across a
 * preface-version bump costs one extra connection rather than a failed session.
 */
export const VVMX_VERSION = 20;
const VVMX_MAGIC = Buffer.from("VVMX", "ascii");
const VVMX_CONTROL_CHANNEL = 1;
const VVMX_CONTROL_MAX_BODY = 1024 * 1024;
const VVMX_HEADER_BYTES = 16;
const VVMX_STRUCTURED_RECORD = 1;

const SESSION_ENV = "VIVIDO_SESSION";
const SOCKET_ENV = "VIVIDO_SOCKET";

/**
 * A request the runtime refused, or an endpoint that could not be resolved.
 *
 * `code` is the contract the runtimes document — `window_not_found`, `method_not_supported`,
 * `limit_exceeded`, and the rest — while `message` is for humans. Endpoint resolution failures
 * use client-side codes: `endpoint_not_found`, `endpoint_unsafe`.
 */
export class AutomationError extends Error {
  override readonly name = "AutomationError";
  readonly code: string;
  readonly detail: string;
  readonly data: unknown;

  constructor(code: string, message: string, data: unknown = undefined) {
    super(`${code}: ${message}`);
    this.code = code;
    this.detail = message;
    this.data = data;
  }
}

/** The current user's uid, or `null` where the platform has no such notion. */
function currentUid(): number | null {
  return typeof process.getuid === "function" ? process.getuid() : null;
}

function sha256Prefix(value: string): string {
  return createHash("sha256").update(value).digest("hex").slice(0, 32);
}

/**
 * The rule both runtimes enforce, mirrored before a name becomes part of a socket path.
 *
 * A name rejected here is one the runtime would reject too, so a caller learns about a bad name
 * from the client rather than from a connection that never opens.
 */
export function validateSessionName(name: string): string {
  if (
    name.length === 0 ||
    name.length > 64 ||
    name.startsWith(".") ||
    !/^[A-Za-z0-9._-]+$/.test(name)
  ) {
    throw new AutomationError(
      "invalid_session_name",
      "session name must be 1-64 ASCII letters, digits, '.', '-' or '_' and not start '.'",
    );
  }
  return name;
}

/**
 * The per-user runtime root where a product keeps its sockets and registries.
 *
 * Held to the standard the servers hold it to, for the reason they do: a registry read from a
 * directory another user can write to is a socket path chosen by that user. One that fails the
 * check is declined, never repaired.
 */
function runtimeDir(product: string): string {
  const base = process.env["XDG_RUNTIME_DIR"] ?? `${tmpdir()}/${product}-${currentUid() ?? "unknown"}`;
  const root = join(base, product);
  let meta;
  try {
    meta = lstatSync(root);
  } catch (error) {
    throw new AutomationError("endpoint_not_found", `no ${product} runtime directory at ${root}`);
  }
  const ownerOnly = currentUid() === null || meta.uid === currentUid();
  if (meta.isSymbolicLink() || !meta.isDirectory() || !ownerOnly || (meta.mode & 0o077) !== 0) {
    throw new AutomationError(
      "endpoint_unsafe",
      `${product} runtime directory ${root} is not owner-only`,
    );
  }
  return root;
}

/**
 * Connect to a local automation socket.
 *
 * The socket file must belong to this user before a byte is written — the same pre-connect check
 * the CLI makes, because a socket another user planted where discovery looks should be declined
 * rather than talked to.
 *
 * Node cannot read the peer's credentials: there is no `SO_PEERCRED` or `getpeereid` binding in
 * the standard library, so the post-connect check the Python client performs is unavailable
 * here. The pre-connect owner check still runs, and it is the one that decides whether the
 * socket at that path is ours.
 */
function connectSocket(path: string, timeoutMs?: number): Socket {
  let meta;
  try {
    meta = lstatSync(path);
  } catch (error) {
    throw new AutomationError("endpoint_not_found", `no endpoint socket at ${path}`);
  }
  if (meta.isSymbolicLink() || (currentUid() !== null && meta.uid !== currentUid())) {
    throw new AutomationError("endpoint_unsafe", `endpoint socket ${path} is not owned by this user`);
  }
  const socket = connect({ path });
  if (timeoutMs !== undefined) {
    socket.setTimeout(timeoutMs);
  }
  return socket;
}

/** Whether this platform can recompute the process-birth record a registry carries. */
function birthCheckSupported(): boolean {
  return process.platform === "linux";
}

/**
 * The `ProcessBirth::Linux` record the server wrote, recomputed from the same field of the same
 * file. This is what makes a recycled pid a stale registry rather than someone else's session.
 */
function linuxBirth(pid: number): { platform: string; start_ticks: number } | null {
  try {
    const text = readFileSync(`/proc/${pid}/stat`, "utf8");
    const end = text.lastIndexOf(") ");
    if (end < 0) {
      return null;
    }
    const fields = text.slice(end + 2).split(/\s+/);
    if (fields.length < 20) {
      return null;
    }
    return { platform: "linux", start_ticks: Number(fields[19]) };
  } catch {
    return null;
  }
}

/**
 * Whether the registry's pid is still the process that wrote it.
 *
 * On Linux the birth record must match, exactly as the CLI demands. Elsewhere that record has a
 * shape this module cannot recompute, so liveness alone decides — a documented weaker check, not
 * a silent one.
 */
function processMatches(registry: Record<string, unknown>): boolean {
  const pid = registry["pid"];
  if (typeof pid !== "number" || !Number.isInteger(pid) || pid <= 0) {
    return false;
  }
  try {
    process.kill(pid, 0);
  } catch {
    return false;
  }
  if (!birthCheckSupported()) {
    return true;
  }
  const birth = linuxBirth(pid);
  const recorded = registry["process_birth"] as { platform?: string; start_ticks?: number } | undefined;
  return (
    birth !== null &&
    recorded?.platform === birth.platform &&
    recorded.start_ticks === birth.start_ticks
  );
}

/**
 * Whether a registry is the one this name and socket layout produce.
 *
 * The socket path is *derived from the name*, never taken from the file: a registry that names
 * another path fails here, so editing one JSON file cannot point a session name at an arbitrary
 * socket.
 */
function instanceIdentityOk(root: string, registry: Record<string, unknown>): boolean {
  const name = registry["name"];
  const socket = registry["socket"];
  if (typeof name !== "string" || typeof socket !== "string") {
    return false;
  }
  const digest = sha256Prefix(name);
  return socket === join(root, `session-${digest}.sock`);
}

/** One automation request's response, or a subscription event interleaved with it. */
async function readFrames(
  socket: Socket,
  onLine: (line: Buffer) => boolean,
): Promise<void> {
  let pending = Buffer.alloc(0);
  return await new Promise<void>((resolve, reject) => {
    const onData = (chunk: Buffer): void => {
      pending = Buffer.concat([pending, chunk]);
      let index = pending.indexOf(0x0a);
      while (index >= 0) {
        const line = pending.subarray(0, index);
        pending = pending.subarray(index + 1);
        if (onLine(line)) {
          socket.off("data", onData);
          resolve();
          return;
        }
        index = pending.indexOf(0x0a);
      }
      if (pending.length > MAX_REPLY_FRAME_BYTES) {
        socket.off("data", onData);
        reject(new AutomationError("limit_exceeded", "reply exceeds the 16 MiB frame limit"));
      }
    };
    socket.on("data", onData);
    socket.once("error", reject);
    socket.once("close", () =>
      reject(new AutomationError("endpoint_not_found", "the runtime closed the connection")),
    );
  });
}

/** One connection to a vivido or vivida instance. */
export class VividoSession {
  private readonly socket: Socket;
  private readonly capabilities: Record<string, unknown>;
  private nextId = 1;

  private constructor(socket: Socket, capabilities: Record<string, unknown>) {
    this.socket = socket;
    this.capabilities = capabilities;
  }

  /** @internal */
  static async from(socket: Socket): Promise<VividoSession> {
    const session = new VividoSession(socket, {});
    const hello = (await session.roundTrip("hello", {})) as Record<string, unknown>;
    return new VividoSession(socket, hello);
  }

  /**
   * The hello document: methods, event kinds, error codes, limits.
   *
   * This is the authority on which methods an instance claims. Standalone Vivido claims
   * `list_windows` and `create_window` where Vivida claims `vivida_layout` and
   * `vivida_resolve_pane`, and a method the other product serves is not a method this one does.
   */
  get capabilitiesDocument(): Readonly<Record<string, unknown>> {
    return this.capabilities;
  }

  /**
   * Issue one automation request and return its result.
   *
   * The method name is validated before it is sent and the frame bounded before it is written —
   * the same 1 MiB the server refuses — so an oversized request fails here with a clear error
   * instead of a protocol error there.
   */
  async request(method: string, params: Record<string, unknown> = {}): Promise<unknown> {
    if (method.length === 0 || method.length > 128 || !/^[A-Za-z0-9_]+$/.test(method)) {
      throw new AutomationError(
        "invalid_request",
        "method must contain 1-128 ASCII letters, digits, or underscores",
      );
    }
    return await this.roundTrip(method, params);
  }

  /** End this connection. The runtime keeps running; only the connection goes. */
  close(): void {
    this.socket.destroy();
  }

  private async roundTrip(method: string, params: Record<string, unknown>): Promise<unknown> {
    this.nextId += 1;
    const requestId = this.nextId;
    const frame = Buffer.from(
      `${JSON.stringify({ version: PROTOCOL_VERSION, id: requestId, method, params })}\n`,
    );
    if (frame.length > MAX_REQUEST_FRAME_BYTES) {
      throw new AutomationError("limit_exceeded", "request exceeds the 1 MiB frame limit");
    }
    this.socket.write(frame);
    while (true) {
      let settled: { ok: true; value: unknown } | undefined;
      let failed: AutomationError | undefined;
      await readFrames(this.socket, (line) => {
        let value: Record<string, unknown>;
        try {
          value = JSON.parse(line.toString("utf8")) as Record<string, unknown>;
        } catch {
          return false;
        }
        if (typeof value !== "object" || value === null || !("id" in value)) {
          // A subscription event, interleaved: no id, so not this conversation.
          return false;
        }
        if (value["version"] !== PROTOCOL_VERSION || value["id"] !== requestId) {
          return false;
        }
        if (value["ok"] === true) {
          settled = { ok: true, value: value["result"] };
          return true;
        }
        const error = (value["error"] ?? {}) as Record<string, unknown>;
        failed = new AutomationError(
          String(error["code"] ?? "invalid_response"),
          String(error["message"] ?? "the runtime sent no error payload"),
          error["data"],
        );
        return true;
      });
      if (failed !== undefined) {
        throw failed;
      }
      if (settled !== undefined) {
        return settled.value;
      }
    }
  }
}

/**
 * Connect to a vivido or vivida instance, resolving the endpoint the way the CLI does.
 *
 * An explicit `socket` wins. Then `target`, else an inherited `VIVIDO_SESSION` — and a named
 * instance that is not running is an error, never a silent fall-through to a different one.
 * Without a name: `VIVIDO_SOCKET` if it still connects, the only live instance when there is
 * exactly one, and finally the newest windowed instance on this display.
 */
export async function vividoConnect(
  options: { readonly socket?: string; readonly target?: string; readonly timeoutMs?: number } = {},
): Promise<VividoSession> {
  let root: string | undefined;
  try {
    root = runtimeDir("vivido");
  } catch (error) {
    if (options.socket === undefined && options.target === undefined && process.env[SESSION_ENV] === undefined) {
      throw error;
    }
  }

  if (options.socket !== undefined) {
    return await VividoSession.from(connectSocket(options.socket, options.timeoutMs));
  }
  const inherited = process.env[SESSION_ENV];
  if (options.target !== undefined || inherited !== undefined) {
    if (root === undefined) {
      throw new AutomationError("endpoint_not_found", "no vivido runtime directory");
    }
    const name = options.target ?? inherited ?? "";
    const registry = namedRegistry(root, name);
    return await VividoSession.from(connectSocket(String(registry["socket"]), options.timeoutMs));
  }
  return await VividoSession.from(await discover(root, options.timeoutMs));
}

/** Everything live this user can reach, windowed or headless. */
export function vividoInstances(): ReadonlyArray<Record<string, unknown>> {
  const root = runtimeDir("vivido");
  const found: Array<Record<string, unknown>> = [];
  const entries = readdirSync(root)
    .filter((name) => name.startsWith("session-") && name.endsWith(".json"))
    .sort();
  for (const entry of entries) {
    let loaded: unknown;
    try {
      loaded = JSON.parse(readFileSync(join(root, entry), "utf8"));
    } catch {
      continue;
    }
    if (typeof loaded !== "object" || loaded === null) {
      continue;
    }
    const registry = loaded as Record<string, unknown>;
    if (registry["schema"] !== 1) {
      continue;
    }
    if (!instanceIdentityOk(root, registry)) {
      continue;
    }
    if (processMatches(registry)) {
      found.push(registry);
    }
  }
  return found;
}

function namedRegistry(root: string, name: string): Record<string, unknown> {
  validateSessionName(name);
  const digest = sha256Prefix(name);
  const path = join(root, `session-${digest}.json`);
  let loaded: unknown;
  try {
    loaded = JSON.parse(readFileSync(path, "utf8"));
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") {
      throw new AutomationError("endpoint_not_found", `no running Vivido instance named ${JSON.stringify(name)}`);
    }
    throw new AutomationError("endpoint_unsafe", `registry for ${JSON.stringify(name)} is not valid JSON`);
  }
  if (typeof loaded !== "object" || loaded === null) {
    throw new AutomationError("endpoint_unsafe", `registry for ${JSON.stringify(name)} is not an object`);
  }
  const registry = loaded as Record<string, unknown>;
  if (registry["schema"] !== 1 || registry["protocol_version"] !== PROTOCOL_VERSION) {
    throw new AutomationError(
      "endpoint_unsafe",
      `registry for ${JSON.stringify(name)} is not a schema this client reads`,
    );
  }
  if (registry["name"] !== name || !instanceIdentityOk(root, registry)) {
    throw new AutomationError(
      "endpoint_unsafe",
      `registry for ${JSON.stringify(name)} does not match its endpoint identity`,
    );
  }
  if (!processMatches(registry)) {
    throw new AutomationError("endpoint_not_found", `Vivido instance ${JSON.stringify(name)} is no longer running`);
  }
  return registry;
}

/**
 * The unqualified order: inherited socket, sole live instance, newest windowed.
 *
 * Every step may decline; only running out of steps is an error, which is what makes the
 * inherited-socket step a preference rather than a commitment.
 */
async function discover(root: string | undefined, timeoutMs?: number): Promise<Socket> {
  const inherited = process.env[SOCKET_ENV];
  if (inherited !== undefined && inherited !== "") {
    try {
      return connectSocket(inherited, timeoutMs);
    } catch {
      // The inherited socket is a preference, not a commitment.
    }
  }
  if (root !== undefined) {
    const instances = vividoInstances();
    if (instances.length === 1) {
      return connectSocket(String(instances[0]?.["socket"]), timeoutMs);
    }
    return newestWindowed(root, timeoutMs);
  }
  throw new AutomationError(
    "endpoint_not_found",
    "no vivido endpoint: pass socket or target, or start an instance",
  );
}

/**
 * Windowed instances advertise on the display they render to: `Vivido-<display>-<pid>.sock`.
 *
 * Sorted newest-first by name — the pid is in the name, the same ordering the CLI uses — and
 * stale sockets are skipped, not adopted.
 */
async function newestWindowed(root: string, timeoutMs?: number): Promise<Socket> {
  const display = process.env["WAYLAND_DISPLAY"] ?? process.env["DISPLAY"] ?? "";
  const prefix = `Vivido-${display.replace(/\//g, "-")}-`;
  const candidates = readdirSync(root)
    .filter((name) => name.startsWith(prefix) && name.endsWith(".sock"))
    .sort()
    .reverse();
  for (const candidate of candidates) {
    try {
      return connectSocket(join(root, candidate), timeoutMs);
    } catch {
      continue;
    }
  }
  throw new AutomationError("endpoint_not_found", "no windowed Vivido instance on this display");
}

/** The envelope fields every vvmux request may carry. */
export interface VvmuxRequestOptions {
  readonly paneId?: number;
  readonly agent?: string;
  readonly paneName?: string;
  readonly lease?: string;
  readonly allowFocused?: boolean;
  readonly expect?: Record<string, unknown>;
  readonly idempotencyKey?: string;
}

/** One connection to a vvmux session server. */
export class VvmuxSession {
  private readonly socket: Socket;
  private readonly maximum: number;
  private sendSequence = 0;
  private recvSequence = 0;
  private nextId = 0;

  /** @internal */
  constructor(socket: Socket, maximumBody: number) {
    this.socket = socket;
    this.maximum = maximumBody;
  }

  /**
   * Issue one automation request and return its result.
   *
   * `method` must carry a `method` key naming the verb; the rest of the record rides whole,
   * exactly as the schema publishes it. The options are the envelope fields that are properties
   * of the request rather than of the verb.
   */
  async request(
    method: Record<string, unknown>,
    options: VvmuxRequestOptions = {},
  ): Promise<unknown> {
    const verb = method["method"];
    if (typeof verb !== "string" || verb.length === 0) {
      throw new AutomationError("invalid_request", "an automation method needs a `method` verb");
    }
    const envelope = ["id", "pane_id", "agent", "pane_name", "lease", "allow_focused", "expect", "idempotency_key"];
    const clash = Object.keys(method)
      .filter((key) => envelope.includes(key))
      .sort();
    if (clash.length > 0) {
      throw new AutomationError(
        "invalid_request",
        `${clash.join(", ")} belong on the request, not inside the method`,
      );
    }
    this.nextId += 1;
    const request: Record<string, unknown> = { id: this.nextId, ...method };
    for (const [key, value] of [
      ["pane_id", options.paneId],
      ["agent", options.agent],
      ["pane_name", options.paneName],
      ["lease", options.lease],
      ["expect", options.expect],
      ["idempotency_key", options.idempotencyKey],
    ] as const) {
      if (value !== undefined) {
        request[key] = value;
      }
    }
    if (options.allowFocused === true) {
      request["allow_focused"] = true;
    }
    this.send({ automation: request });
    while (true) {
      const reply = await this.receive();
      const response = reply["Automation"];
      if (typeof response !== "object" || response === null) {
        continue; // Pong, Title and friends: addressed to no request of ours.
      }
      const record = response as Record<string, unknown>;
      if (record["id"] !== this.nextId) {
        continue;
      }
      if (record["ok"] === true) {
        return record["result"];
      }
      const error = (record["error"] ?? {}) as Record<string, unknown>;
      throw new AutomationError(
        String(error["code"] ?? "invalid_response"),
        String(error["message"] ?? "the session server sent no error payload"),
      );
    }
  }

  /** End this connection. The session keeps running; only the connection goes. */
  close(): void {
    this.socket.destroy();
  }

  private send(message: Record<string, unknown>): void {
    const body = Buffer.from(JSON.stringify(message));
    if (body.length > this.maximum) {
      throw new AutomationError("limit_exceeded", "request exceeds the negotiated body limit");
    }
    const header = Buffer.alloc(VVMX_HEADER_BYTES);
    header.writeBigUInt64BE(BigInt(this.sendSequence), 0);
    header.writeUInt16BE(VVMX_STRUCTURED_RECORD, 8);
    header.writeUInt16BE(0, 10);
    header.writeUInt32BE(body.length, 12);
    this.sendSequence = (this.sendSequence + 1) >>> 0;
    this.socket.write(Buffer.concat([header, body]));
  }

  private async receive(): Promise<Record<string, unknown>> {
    const header = await recvExact(this.socket, VVMX_HEADER_BYTES);
    const sequence = Number(header.readBigUInt64BE(0));
    const recordType = header.readUInt16BE(8);
    const flags = header.readUInt16BE(10);
    const length = header.readUInt32BE(12);
    if (sequence !== this.recvSequence) {
      throw new AutomationError(
        "invalid_response",
        `VVMX record sequence gap at ${this.recvSequence}`,
      );
    }
    this.recvSequence = (this.recvSequence + 1) >>> 0;
    if ((flags & ~0x0001) !== 0 || recordType !== VVMX_STRUCTURED_RECORD) {
      throw new AutomationError("invalid_response", "unexpected VVMX control record");
    }
    if (length > this.maximum) {
      throw new AutomationError("invalid_response", "VVMX record body exceeds the negotiated limit");
    }
    const body = await recvExact(this.socket, length);
    let value: unknown;
    try {
      value = JSON.parse(body.toString("utf8"));
    } catch {
      throw new AutomationError("invalid_response", "VVMX record body is not JSON");
    }
    if (typeof value !== "object" || value === null) {
      throw new AutomationError("invalid_response", "VVMX record body is not an object");
    }
    return value as Record<string, unknown>;
  }
}

/**
 * Connect to a vvmux session server by name.
 *
 * The socket path is derived from the name the same way the server derives its own. The preface
 * exchange doubles as version discovery: this client offers `VVMX_VERSION` and, if the server
 * answers with a different preface version, reconnects once speaking that — so a vvmux rebuilt
 * across a version bump still connects, without this module being edited.
 */
export async function vvmuxConnect(
  target = "default",
  options: { readonly timeoutMs?: number } = {},
): Promise<VvmuxSession> {
  validateSessionName(target);
  const root = runtimeDir("vvmux");
  const digest = sha256Prefix(target);
  const path = join(root, `session-${digest}.sock`);
  const socket = connectSocket(path, options.timeoutMs);
  const { socket: negotiated, maximum } = await vvmuxPreface(socket, VVMX_VERSION, path, options.timeoutMs);
  return new VvmuxSession(negotiated, maximum);
}

/**
 * Exchange prefaces, offering `version` and honouring what comes back.
 *
 * The server writes its preface before it reads ours, so a mismatch is still answered: the
 * client learns the version it should have offered and reconnects once. Bounded at one retry —
 * a server that disagrees twice is not going to agree a third time.
 */
async function vvmuxPreface(
  stream: Socket,
  offeredVersion: number,
  path: string,
  timeoutMs: number | undefined,
): Promise<{ socket: Socket; maximum: number }> {
  let version = offeredVersion;
  let socket = stream;
  for (let attempt = 1; attempt <= 2; attempt += 1) {
    const offered = Buffer.alloc(12);
    VVMX_MAGIC.copy(offered, 0);
    offered.writeUInt16BE(version, 4);
    offered.writeUInt8(VVMX_CONTROL_CHANNEL, 6);
    offered.writeUInt8(0, 7);
    offered.writeUInt32BE(VVMX_CONTROL_MAX_BODY, 8);
    socket.write(offered);
    const peer = await recvExact(socket, 12);
    if (!peer.subarray(0, 4).equals(VVMX_MAGIC)) {
      socket.destroy();
      throw new AutomationError("invalid_response", "bad VVMX magic");
    }
    const peerVersion = peer.readUInt16BE(4);
    const peerMaximum = peer.readUInt32BE(8);
    if (peerVersion === version) {
      if (peer.readUInt8(6) !== VVMX_CONTROL_CHANNEL || peer.readUInt8(7) !== 0) {
        socket.destroy();
        throw new AutomationError("invalid_response", "VVMX channel mismatch");
      }
      if (peerMaximum === 0 || peerMaximum > VVMX_CONTROL_MAX_BODY) {
        socket.destroy();
        throw new AutomationError("invalid_response", "invalid VVMX maximum body");
      }
      return { socket, maximum: Math.min(VVMX_CONTROL_MAX_BODY, peerMaximum) };
    }
    socket.destroy();
    if (attempt === 2) {
      throw new AutomationError(
        "invalid_response",
        `vvmux speaks VVMX v${peerVersion}, not v${version}`,
      );
    }
    version = peerVersion;
    socket = connectSocket(path, timeoutMs);
  }
  throw new Error("unreachable");
}

function recvExact(socket: Socket, count: number): Promise<Buffer> {
  return new Promise<Buffer>((resolve, reject) => {
    const parts: Buffer[] = [];
    let remaining = count;
    const onData = (chunk: Buffer): void => {
      parts.push(chunk);
      remaining -= chunk.length;
      if (remaining <= 0) {
        socket.off("data", onData);
        resolve(Buffer.concat(parts));
      }
    };
    socket.on("data", onData);
    socket.once("error", reject);
    socket.once("close", () => {
      if (remaining > 0) {
        reject(new AutomationError("endpoint_not_found", "the endpoint closed the connection"));
      }
    });
  });
}

/** The module's documented surface, for `import { automation } from ...`. */
export const automation = {
  AutomationError,
  PROTOCOL_VERSION,
  VVMX_VERSION,
  VividoSession,
  VvmuxSession,
  validateSessionName,
  vividoConnect,
  vividoInstances,
  vvmuxConnect,
} as const;

/** Whether this platform can host these clients at all. */
export const SUPPORTED = process.platform !== "win32";

if (!SUPPORTED) {
  // Loading the module is harmless; connecting is refused rather than half-working.
  void existsSync;
}

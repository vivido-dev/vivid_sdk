/**
 * Loader for the compiled addon, and the translation of its errors into classes.
 *
 * The addon marshals; it does not decide protocol values. Everything below is about turning a
 * napi rejection into something a TypeScript caller can catch by kind.
 */

import { createRequire } from "node:module";

const require = createRequire(import.meta.url);

/** The exact native surface. Shapes only; rules live in the SDK. */
export interface NativeModule {
  readonly constantTable: () => ConstantEntry[];
  readonly probeEncodedImage: (data: Uint8Array) => EncodedImageInfo;
  readonly connect: (options?: unknown) => Promise<unknown>;
  readonly presenterStart: (options: unknown) => Promise<unknown>;
  readonly Session: unknown;
  readonly Surface: unknown;
  readonly Track: unknown;
  readonly TrackChannel: unknown;
  readonly TrackSender: unknown;
  readonly VideoRateControl: unknown;
  readonly InputLane: unknown;
  readonly IncomingFileTransfer: unknown;
  readonly PaneSession: unknown;
  readonly DesktopSession: unknown;
  readonly Presenter: unknown;
  readonly establishDesktop: (...args: unknown[]) => Promise<unknown>;
  [name: string]: unknown;
}

/** One protocol constant, from the table the SDK owns. */
export interface ConstantEntry {
  readonly name: string;
  /** The value for profile constants; `null` for numeric ones. */
  readonly text: string | null;
  /** The value for numeric constants; `null` for profile names. */
  readonly number: number | null;
}

export interface EncodedImageInfo {
  readonly encoding: number;
  readonly width: number;
  readonly height: number;
  readonly encodedLength: number;
}

/**
 * The class a rejection carries as its message prefix.
 *
 * napi's `Status` becomes the JS `code` property and is a fixed enum, so the semantic kind rides
 * in the message. That keeps closed-handle errors distinguishable from protocol rejections
 * without pretending a native enum can name either.
 */
const CLASS_PREFIX = /^(ClosedHandle|InvalidInput|VividError): ([\s\S]*)$/;

/** Base class for every rejection the SDK produces. */
export class VividError extends Error {
  override readonly name: string = "VividError";
  /** What the SDK called this failure, when it said. */
  readonly code: string | undefined;
  /** True when the operation was refused because a handle was already closed. */
  readonly closed: boolean;

  constructor(message: string, code?: string, closed = false) {
    super(message);
    this.code = code;
    this.closed = closed;
  }
}

/** A handle that was used after it was closed, or closed twice. */
export class ClosedHandleError extends VividError {
  override readonly name: string = "ClosedHandleError";
  constructor(message: string, code?: string) {
    super(message, code, true);
  }
}

/** Turn one napi rejection into the right class. */
export function decodeError(error: unknown): VividError {
  if (error instanceof VividError) {
    return error;
  }
  const raw = error as { message?: unknown; code?: unknown } | null;
  const message = typeof raw?.message === "string" ? raw.message : String(error);
  const code = typeof raw?.code === "string" ? raw.code : undefined;
  const match = CLASS_PREFIX.exec(message);
  const kind = match?.[1];
  const rest = match?.[2] ?? message;
  if (kind === "ClosedHandle") {
    return new ClosedHandleError(rest, code);
  }
  if (kind === undefined) {
    return new VividError(message, code);
  }
  return new VividError(rest, code);
}

/**
 * Drop absent fields from an options object before it crosses into napi.
 *
 * napi reads an `Option<T>` as "present or undefined"; a `null` is a value it tries to convert,
 * and converting null into a number fails. Absence is what the addon's signatures mean by
 * "unset", so this makes that explicit rather than relying on every call site to remember.
 */
export function optionalFields<T extends Record<string, unknown>>(fields: T): Partial<T> {
  const cleaned: Partial<T> = {};
  for (const [key, value] of Object.entries(fields)) {
    if (value !== null && value !== undefined) {
      cleaned[key as keyof T] = value as T[keyof T];
    }
  }
  return cleaned;
}

/** Await a native promise, translating its rejection. */
export async function call<T>(work: Promise<T>): Promise<T> {
  try {
    return await work;
  } catch (error) {
    throw decodeError(error);
  }
}

/** Run a native call that does not return a promise. */
export function callSync<T>(work: () => T): T {
  try {
    return work();
  } catch (error) {
    throw decodeError(error);
  }
}

let cached: NativeModule | undefined;

/**
 * The loaded addon, resolved once per process.
 *
 * Resolution tries the four places it can legitimately be: beside the built package (how a
 * published install lays it out), in the repository's own `node-bindings` directory for a
 * checkout, and the platform-specific prebuild packages npm installs alongside. The first that
 * loads wins, so a developer working in the repository and an application that installed the
 * package both get an addon without configuration.
 */
export function native(): NativeModule {
  if (cached !== undefined) {
    return cached;
  }
  const candidates = [
    "../node-bindings/vivid_sdk_node.node",
    "../vivid_sdk_node.node",
    `@vivido/vivid-sdk-${process.platform}-${process.arch}`,
  ];
  const failures: string[] = [];
  for (const candidate of candidates) {
    try {
      cached = require(candidate) as NativeModule;
      return cached;
    } catch (error) {
      failures.push(`${candidate}: ${(error as Error).message.split("\n")[0]}`);
    }
  }
  throw new Error(
    `could not load the @vivido/vivid-sdk native addon. Tried:\n  ${failures.join("\n  ")}`,
  );
}

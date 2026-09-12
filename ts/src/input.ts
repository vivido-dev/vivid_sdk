/**
 * Desktop input lanes: ordinary input over an authenticated interactive connection.
 *
 * A lane is a separate authenticated connection from control and media, so a stalled or
 * malformed media source cannot delay input and a revoked grant cannot hold up anything else.
 * Nothing here injects input; a lane only reports what arrived.
 */

import { call, callSync, native } from "./native.js";
import type { Session } from "./index.js";
import type { NativeInputLane } from "./native-types.js";
import { MAX_WATCHDOG_US } from "./constants.js";

/** How long a lane wait blocks before returning nothing. */
export const DEFAULT_LANE_WAIT_MS = 1000;

/** The presenter's answer to a binding request. */
export interface InputBindingStatus {
  readonly producerEpoch: number;
  readonly grantGeneration: number;
  readonly contextId: number;
  readonly surfaceId: number;
  readonly surfaceGeneration: number;
  readonly effectiveClasses: number;
  readonly state: number;
  readonly reason: number;
  readonly watchdogTimeoutUs: number;
}

/** One lane event, flattened by `kind`. */
export interface InputLaneEvent {
  readonly kind: "input" | "renew" | "revoked" | "reset" | "lane_closed" | "error";
  readonly recordType?: number;
  readonly surfaceId?: number;
  /** The presenter's exact payload, still generation-qualified. */
  readonly payload?: unknown;
  readonly producerEpoch?: number;
  readonly grantGeneration?: number;
  readonly contextId?: number;
  readonly surfaceGeneration?: number;
  readonly renewalSequence?: number;
  readonly watchdogTimeoutUs?: number;
  readonly reason?: number;
  readonly diagnostic?: string;
  readonly message?: string;
}

/** A desktop-input lane. */
export class InputLane {
  readonly raw: NativeInputLane;

  private constructor(raw: NativeInputLane) {
    this.raw = raw;
  }

  /** @internal */
  static wrap(raw: NativeInputLane): InputLane {
    return new InputLane(raw);
  }

  get generation(): number {
    return callSync(() => this.raw.generation) as number;
  }

  get closed(): boolean {
    return callSync(() => this.raw.closed) as boolean;
  }

  /**
   * Bind the lane to a surface's input classes.
   *
   * A zero context and surface disable injection; anything else asks for exactly the classes
   * named. The presenter answers with the classes it actually granted, which is what a host's
   * injection gate must be driven from.
   */
  async setBinding(binding: {
    readonly producerEpoch: number;
    readonly contextId: number;
    readonly surfaceId: number;
    readonly surfaceGeneration: number;
    readonly requestedClasses: number;
    readonly reason?: number;
    readonly requestedWatchdogUs?: number;
  }): Promise<InputBindingStatus> {
    return (await call(this.raw.setBinding(binding))) as InputBindingStatus;
  }

  takeEvent(): InputLaneEvent | null {
    return callSync(() => this.raw.takeEvent()) as InputLaneEvent | null;
  }

  /**
   * The next lane event, waiting up to `timeoutMs`.
   *
   * Pointer motion arrives far faster than any sensible poll interval, and a slow reader is what
   * overruns the lane's queue bound, so drive a lane from here rather than from `takeEvent`.
   */
  async waitEvent(timeoutMs = DEFAULT_LANE_WAIT_MS): Promise<InputLaneEvent | null> {
    return (await call(this.raw.waitEvent(timeoutMs))) as InputLaneEvent | null;
  }

  /** Iterate lane events until it closes or the signal aborts. */
  async *events(
    options: { readonly timeoutMs?: number; readonly signal?: AbortSignal } = {},
  ): AsyncGenerator<InputLaneEvent> {
    const timeoutMs = options.timeoutMs ?? DEFAULT_LANE_WAIT_MS;
    while (options.signal?.aborted !== true) {
      const event = await this.waitEvent(timeoutMs);
      if (event === null || event.kind === "lane_closed") {
        return;
      }
      yield event;
    }
  }

  /** Close the lane. The session stays usable. */
  async close(): Promise<void> {
    await call(this.raw.close());
  }

  async [Symbol.asyncDispose](): Promise<void> {
    await this.close();
  }
}

/** Open a desktop-input lane. Requires `desktop-input-v1`. */
export async function openLane(session: Session, laneGeneration = 1): Promise<InputLane> {
  return InputLane.wrap(await call(session.raw.openInputLane(laneGeneration)));
}

/** The protocol's ceiling on a requested watchdog window, for reference by callers. */
export const MAX_REQUESTED_WATCHDOG_US = MAX_WATCHDOG_US;

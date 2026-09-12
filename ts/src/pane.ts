/**
 * One image in one terminal pane, over the SDK's own pane state machine.
 *
 * The node and surface lifecycle, the fixed-point cell geometry, and the 80x24 defaults all live
 * in the SDK; this class adds the connect options and `using`, and nothing else.
 */

import { call, native } from "./native.js";
import type { ConnectOptions, Session } from "./index.js";
import { Session as SessionHandle } from "./index.js";
import type { NativePaneSession } from "./native-types.js";

/** Placement and descriptor options for one pane image. */
export interface PaneImageOptions {
  readonly title?: string;
  readonly columns?: number;
  readonly rows?: number;
  readonly textLayer?: number;
}

/**
 * A producer session that owns at most one pane-scoped image presentation.
 *
 * Creating a new presentation clears the previous node and surface. Discovery and authentication
 * come from the standard environment, consumed on the Rust side, so no capability material is
 * retained here.
 */
export class PaneSession {
  readonly raw: NativePaneSession;

  private constructor(raw: NativePaneSession) {
    this.raw = raw;
  }

  /** Connect through the standard discovery environment. */
  static async connect(options: ConnectOptions = {}): Promise<PaneSession> {
    return PaneSession.adopt(await SessionHandle.connect(options));
  }

  /**
   * Adopt an established session, which the pane then owns.
   *
   * Asynchronous because the native factory has to take the session out of its handle, and a
   * handle that has been adopted must not be used again — the session it named now belongs to
   * the pane.
   */
  static async adopt(session: Session): Promise<PaneSession> {
    const factory = native().PaneSession as {
      fromSession: (session: unknown) => Promise<NativePaneSession>;
    };
    return new PaneSession(await call(factory.fromSession(session.raw)));
  }

  get closed(): boolean {
    return this.raw.closed;
  }

  /** Whether a presentation is currently retained. */
  get hasPresentation(): boolean {
    return this.raw.hasPresentation;
  }

  /** Present one complete PNG or JPEG, replacing any current presentation. */
  async showEncodedImage(encoded: Uint8Array, options: PaneImageOptions = {}): Promise<void> {
    await call(
      this.raw.showEncodedImage(
        Buffer.from(encoded),
        options.title ?? undefined,
        options.columns ?? undefined,
        options.rows ?? undefined,
        options.textLayer ?? undefined,
      ),
    );
  }

  /** Present one tightly packed sRGB RGBA8 frame, replacing any current presentation. */
  async showRgba(
    width: number,
    height: number,
    rgba: Uint8Array,
    options: PaneImageOptions = {},
  ): Promise<void> {
    await call(
      this.raw.showRgba(
        width,
        height,
        Buffer.from(rgba),
        options.title ?? undefined,
        options.columns ?? undefined,
        options.rows ?? undefined,
        options.textLayer ?? undefined,
      ),
    );
  }

  /** Remove the current presentation; idempotent. */
  async clear(): Promise<void> {
    await call(this.raw.clear());
  }

  async close(): Promise<void> {
    await call(this.raw.close());
  }

  /** Clear and close when the block exits, so `await using` leaves nothing behind. */
  async [Symbol.asyncDispose](): Promise<void> {
    await this.clear();
    await this.close();
  }
}

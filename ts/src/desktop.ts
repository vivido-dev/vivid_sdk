/**
 * Desktop presentation orchestration.
 *
 * Establishing a desktop presentation is one shape every time: a desktop surface carrying typed
 * parameters, a full-target node, a video track with its sender, optional audio, and an input
 * lane when the presenter accepts injection. `DesktopSession` owns that shape, and the session
 * it takes.
 */

import { call, native } from "./native.js";
import type { Session, SurfaceConfig, TrackConfig } from "./index.js";
import { Track } from "./index.js";
import type { NativeDesktopSession } from "./native-types.js";

/** A desktop presentation: its surface, node, senders, and input lane. */
export class DesktopSession {
  readonly raw: NativeDesktopSession;

  private constructor(raw: NativeDesktopSession) {
    this.raw = raw;
  }

  /** @internal */
  static wrap(raw: NativeDesktopSession): DesktopSession {
    return new DesktopSession(raw);
  }

  get closed(): boolean {
    return this.raw.closed;
  }

  /** The video track handle, for milestone waits and queries. */
  async videoTrack(): Promise<Track> {
    return Track.wrap(await call(this.raw.videoTrack()));
  }

  /** The audio track handle, when audio was established. */
  async audioTrack(): Promise<Track | null> {
    const track = await call(this.raw.audioTrack());
    return track === null ? null : Track.wrap(track);
  }

  /** Send one video access unit; packet ID and epoch default to the sender's continuity. */
  async sendVideo(
    data: Uint8Array,
    options: {
      readonly ptsUs: number;
      readonly packetId?: number;
      readonly dtsUs?: number;
      readonly durationUs?: number;
      readonly key?: boolean;
      readonly epoch?: number;
    },
  ): Promise<number> {
    return (await call(
      this.raw.sendVideo({
        data: Buffer.from(data),
        packetId: options.packetId ?? undefined,
        ptsUs: options.ptsUs,
        dtsUs: options.dtsUs ?? undefined,
        durationUs: options.durationUs ?? undefined,
        key: options.key ?? undefined,
        epoch: options.epoch ?? undefined,
      }),
    )) as number;
  }

  /** Send one audio access unit through the audio sender. */
  async sendAudio(
    data: Uint8Array,
    options: { readonly ptsUs: number; readonly durationUs: number; readonly packetId: number },
  ): Promise<number> {
    return (await call(
      this.raw.sendAudio({
        data: Buffer.from(data),
        packetId: options.packetId,
        ptsUs: options.ptsUs,
        durationUs: options.durationUs,
      }),
    )) as number;
  }

  /** Wait for decoded-output readiness and activate the video and audio slots atomically. */
  async activateSlots(): Promise<void> {
    await call(this.raw.activateSlots());
  }

  async close(): Promise<void> {
    await call(this.raw.close());
  }

  async [Symbol.asyncDispose](): Promise<void> {
    await this.close();
  }
}

/**
 * Establish a desktop presentation, taking ownership of `session`.
 *
 * The session is consumed because the orchestrator holds it for its whole life; keeping it live
 * would let a caller close it out from under the senders.
 */
export async function establishDesktop(
  session: Session,
  surface: SurfaceConfig,
  video: TrackConfig,
  audio?: TrackConfig,
): Promise<DesktopSession> {
  const established = await call(
    native().establishDesktop(session.raw, surface, video, audio ?? undefined),
  );
  return DesktopSession.wrap(established as NativeDesktopSession);
}

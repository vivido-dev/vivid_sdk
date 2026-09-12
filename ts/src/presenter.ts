/**
 * The presenter role: terminate a Vivid session and present what it carries.
 *
 * The presenter's own threads are pure Rust and never enter JavaScript, which is why every verb
 * here is either a synchronous handle read or an async call onto the worker pool. Pane reads are
 * pull-based — `capturePane` and a bounded `waitForMedia` — rather than an event stream, so a
 * presenter never has to hold a callback into a host scheduler.
 */

import { call, callSync, native } from "./native.js";
import type { NativePresenter } from "./native-types.js";

/** One source identity: every field is part of it, none is optional. */
export interface SourceKey {
  readonly producer: number;
  readonly context: number;
  readonly surface: number;
  readonly track: number;
}

export interface ClipRect {
  readonly x: number;
  readonly y: number;
  readonly width: number;
  readonly height: number;
}

/** One node's retained pixels and the rectangle they occupy. */
export interface CaptureLayer {
  readonly source: SourceKey;
  readonly nodeId: number;
  readonly zIndex: number;
  readonly x: number;
  readonly y: number;
  readonly width: number;
  readonly height: number;
  readonly clip: ClipRect | null;
  /** `raster` for decoded pixels, `encodedImage` for the producer's original bytes. */
  readonly contentKind: "raster" | "encodedImage";
  readonly epoch: number | null;
  readonly frameId: number | null;
  readonly rasterWidth: number | null;
  readonly rasterHeight: number | null;
  readonly pixels: Uint8Array | null;
  readonly encodedImage: Uint8Array | null;
}

/** A visual source that contributed nothing, and why. */
export interface SkippedSource {
  readonly source: SourceKey;
  readonly nodeId: number;
  /** `undecoded_video`, `no_retained_pixels`, or `node_hidden`. */
  readonly reason: string;
}

/**
 * A read-only read of a pane's media.
 *
 * This is not a screenshot: it composes the retained pixels the presenter holds, and it says in
 * `skipped` which sources contributed nothing and why. A blank capture that explains itself is a
 * fact; a blank capture that says nothing reads as a broken request.
 */
export interface PaneCapture {
  readonly layers: readonly CaptureLayer[];
  readonly skipped: readonly SkippedSource[];
}

export interface PaneTrackSummary {
  readonly source: SourceKey;
  readonly kind: string;
  readonly capturable: boolean;
}

export interface PaneMediaSummary {
  readonly surfaces: readonly string[];
  readonly tracks: readonly PaneTrackSummary[];
}

export interface PresenterOptions {
  /** `unix:/absolute/path` or `tcp:host:port`, loopback only; port 0 is ephemeral. */
  readonly endpoint: string;
  /** Present a desktop target of this size instead of a terminal target. */
  readonly desktopWidth?: number;
  readonly desktopHeight?: number;
  readonly aggregateRetainedBytes?: number;
}

/** One media resource: a stable name for content this presenter holds. */
export interface MediaResourceInfo {
  readonly bindingPinned: boolean;
  readonly producer: number;
  readonly context: number;
  readonly surface: number;
  readonly surfaceRevision: number;
  readonly surfaceGeneration: number;
  readonly trackId: number | null;
  readonly trackRevision: number | null;
  readonly trackChannelGeneration: number | null;
  readonly trackMediaEpoch: number | null;
  readonly capturable: boolean | null;
}

/** A running presenter. */
export class Presenter {
  readonly raw: NativePresenter;

  private constructor(raw: NativePresenter) {
    this.raw = raw;
  }

  /** Start a presenter. */
  static async start(options: PresenterOptions): Promise<Presenter> {
    const started = await call(
      native().presenterStart({
        endpoint: options.endpoint,
        desktopWidth: options.desktopWidth ?? undefined,
        desktopHeight: options.desktopHeight ?? undefined,
        aggregateRetainedBytes: options.aggregateRetainedBytes ?? undefined,
      }),
    );
    return new Presenter(started as NativePresenter);
  }

  get closed(): boolean {
    return callSync(() => this.raw.closed) as boolean;
  }

  /** The endpoint this presenter is reachable at, as bound (a port-0 request resolves here). */
  endpoint(): string {
    return callSync(() => this.raw.endpoint()) as string;
  }

  /**
   * Mint a pane capability.
   *
   * The secret is returned once and never appears in a repr or an error; it is the whole
   * capability for the pane it names.
   */
  issuePaneCapability(pane: number): string {
    return callSync(() => this.raw.issuePaneCapability(pane)) as string;
  }

  revokePane(pane: number): void {
    callSync(() => this.raw.revokePane(pane));
  }

  updateMetrics(
    pane: number,
    columns: number,
    rows: number,
    cellWidth: number,
    cellHeight: number,
  ): void {
    callSync(() => this.raw.updateMetrics(pane, columns, rows, cellWidth, cellHeight));
  }

  /** Wait for retained media to arrive, bounded. */
  async waitForMedia(pane: number, timeoutMs: number): Promise<boolean> {
    return (await call(this.raw.waitForMedia(pane, timeoutMs))) as boolean;
  }

  async capturePane(pane: number, viewportOffset = 0): Promise<PaneCapture> {
    return (await call(this.raw.capturePane(pane, viewportOffset))) as PaneCapture;
  }

  async paneMediaSummary(pane: number): Promise<PaneMediaSummary> {
    return (await call(this.raw.paneMediaSummary(pane))) as PaneMediaSummary;
  }

  /** Deliver one microphone packet from the pane app; empty bytes revoke the microphone. */
  async queueMicrophone(
    source: SourceKey,
    generation: number,
    bytes: Uint8Array,
  ): Promise<boolean> {
    return (await call(
      this.raw.queueMicrophone(source, generation, Buffer.from(bytes)),
    )) as boolean;
  }

  revokeMicrophones(): void {
    callSync(() => this.raw.revokeMicrophones());
  }

  /** Tell the pane app the capability set changed; returns the new generation. */
  notifyCapabilitiesChanged(reasonMask: number): number {
    return callSync(() => this.raw.notifyCapabilitiesChanged(reasonMask)) as number;
  }

  updateDesktopTarget(
    pane: number,
    width: number,
    height: number,
    reasonMask: number,
  ): number {
    return callSync(() =>
      this.raw.updateDesktopTarget(pane, width, height, reasonMask),
    ) as number;
  }

  /** Feed a marker the terminal printed, so the presenter can place the anchor. */
  observeMarker(
    pane: number,
    value: string,
    row: number,
    column: number,
    alternate: boolean,
  ): void {
    callSync(() => this.raw.observeMarker(pane, value, row, column, alternate));
  }

  scrollAnchors(pane: number, lines: number, alternate: boolean): void {
    callSync(() => this.raw.scrollAnchors(pane, lines, alternate));
  }

  clearAnchors(pane: number, alternate: boolean): void {
    callSync(() => this.raw.clearAnchors(pane, alternate));
  }

  setAlternateScreen(pane: number, alternate: boolean): void {
    callSync(() => this.raw.setAlternateScreen(pane, alternate));
  }

  /** Which pane a projected source landed on, if any. */
  paneForSource(source: SourceKey): number | null {
    return callSync(() => this.raw.paneForSource(source)) as number | null;
  }

  get projectionRevision(): number {
    return callSync(() => this.raw.projectionRevision) as number;
  }

  /** Request a keyframe; the outcome says whether it was forwarded, damped, or ignored. */
  requestKeyframe(source: SourceKey, reason: number, minimumEpoch?: number): string {
    return callSync(() =>
      this.raw.requestKeyframe(source, minimumEpoch ?? undefined, reason),
    ) as string;
  }

  requestFullFrames(sources: readonly SourceKey[], reason: number): void {
    callSync(() => this.raw.requestFullFrames([...sources], reason));
  }

  /** Record the outer gateway's playback position for a source. */
  applyOuterPosition(
    source: SourceKey,
    position: {
      readonly decoderResetSerial: number;
      readonly playing: boolean;
      readonly startPtsUs: number;
      readonly state: number;
      readonly clockPtsUs?: number;
      readonly decodedPtsUs: number;
      readonly presentedPtsUs: number;
      readonly presentationId: number;
    },
  ): void {
    callSync(() =>
      this.raw.applyOuterPosition(source, {
        decoderResetSerial: position.decoderResetSerial,
        playing: position.playing,
        startPtsUs: position.startPtsUs,
        state: position.state,
        clockPtsUs: position.clockPtsUs ?? undefined,
        decodedPtsUs: position.decodedPtsUs,
        presentedPtsUs: position.presentedPtsUs,
        presentationId: position.presentationId,
      }),
    );
  }

  /** Record the outer gateway's playback and end-of-stream state for a source. */
  applyOuterPlayback(
    source: SourceKey,
    decoderResetSerial: number,
    stateValue: number,
    eosState: number,
  ): void {
    callSync(() =>
      this.raw.applyOuterPlayback(source, decoderResetSerial, stateValue, eosState),
    );
  }

  /** Mint a media resource id; `pinned` freezes the content, otherwise it follows the surface. */
  announceMediaResource(source: SourceKey, pinned: boolean): string {
    return callSync(() => this.raw.announceMediaResource(source, pinned)) as string;
  }

  describeMediaResource(id: string): MediaResourceInfo {
    return callSync(() => this.raw.describeMediaResource(id)) as MediaResourceInfo;
  }

  releaseMediaResource(id: string): boolean {
    return callSync(() => this.raw.releaseMediaResource(id)) as boolean;
  }

  async close(): Promise<void> {
    await call(this.raw.close());
  }

  async [Symbol.asyncDispose](): Promise<void> {
    await this.close();
  }
}

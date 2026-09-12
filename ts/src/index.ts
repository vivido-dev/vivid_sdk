/**
 * Typed TypeScript producer and presenter SDK for Vivid Protocol 1.5.
 *
 * Mirrors the Python package one-for-one, over the same native addon, so the two read as
 * translations of each other. The API is the 1.5 object model: stable surfaces own immutable
 * tracks, and each track is fed through an authenticated channel generation.
 *
 * Everything blocking is async here. Native calls run on the addon's worker pool, so a flow wait
 * or a control round trip never holds the event loop.
 */

import { ClosedHandleError, call, callSync, native, optionalFields } from "./native.js";
import type { ConstantEntry } from "./native.js";
import { encodeSceneGeometry } from "./payload.js";
import type { Payload } from "./payload.js";
import { MAX_TRACK_WAIT_TIMEOUT_US } from "./constants.js";

export * from "./constants.js";
export { VividError, ClosedHandleError } from "./native.js";
export { automation } from "./automation.js";
export type { Payload, PayloadRaw, PayloadScalar } from "./payload.js";
export * as input from "./input.js";
export * as fileDrop from "./file-drop.js";
export * as pipeline from "./pipeline.js";
export * as lease from "./lease.js";
export { PaneSession } from "./pane.js";
export { DesktopSession, establishDesktop } from "./desktop.js";
export * as presenter from "./presenter.js";

// ---------------------------------------------------------------------------
// Options and configuration
// ---------------------------------------------------------------------------

/** How to reach a presenter and who to be. */
export interface ConnectOptions {
  /** Bypass every endpoint and connect in-process against the offline contract. */
  readonly offline?: boolean;
  /** The desktop producer profile set: `desktop-surface-v1` target, live media required. */
  readonly desktop?: boolean;
  /** Record metadata-only control and track traces under this directory. */
  readonly traceDir?: string;
  readonly endpointControl?: string;
  readonly endpointInteractive?: string;
  readonly endpointRealtime?: string;
  readonly endpointBulk?: string;
  /**
   * Root secret as hex. Prefer leaving this unset: the discovery environment is read on the
   * Rust side, and a value passed through here has crossed JavaScript.
   */
  readonly rootSecret?: string;
  readonly producerName?: string;
  readonly producerVersion?: string;
  readonly targetProfile?: string;
  readonly requiredProfiles?: readonly string[];
  readonly optionalProfiles?: readonly string[];
}

/** One output in a desktop surface topology. */
export interface OutputConfig {
  readonly outputId: number;
  readonly originX: number;
  readonly originY: number;
  readonly width: number;
  readonly height: number;
  readonly scaleNumerator?: number;
  readonly scaleDenominator?: number;
  /** Protocol rotation code: 0 none, 1 90, 2 180, 3 270. */
  readonly rotation?: number;
  readonly primary?: boolean;
}

/**
 * Typed parameters that make a surface a desktop surface.
 *
 * Without these a `desktop-content-v1` surface carries no captured origin, topology, or input
 * capabilities, and the presenter has nothing to map input back onto.
 */
export interface DesktopParameters {
  readonly capturedOriginX: number;
  readonly capturedOriginY: number;
  readonly topology: readonly OutputConfig[];
  readonly semanticGeneration: number;
  readonly inputCapabilities?: number;
}

/** A surface's semantic, scene, and policy identity. */
export interface SurfaceConfig {
  readonly logicalWidth: number;
  readonly logicalHeight: number;
  readonly semanticProfile?: string;
  readonly coordinateModel?: number;
  readonly role?: number;
  readonly title?: string;
  readonly semanticContentRevision?: number;
  readonly semanticAvailability?: number;
  readonly locatorHint?: string;
  readonly policy?: number;
  readonly scaleNumerator?: number;
  readonly scaleDenominator?: number;
  readonly rotation?: number;
  readonly contextId?: number;
  readonly surfaceId?: number;
  readonly desktopParameters?: DesktopParameters;
}

/**
 * A retained raster track.
 *
 * A full frame is a fixed size, so the record-body, in-flight, and retained-pixel claims all
 * follow from the geometry and are computed in Rust. State them only to narrow them.
 */
export interface RasterTrackConfig {
  readonly width: number;
  readonly height: number;
  readonly maximumRateMillihertz?: number;
  readonly alphaMode?: number;
  readonly deltaEnabled?: boolean;
  readonly maximumDeltaOperations?: number;
  readonly zstdEnabled?: boolean;
  readonly slot?: number;
  readonly mode?: number;
  readonly lane?: number;
  readonly maximumEncodedBitsPerSecond?: number;
  readonly trackId?: number;
}

/**
 * A one-shot encoded-image track.
 *
 * The container is inspected by the SDK, so the declared dimensions and length are the file's
 * real ones; pass `sha256` to let a presenter cache the image across presentations.
 */
export interface ImageTrackConfig {
  readonly encoded: Uint8Array;
  readonly sha256?: Uint8Array;
  readonly cacheLookup?: boolean;
  readonly slot?: number;
  readonly lane?: number;
  readonly trackId?: number;
}

/** A live or timed video track. */
export interface VideoTrackConfig {
  readonly codec: string;
  readonly width: number;
  readonly height: number;
  readonly packetization?: string;
  readonly maximumAccessUnitBytes?: number;
  readonly maximumRateMillihertz?: number;
  readonly maximumEncodedBitsPerSecond?: number;
  readonly extradata?: Uint8Array;
  readonly profile?: number;
  readonly level?: number;
  readonly maximumReorderDepth?: number;
  readonly colorPrimaries?: number;
  readonly transfer?: number;
  readonly matrix?: number;
  readonly signalRange?: number;
  readonly aspectNumerator?: number;
  readonly aspectDenominator?: number;
  readonly codecString?: string;
  readonly decoderConfiguration?: Uint8Array;
  readonly slot?: number;
  readonly mode?: number;
  readonly lane?: number;
  readonly trackId?: number;
}

/** An audio track. Defaults to Opus in 20 ms packets on the realtime lane. */
export interface AudioTrackConfig {
  readonly sampleRate: number;
  readonly channels: number;
  readonly codec?: string;
  readonly packetization?: string;
  readonly maximumAccessUnitBytes?: number;
  readonly maximumEncodedBitsPerSecond?: number;
  readonly maximumRateMillihertz?: number;
  readonly extradata?: Uint8Array;
  readonly channelMask?: number;
  readonly codecString?: string;
  readonly slot?: number;
  readonly mode?: number;
  readonly lane?: number;
  /** Declare the track as microphone audio flowing toward the producer side. */
  readonly uplink?: boolean;
  readonly trackId?: number;
}

export type TrackConfig =
  | ({ readonly kind: "raster" } & RasterTrackConfig)
  | ({ readonly kind: "image" } & ImageTrackConfig)
  | ({ readonly kind: "video" } & VideoTrackConfig)
  | ({ readonly kind: "audio" } & AudioTrackConfig);

/** One node in a surface's retained scene. */
export interface SceneNodeConfig {
  readonly nodeId: number;
  /** The protocol's integer-keyed geometry map for the surface's coordinate model. */
  readonly geometry: Readonly<Record<number, number | string | boolean>>;
  readonly fit?: number;
  readonly linearSampling?: boolean;
  readonly zIndex?: number;
  readonly visible?: boolean;
  readonly opacity?: number;
}

/** One slot's activation binding, naming the exact channel generation it expects. */
export interface SlotBindingConfig {
  readonly slot: number;
  readonly trackId: number;
  readonly expectedChannelGeneration: number;
  readonly requiredMilestone?: number;
}

/** Session identity and the profiles the presenter accepted. */
export interface SessionInfo {
  readonly sessionId: number;
  /** Opaque session tag, hex. */
  readonly sessionTag: string;
  readonly rootContextId: number;
  readonly targetGeneration: number;
  readonly targetProfile: string;
  readonly acceptedProfiles: readonly string[];
  readonly sessionRevision: number;
  readonly sceneRevision: number;
  readonly establishmentState: number;
  readonly resumeGeneration: number;
}

/** One session event, flattened by `kind`. */
export interface SessionEvent {
  readonly kind:
    | "targetChanged"
    | "anchorReady"
    | "anchorGone"
    | "trackLost"
    | "contextChanged"
    | "fileDropOffered"
    | "fileDropCancelled"
    | "other"
    | "connectionClosed";
  readonly contextId?: number;
  readonly anchorId?: number;
  readonly objectId?: number;
  readonly recordType?: number;
  readonly diagnostic?: string;
  readonly payload?: Payload;
}

/** One reverse-channel event. */
export interface ChannelEvent {
  readonly kind: "needKeyframe" | "needFullFrame" | "error";
  readonly payload?: Payload;
  readonly code?: number;
  readonly message?: string;
}

/**
 * How long the last sends waited, split by cause.
 *
 * The three causes have opposite remedies — lower the encoder's output, wait for the presenter
 * to return channel-flow capacity, or shrink the transport writes — so they stay separate.
 */
export interface SendPressure {
  readonly rateLimitedUs: number;
  readonly flowLimitedUs: number;
  readonly transportUs: number;
  readonly records: number;
}

// ---------------------------------------------------------------------------
// Handles
// ---------------------------------------------------------------------------

/* eslint-disable @typescript-eslint/no-explicit-any */
type Raw = any;

/** An established logical session. */
export class Session {
  readonly raw: Raw;

  private constructor(raw: Raw) {
    this.raw = raw;
  }

  /** @internal */
  static wrap(raw: Raw): Session {
    return new Session(raw);
  }

  /** Connect to a presenter. Blocking work runs on the addon's worker pool. */
  static async connect(options: ConnectOptions = {}): Promise<Session> {
    return Session.wrap(await call(native().connect(options)));
  }

  async close(): Promise<void> {
    await call(this.raw.close());
  }

  /** Close the lifecycle without a `GOODBYE` round trip, releasing blocked senders. */
  abort(): void {
    callSync(() => this.raw.abort());
  }

  get closed(): boolean {
    return callSync(() => this.raw.closed) as boolean;
  }

  info(): SessionInfo {
    return callSync(() => this.raw.info()) as SessionInfo;
  }

  supports(profile: string): boolean {
    return callSync(() => this.raw.supports(profile)) as boolean;
  }

  async allocateId(): Promise<number> {
    return call(this.raw.allocateId());
  }

  /** The next session event, or `null` when the queue is empty. */
  takeEvent(): SessionEvent | null {
    return callSync(() => this.raw.takeEvent()) as SessionEvent | null;
  }

  /** The next session event, waiting up to `timeoutMs`. */
  async waitEvent(timeoutMs = MAX_TRACK_WAIT_TIMEOUT_US / 1000): Promise<SessionEvent | null> {
    return (await call(this.raw.waitEvent(timeoutMs))) as SessionEvent | null;
  }

  /**
   * Iterate this session's events until it closes.
   *
   * Each step parks a worker on a bounded wait, so the loop costs nothing while the session is
   * quiet and the event loop stays free. The iterator ends when the session reports
   * `connectionClosed`, which is the last event it will produce, or when the session has been
   * closed underneath it — a closed session has no events left, and that is an end rather than
   * a failure. Passing a signal stops it sooner.
   */
  async *events(
    options: { readonly timeoutMs?: number; readonly signal?: AbortSignal } = {},
  ): AsyncGenerator<SessionEvent> {
    const timeoutMs = options.timeoutMs ?? MAX_TRACK_WAIT_TIMEOUT_US / 1000;
    while (options.signal?.aborted !== true) {
      let event: SessionEvent | null;
      try {
        event = await this.waitEvent(timeoutMs);
      } catch (error) {
        if (error instanceof ClosedHandleError) {
          return;
        }
        throw error;
      }
      if (event === null || event.kind === "connectionClosed") {
        return;
      }
      yield event;
    }
  }

  async createSurface(config: SurfaceConfig): Promise<Surface> {
    return Surface.wrap(await call(this.raw.createSurface(config)));
  }

  async updateSurface(surface: Surface, config: SurfaceConfig): Promise<void> {
    await call(this.raw.updateSurface(surface.raw, config));
  }

  async destroySurface(surface: Surface): Promise<void> {
    await call(this.raw.destroySurface(surface.raw));
  }

  async querySurface(surface: Surface): Promise<Record<string, unknown>> {
    return (await call(this.raw.querySurface(surface.raw))) as Record<string, unknown>;
  }

  async createTrack(surface: Surface, config: TrackConfig): Promise<Track> {
    return Track.wrap(await call(this.raw.createTrack(surface.raw, config)));
  }

  async destroyTrack(track: Track): Promise<void> {
    await call(this.raw.destroyTrack(track.raw));
  }

  async queryTrack(track: Track): Promise<Record<string, unknown>> {
    return (await call(this.raw.queryTrack(track.raw))) as Record<string, unknown>;
  }

  /** Whether the presenter would admit this track, without creating it. */
  async probeTrack(
    surface: Surface,
    config: TrackConfig,
  ): Promise<{ supported: boolean; selectedDecoder: string; capabilityGeneration: number }> {
    return (await call(this.raw.probeTrack(surface.raw, config))) as {
      supported: boolean;
      selectedDecoder: string;
      capabilityGeneration: number;
    };
  }

  async openTrackChannel(track: Track): Promise<TrackChannel> {
    return TrackChannel.wrap(await call(this.raw.openTrackChannel(track.raw)));
  }

  /** Start a fresh authenticated channel generation and return its channel. */
  async advanceChannel(track: Track, reason: number): Promise<TrackChannel> {
    return TrackChannel.wrap(await call(this.raw.advanceChannel(track.raw, reason)));
  }

  /** Recover a lost channel: advance, reopen, and send the key unit. */
  async recoverChannel(track: Track, keyUnit: Uint8Array): Promise<TrackSender> {
    return TrackSender.wrap(await call(this.raw.recoverChannel(track.raw, Buffer.from(keyUnit))));
  }

  async waitTrack(
    track: Track,
    condition: number,
    value?: number,
    timeoutMs?: number,
  ): Promise<Record<string, number>> {
    return (await call(
      this.raw.waitTrack(track.raw, condition, value ?? undefined, timeoutMs),
    )) as Record<string, number>;
  }

  /** Activate this track into its configured slot at a compositor boundary. */
  async activateTrack(
    surface: Surface,
    track: Track,
    requiredMilestone?: number,
  ): Promise<number> {
    return (await call(
      this.raw.activateTrack(surface.raw, track.raw, requiredMilestone),
    )) as number;
  }

  /** Activate a slot set atomically at a compositor boundary. */
  async activateTracks(surface: Surface, bindings: readonly SlotBindingConfig[]): Promise<number> {
    return (await call(
      this.raw.activateTracks(
        surface.raw,
        bindings.map((binding) => ({
          slot: binding.slot,
          trackId: binding.trackId,
          expectedChannelGeneration: binding.expectedChannelGeneration,
          requiredMilestone: binding.requiredMilestone,
        })),
      ),
    )) as number;
  }

  async play(
    track: Track,
    options: {
      readonly startPtsUs?: number;
      readonly minimumBufferUs?: number;
      readonly maximumLatencyUs?: number;
    } = {},
  ): Promise<void> {
    await call(
      this.raw.play(
        track.raw,
        options.startPtsUs ?? 0,
        options.minimumBufferUs ?? 0,
        options.maximumLatencyUs ?? 0,
      ),
    );
  }

  async pause(track: Track): Promise<void> {
    await call(this.raw.pause(track.raw));
  }

  /** Set track gain as a micropercent, where `2^32` is unity and `2^33` the protocol maximum. */
  async setAudioGain(track: Track, raw: number): Promise<void> {
    await call(this.raw.setAudioGain(track.raw, raw));
  }

  /** Discard media below a new epoch and keep the channel open. */
  async flush(track: Track, newEpoch: number): Promise<void> {
    await call(this.raw.flush(track.raw, newEpoch));
  }

  /** Wait until the presenter has consumed everything sent so far. */
  async drain(track: Track): Promise<void> {
    await call(this.raw.drain(track.raw));
  }

  async createNode(
    surface: Surface,
    node: SceneNodeConfig,
  ): Promise<{ sceneRevision: number; targetGeneration: number }> {
    return (await call(
      this.raw.createNode(surface.raw, node.nodeId, encodeSceneNode(node)),
    )) as { sceneRevision: number; targetGeneration: number };
  }

  async updateNode(
    surface: Surface,
    node: SceneNodeConfig,
  ): Promise<{ sceneRevision: number; targetGeneration: number }> {
    return (await call(
      this.raw.updateNode(surface.raw, node.nodeId, encodeSceneNode(node)),
    )) as { sceneRevision: number; targetGeneration: number };
  }

  async placeTerminalSurface(
    surface: Surface,
    options: {
      readonly nodeId: number;
      readonly x?: number;
      readonly y?: number;
      readonly width: number;
      readonly height: number;
      readonly textLayer?: number;
    },
  ): Promise<{ sceneRevision: number; targetGeneration: number }> {
    return (await call(
      this.raw.placeTerminalSurface(
        surface.raw,
        options.nodeId,
        options.x ?? 0,
        options.y ?? 0,
        options.width,
        options.height,
        options.textLayer ?? undefined,
      ),
    )) as { sceneRevision: number; targetGeneration: number };
  }

  async deleteNode(
    contextId: number,
    nodeId: number,
  ): Promise<{ sceneRevision: number; targetGeneration: number }> {
    return (await call(this.raw.deleteNode(contextId, nodeId))) as {
      sceneRevision: number;
      targetGeneration: number;
    };
  }

  /** The anchor marker spelling for this anchor, for a terminal host to print. */
  anchorMarker(contextId: number, anchorId: number): string {
    return callSync(() => this.raw.anchorMarker(contextId, anchorId)) as string;
  }

  /** The marker spelling a ConPTY host prints. */
  conptyAnchorMarker(contextId: number, anchorId: number): string {
    return callSync(() => this.raw.conptyAnchorMarker(contextId, anchorId)) as string;
  }

  async queryAnchor(contextId: number, anchorId: number): Promise<unknown | null> {
    return await call(this.raw.queryAnchor(contextId, anchorId));
  }

  async querySession(): Promise<Payload> {
    return (await call(this.raw.querySession())) as Payload;
  }
}

/** A surface: the identity that owns a scene, a policy, and tracks. */
export class Surface {
  readonly raw: Raw;

  private constructor(raw: Raw) {
    this.raw = raw;
  }

  /** @internal */
  static wrap(raw: Raw): Surface {
    return new Surface(raw);
  }

  get contextId(): number {
    return callSync(() => this.raw.contextId) as number;
  }

  get id(): number {
    return callSync(() => this.raw.id) as number;
  }

  get revision(): number {
    return callSync(() => this.raw.revision) as number;
  }

  get generation(): number {
    return callSync(() => this.raw.generation) as number;
  }
}

/** One immutable media configuration owned by a surface. */
export class Track {
  readonly raw: Raw;

  private constructor(raw: Raw) {
    this.raw = raw;
  }

  /** @internal */
  static wrap(raw: Raw): Track {
    return new Track(raw);
  }

  get contextId(): number {
    return callSync(() => this.raw.contextId) as number;
  }

  get surfaceId(): number {
    return callSync(() => this.raw.surfaceId) as number;
  }

  get id(): number {
    return callSync(() => this.raw.id) as number;
  }

  get kind(): string {
    return callSync(() => this.raw.kind) as string;
  }

  get revision(): number {
    return callSync(() => this.raw.revision) as number;
  }

  get channelGeneration(): number {
    return callSync(() => this.raw.channelGeneration) as number;
  }
}

/** One authenticated transport generation for a track. */
export class TrackChannel {
  readonly raw: Raw;

  private constructor(raw: Raw) {
    this.raw = raw;
  }

  /** @internal */
  static wrap(raw: Raw): TrackChannel {
    return new TrackChannel(raw);
  }

  get contextId(): number {
    return callSync(() => this.raw.contextId) as number;
  }

  get surfaceId(): number {
    return callSync(() => this.raw.surfaceId) as number;
  }

  get trackId(): number {
    return callSync(() => this.raw.trackId) as number;
  }

  get kind(): string {
    return callSync(() => this.raw.kind) as string;
  }

  get generation(): number {
    return callSync(() => this.raw.generation) as number;
  }

  get closed(): boolean {
    return callSync(() => this.raw.closed) as boolean;
  }

  async sendRaster(
    rgba: Uint8Array,
    options: { readonly epoch?: number; readonly frameId?: number; readonly compress?: boolean } = {},
  ): Promise<number> {
    return (await call(
      this.raw.sendRaster(
        Buffer.from(rgba),
        options.epoch,
        options.frameId,
        options.compress,
      ),
    )) as number;
  }

  /** Send a frame, compressing only when the result is actually smaller than raw. */
  async sendRasterAdaptive(
    rgba: Uint8Array,
    options: { readonly epoch?: number; readonly frameId?: number } = {},
  ): Promise<number> {
    return (await call(
      this.raw.sendRasterAdaptive(
        Buffer.from(rgba),
        options.epoch ?? undefined,
        options.frameId ?? undefined,
      ),
    )) as number;
  }

  async sendImage(encoded: Uint8Array): Promise<number> {
    return (await call(this.raw.sendImage(Buffer.from(encoded)))) as number;
  }

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
      this.raw.sendVideo(Buffer.from(data), {
        packetId: options.packetId ?? undefined,
        ptsUs: options.ptsUs,
        dtsUs: options.dtsUs ?? undefined,
        durationUs: options.durationUs ?? undefined,
        key: options.key ?? undefined,
        epoch: options.epoch ?? undefined,
      }),
    )) as number;
  }

  async sendAudio(
    data: Uint8Array,
    options: {
      readonly ptsUs: number;
      readonly durationUs: number;
      readonly packetId?: number;
      readonly epoch?: number;
      readonly trimStartSamples?: number;
      readonly trimEndSamples?: number;
    },
  ): Promise<number> {
    return (await call(
      this.raw.sendAudio(Buffer.from(data), {
        packetId: options.packetId ?? undefined,
        ptsUs: options.ptsUs,
        dtsUs: options.ptsUs,
        durationUs: options.durationUs,
        epoch: options.epoch ?? undefined,
        trimStartSamples: options.trimStartSamples ?? undefined,
        trimEndSamples: options.trimEndSamples ?? undefined,
      }),
    )) as number;
  }

  /** Signal ordered end-of-stream. */
  async eos(): Promise<number> {
    return (await call(this.raw.eos())) as number;
  }

  async close(): Promise<void> {
    await call(this.raw.close());
  }

  takeEvent(): ChannelEvent | null {
    return callSync(() => this.raw.takeEvent()) as ChannelEvent | null;
  }

  /**
   * The next reverse-channel event, waiting up to `timeoutMs`.
   *
   * A keyframe request that arrives while nobody is looking is the difference between a fast
   * recovery and a frozen picture, so park here between frames rather than polling.
   */
  async waitEvent(timeoutMs = MAX_TRACK_WAIT_TIMEOUT_US / 1000): Promise<ChannelEvent | null> {
    return (await call(this.raw.waitEvent(timeoutMs))) as ChannelEvent | null;
  }

  /** Iterate reverse events until the transport ends. */
  async *events(
    options: { readonly timeoutMs?: number; readonly signal?: AbortSignal } = {},
  ): AsyncGenerator<ChannelEvent> {
    const timeoutMs = options.timeoutMs ?? MAX_TRACK_WAIT_TIMEOUT_US / 1000;
    while (options.signal?.aborted !== true) {
      const event = await this.waitEvent(timeoutMs);
      if (event === null) {
        return;
      }
      yield event;
    }
  }

  takeSendPressure(): SendPressure {
    return callSync(() => this.raw.takeSendPressure()) as SendPressure;
  }

  /** Whether a record of `bodyLength` bytes fits the channel's flow window right now. */
  mediaCreditAvailable(bodyLength: number): boolean {
    return callSync(() => this.raw.mediaCreditAvailable(bodyLength)) as boolean;
  }

  /** Ask the presenter for the next microphone packet. */
  async grantAudioInput(): Promise<void> {
    await call(this.raw.grantAudioInput());
  }

  /** Take one microphone packet, or `null` when the presenter has produced none. */
  async takeAudioInput(): Promise<MicPacket | null> {
    return (await call(this.raw.takeAudioInput())) as MicPacket | null;
  }
}

/** One microphone packet: 20 ms of 48 kHz mono s16LE. */
export interface MicPacket {
  readonly epoch: number;
  readonly packetId: number;
  readonly ptsUs: number;
  readonly pcm: Uint8Array;
}

/** A sender that keeps packet IDs and the media epoch continuous across channel recovery. */
export class TrackSender {
  readonly raw: Raw;

  private constructor(raw: Raw) {
    this.raw = raw;
  }

  /** @internal */
  static wrap(raw: Raw): TrackSender {
    return new TrackSender(raw);
  }

  get generation(): number {
    return callSync(() => this.raw.generation) as number;
  }

  get detached(): boolean {
    return callSync(() => this.raw.detached) as boolean;
  }

  async nextPacketId(): Promise<number> {
    return (await call(this.raw.nextPacketId())) as number;
  }

  async currentEpoch(): Promise<number> {
    return (await call(this.raw.currentEpoch())) as number;
  }

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

  async sendAudio(
    data: Uint8Array,
    options: {
      readonly ptsUs: number;
      readonly durationUs: number;
      readonly packetId?: number;
      readonly epoch?: number;
    },
  ): Promise<number> {
    return (await call(
      this.raw.sendAudio({
        data: Buffer.from(data),
        packetId: options.packetId ?? undefined,
        ptsUs: options.ptsUs,
        durationUs: options.durationUs,
        epoch: options.epoch ?? undefined,
      }),
    )) as number;
  }

  detach(): void {
    callSync(() => this.raw.detach());
  }
}

/**
 * Producer-side encoder pacing, fed by `SendPressure` observations.
 *
 * Send pressure has three causes with opposite remedies: a rate limit means the encoder should
 * produce less, a flow limit means the presenter is behind and the sender should wait, and
 * transport time means the writes themselves are slow. The controller distinguishes them.
 */
export class VideoRateControl {
  readonly raw: Raw;

  constructor(configuredBitsPerSecond: number) {
    this.raw = callSync(() => new (native().VideoRateControl as new (value: number) => unknown)(configuredBitsPerSecond));
  }

  observeSend(bytes: number, pressure: SendPressure): void {
    callSync(() =>
      this.raw.observeSend(
        bytes,
        pressure.rateLimitedUs,
        pressure.flowLimitedUs,
        pressure.transportUs,
        pressure.records,
      ),
    );
  }

  /** Tell the controller how far audio has fallen behind, so video yields to it. */
  observeAudioBacklog(backlogUs: number): void {
    callSync(() => this.raw.observeAudioBacklog(backlogUs));
  }

  /** The encoder target, if it changed since the last poll. */
  poll(): number | null {
    return callSync(() => this.raw.poll()) as number | null;
  }

  snapshot(): {
    configuredBitsPerSecond: number;
    targetBitsPerSecond: number;
    adjustments: number;
    rateLimitedUs: number;
    flowLimitedUs: number;
    transportUs: number;
  } {
    return callSync(() => this.raw.snapshot()) as {
      configuredBitsPerSecond: number;
      targetBitsPerSecond: number;
      adjustments: number;
      rateLimitedUs: number;
      flowLimitedUs: number;
      transportUs: number;
    };
  }
}

// ---------------------------------------------------------------------------
// Free functions
// ---------------------------------------------------------------------------

/** The protocol constants the SDK exposes, straight from its table. */
export function constantTable(): readonly ConstantEntry[] {
  return native().constantTable();
}

/**
 * `{ encoding, width, height, encodedLength }` for a complete PNG or JPEG.
 *
 * The container is walked in Rust, beside the configuration it produces, so the dimensions a
 * track declares and the pixels it later sends cannot come from two different parsers.
 */
export function probeEncodedImage(data: Uint8Array): {
  encoding: number;
  width: number;
  height: number;
  encodedLength: number;
} {
  return callSync(() => native().probeEncodedImage(Buffer.from(data)));
}

function encodeSceneNode(node: SceneNodeConfig): Record<string, unknown> {
  return {
    geometry: encodeSceneGeometry(node.geometry),
    fit: node.fit ?? undefined,
    linearSampling: node.linearSampling ?? undefined,
    zIndex: node.zIndex ?? undefined,
    visible: node.visible ?? undefined,
    opacity: node.opacity ?? undefined,
  };
}

// ---------------------------------------------------------------------------
// Module-level entry points
// ---------------------------------------------------------------------------
//
// The same verbs the Python package exposes at module scope. `Session.connect` and this
// `connect` are the same call; having both means a caller ported from Python does not have to
// learn a new shape, and one written fresh can use the class.

/** Connect to a presenter. */
export async function connect(options: ConnectOptions = {}): Promise<Session> {
  return await Session.connect(options);
}

/** End the session with a `GOODBYE` round trip. */
export async function close(session: Session): Promise<void> {
  await session.close();
}

/** Whether the presenter accepted this profile. */
export function supports(session: Session, profile: string): boolean {
  return session.supports(profile);
}

/** Close a surface and its tracks. */
export async function destroySurface(session: Session, surface: Surface): Promise<void> {
  await session.destroySurface(surface);
}

/** Close a track channel generation. */
export async function closeChannel(channel: TrackChannel): Promise<void> {
  await channel.close();
}

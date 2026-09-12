/**
 * Structural types for the native handles.
 *
 * The addon's own `.d.ts` describes these, but it is generated per platform at build time and is
 * not in the repository. These declarations say only what the TypeScript layer calls, so a
 * change in the addon's shape shows up as a compile error here rather than a runtime one.
 */

export interface NativePaneSession {
  readonly closed: boolean;
  readonly hasPresentation: boolean;
  showEncodedImage: (
    encoded: Buffer,
    title: string | undefined,
    columns: number | undefined,
    rows: number | undefined,
    textLayer: number | undefined,
  ) => Promise<void>;
  showRgba: (
    width: number,
    height: number,
    rgba: Buffer,
    title: string | undefined,
    columns: number | undefined,
    rows: number | undefined,
    textLayer: number | undefined,
  ) => Promise<void>;
  clear: () => Promise<void>;
  close: () => Promise<void>;
}

export interface NativeInputLane {
  readonly generation: number;
  readonly closed: boolean;
  setBinding: (binding: unknown) => Promise<unknown>;
  setInputBinding: (config: unknown) => Promise<unknown>;
  takeEvent: () => unknown;
  waitEvent: (timeoutMs: number) => Promise<unknown>;
  close: () => Promise<void>;
}

export interface NativeIncomingFileTransfer {
  readonly closed: boolean;
  readEvent: () => Promise<unknown>;
  setReadDeadline: (timeoutUs: number | undefined) => void;
  grant: (maximumBodyBytes: number, maximumRecords: number) => Promise<void>;
  sendResult: (spec: unknown) => Promise<void>;
  abort: (reason: number) => Promise<void>;
}

export interface NativePresenter {
  readonly closed: boolean;
  endpoint: () => string;
  readonly projectionRevision: number;
  issuePaneCapability: (pane: number) => string;
  revokePane: (pane: number) => void;
  updateMetrics: (
    pane: number,
    columns: number,
    rows: number,
    cellWidth: number,
    cellHeight: number,
  ) => void;
  waitForMedia: (pane: number, timeoutMs: number) => Promise<boolean>;
  capturePane: (pane: number, viewportOffset: number) => Promise<unknown>;
  paneMediaSummary: (pane: number) => Promise<unknown>;
  queueMicrophone: (
    source: unknown,
    generation: number,
    bytes: Buffer,
  ) => Promise<boolean>;
  revokeMicrophones: () => void;
  notifyCapabilitiesChanged: (reasonMask: number) => number;
  updateDesktopTarget: (
    pane: number,
    width: number,
    height: number,
    reasonMask: number,
  ) => number;
  observeMarker: (
    pane: number,
    value: string,
    row: number,
    column: number,
    alternate: boolean,
  ) => void;
  scrollAnchors: (pane: number, lines: number, alternate: boolean) => void;
  clearAnchors: (pane: number, alternate: boolean) => void;
  setAlternateScreen: (pane: number, alternate: boolean) => void;
  paneForSource: (source: unknown) => number | undefined;
  requestKeyframe: (source: unknown, minimumEpoch: number | undefined, reason: number) => string;
  requestFullFrames: (sources: readonly unknown[], reason: number) => void;
  applyOuterPosition: (source: unknown, position: unknown) => void;
  applyOuterPlayback: (
    source: unknown,
    decoderResetSerial: number,
    stateValue: number,
    eosState: number,
  ) => void;
  announceMediaResource: (source: unknown, pinned: boolean) => string;
  describeMediaResource: (id: string) => unknown;
  releaseMediaResource: (id: string) => boolean;
  close: () => Promise<void>;
}

export interface NativeDesktopSession {
  readonly closed: boolean;
  videoTrack: () => Promise<unknown>;
  audioTrack: () => Promise<unknown | null>;
  sendVideo: (spec: unknown) => Promise<number>;
  sendAudio: (spec: unknown) => Promise<number>;
  activateSlots: () => Promise<void>;
  close: () => Promise<void>;
}

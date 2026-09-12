/**
 * File-drop bindings and incoming transfers.
 *
 * A drop is offered to a surface's effective binding, accepted by a policy or a person, and
 * carried on its own authenticated connection with independent flow control. That connection is
 * the only place file bytes travel, and it never touches the control or media lanes.
 */

import { call, native } from "./native.js";
import type { Session } from "./index.js";
import type { NativeIncomingFileTransfer } from "./native-types.js";

export { }; // keep this a module even as the surface grows

/** A destination the host is willing to commit into. */
export const DESTINATION_SHELL_CWD = 1;
/** The user's desktop folder. */
export const DESTINATION_DESKTOP_FOLDER = 2;

// The specification's `FILE_DROP_STATUS` states.
export const DROP_OFFERED = 1;
export const DROP_ACCEPTED = 2;
export const DROP_TRANSFERRING = 3;
export const DROP_COMMITTED = 4;
export const DROP_CANCELLED = 5;
export const DROP_FAILED = 6;

/** The presenter's effective file-drop binding. */
export interface FileDropGrant {
  readonly producerEpoch: number;
  readonly grantGeneration: number;
  readonly contextId: number;
  readonly surfaceId: number;
  readonly surfaceGeneration: number;
  readonly state: number;
  readonly destination: number | null;
  readonly maximumFileBytes: number;
  readonly maximumPendingOffers: number;
  readonly maximumActiveTransfers: number;
  readonly maximumRecordBody: number;
  readonly acceptanceTimeoutUs: number;
  readonly idleTimeoutUs: number;
  readonly reason: number;
}

/** The complete drop identity every verb names. */
export interface FileDropTuple {
  readonly producerEpoch: number;
  readonly grantGeneration: number;
  readonly contextId: number;
  readonly surfaceId: number;
  readonly surfaceGeneration: number;
  readonly dropId: number;
}

export interface FileDropAccepted {
  readonly dropId: number;
  readonly transferId: number;
  readonly transferGeneration: number;
  readonly openTimeoutUs: number;
}

export interface FileTransferAdvanced {
  readonly transferId: number;
  readonly generation: number;
  readonly committedOffset: number;
  readonly openTimeoutUs: number;
}

export interface FileDropStatus {
  readonly dropId: number;
  readonly state: number;
  readonly transferId: number;
  readonly generation: number;
  readonly committedOffset: number;
  readonly result: number | null;
  readonly finalName: string;
}

/** One event on an incoming transfer connection. */
export interface TransferEvent {
  readonly kind: "data" | "finished" | "aborted";
  readonly offset?: number;
  readonly bytes?: Uint8Array;
  readonly finalLength?: number;
  readonly reason?: number;
  readonly finalOffset?: number;
}

/** An incoming transfer connection, once an offer has been accepted. */
export class IncomingFileTransfer {
  readonly raw: NativeIncomingFileTransfer;

  private constructor(raw: NativeIncomingFileTransfer) {
    this.raw = raw;
  }

  /** @internal */
  static wrap(raw: NativeIncomingFileTransfer): IncomingFileTransfer {
    return new IncomingFileTransfer(raw);
  }

  get closed(): boolean {
    return this.raw.closed;
  }

  /**
   * Read the next event, blocking until one arrives.
   *
   * Bound the wait with `setReadDeadline` when a send can stall, so a quiet peer cannot pin the
   * reading worker forever.
   */
  async readEvent(): Promise<TransferEvent> {
    return (await call(this.raw.readEvent())) as TransferEvent;
  }

  /** Bound every later read; `null` restores unbounded reads. */
  setReadDeadline(timeoutUs: number | null): void {
    // napi reads an Option as present-or-undefined, so absence is undefined here.
    this.raw.setReadDeadline(timeoutUs ?? undefined);
  }

  /** Return flow capacity to the sender after committing the bytes read so far. */
  async grant(maximumBodyBytes: number, maximumRecords: number): Promise<void> {
    await call(this.raw.grant(maximumBodyBytes, maximumRecords));
  }

  /**
   * Report the outcome.
   *
   * `committedPath` requires `file-drop-path-v1` and is refused locally without it, rather than
   * failing the whole connection.
   */
  async sendResult(result: {
    readonly transferId: number;
    readonly transferGeneration: number;
    readonly result: number;
    readonly committedLength?: number;
    readonly finalName?: string;
    readonly committedPath?: string;
  }): Promise<void> {
    await call(
      this.raw.sendResult({
        transferId: result.transferId,
        transferGeneration: result.transferGeneration,
        result: result.result,
        committedLength: result.committedLength ?? undefined,
        finalName: result.finalName ?? undefined,
        committedPath: result.committedPath ?? undefined,
      }),
    );
  }

  /** Abandon the transfer, telling the sender why. */
  async abort(reason: number): Promise<void> {
    await call(this.raw.abort(reason));
  }
}

/**
 * Enable, replace, or disable a surface's file-drop binding.
 *
 * A `null` destination disables the binding rather than offering a default: a host with nowhere
 * to put a file should say so, not accept one it will drop.
 */
export async function setBinding(
  session: Session,
  binding: {
    readonly producerEpoch: number;
    readonly contextId: number;
    readonly surfaceId: number;
    readonly surfaceGeneration: number;
    readonly maximumFileBytes: number;
    readonly maximumRecordBody: number;
    readonly destination?: number | null;
    readonly maximumPendingOffers?: number;
    readonly maximumActiveTransfers?: number;
    readonly acceptanceTimeoutUs?: number;
    readonly idleTimeoutUs?: number;
  },
): Promise<FileDropGrant> {
  return (await call(
    session.raw.setFileDropBinding({
      producerEpoch: binding.producerEpoch,
      contextId: binding.contextId,
      surfaceId: binding.surfaceId,
      surfaceGeneration: binding.surfaceGeneration,
      destination: binding.destination ?? undefined,
      maximumFileBytes: binding.maximumFileBytes,
      maximumPendingOffers: binding.maximumPendingOffers ?? undefined,
      maximumActiveTransfers: binding.maximumActiveTransfers ?? undefined,
      maximumRecordBody: binding.maximumRecordBody,
      acceptanceTimeoutUs: binding.acceptanceTimeoutUs ?? undefined,
      idleTimeoutUs: binding.idleTimeoutUs ?? undefined,
    }),
  )) as FileDropGrant;
}

/** Accept an offer, naming the transfer this receiver is about to read. */
export async function accept(
  session: Session,
  options: {
    readonly drop: FileDropTuple;
    readonly transferId: number;
    readonly transferGeneration: number;
    readonly maximumRecordBody: number;
    readonly initialMaximumBodyBytes: number;
    readonly initialMaximumRecords: number;
  },
): Promise<FileDropAccepted> {
  return (await call(
    session.raw.acceptFileDrop(
      { ...options.drop },
      options.transferId,
      options.transferGeneration,
      options.maximumRecordBody,
      options.initialMaximumBodyBytes,
      options.initialMaximumRecords,
    ),
  )) as FileDropAccepted;
}

/** Decline an offer or give up on an accepted one. */
export async function cancel(
  session: Session,
  drop: FileDropTuple,
  reason: number,
): Promise<void> {
  await call(session.raw.cancelFileDrop({ ...drop }, reason));
}

/** Move a transfer onto a fresh generation after a resume, at a committed offset. */
export async function advance(
  session: Session,
  advance: {
    readonly contextId: number;
    readonly surfaceId: number;
    readonly dropId: number;
    readonly transferId: number;
    readonly expectedGeneration: number;
    readonly newGeneration: number;
    readonly committedOffset: number;
    readonly maximumBodyBytes: number;
    readonly maximumRecords: number;
  },
): Promise<FileTransferAdvanced> {
  return (await call(session.raw.advanceFileTransfer(advance))) as FileTransferAdvanced;
}

/** What the presenter believes about a drop, for reconciling after a reconnect. */
export async function query(session: Session, dropId: number): Promise<FileDropStatus> {
  return (await call(session.raw.queryFileDrop(dropId))) as FileDropStatus;
}

/** Take over the transfer connection the presenter opened for this acceptance. */
export async function openTransfer(
  session: Session,
  request: {
    readonly contextId: number;
    readonly surfaceId: number;
    readonly producerEpoch: number;
    readonly grantGeneration: number;
    readonly surfaceGeneration: number;
    readonly dropId: number;
    readonly transferId: number;
    readonly transferGeneration: number;
    readonly declaredLength: number;
    readonly maximumRecordBody: number;
    readonly maximumBodyBytes: number;
    readonly maximumRecords: number;
    readonly resumeOffset?: number;
  },
): Promise<IncomingFileTransfer> {
  return IncomingFileTransfer.wrap(await call(session.raw.openIncomingFileTransfer(request)));
}

/** The file-drop profile names, so a caller can check negotiation before binding. */
export const PROFILE_FILE_DROP_NAME = "file-drop-v1";
/** The path-disclosure extension. */
export const PROFILE_FILE_DROP_PATH_NAME = "file-drop-path-v1";

/** The native module, for callers that need to check a capability directly. */
export const nativeModule = native;

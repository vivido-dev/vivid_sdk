/**
 * Contexts, bounded session leases, and resumable authentication.
 *
 * A lease is how one producer hands a bounded piece of its own authority to another: the holder
 * gets a context, a profile set, a resource contract, and a deadline, and nothing else. The
 * activation secret is the lease's whole capability, so it leaves this module exactly once and is
 * not part of any `toJSON` or `inspect` output.
 */

import { call, native } from "./native.js";
import type { Session } from "./index.js";
import { DEFAULT_ACTIVATION_TIMEOUT_US, MAX_ACTIVATION_TIMEOUT_US } from "./constants.js";

export { DEFAULT_ACTIVATION_TIMEOUT_US, MAX_ACTIVATION_TIMEOUT_US };

/** A lease that is cleaned up as soon as the holder disconnects uncleanly. */
export { CLEANUP_IMMEDIATE } from "./constants.js";
/** A lease that may be resumed within its disconnect grace instead. */
export { CLEANUP_SUSPEND_ON_UNCLEAN_LOSS } from "./constants.js";

/** A created child context and the authority it was actually granted. */
export interface ContextReady {
  readonly contextId: number;
  readonly operationClasses: number;
  readonly contract: readonly number[];
  readonly lifetimeUs: number;
  readonly revision: number;
}

/** A minted session lease. */
export interface SessionLeaseReady {
  readonly contextId: number;
  readonly leaseId: number;
  readonly state: number;
  readonly activationTimeoutUs: number;
  readonly disconnectGraceUs: number;
  readonly cleanupPolicy: number;
  readonly permittedProfiles: readonly string[];
  readonly contract: readonly number[];
  readonly revision: number;
  /**
   * The holder's whole capability, returned once.
   *
   * Non-enumerable so it does not ride along in `JSON.stringify`, `util.inspect`, or a spread —
   * a capability that leaks through a log line is the same as one that leaked through the wire.
   */
  readonly activationSecretHex: string;
}

/** Create a child context scoped to the operation classes and contract given. */
export async function createContext(
  session: Session,
  options: {
    readonly contextId: number;
    readonly parentContextId: number;
    readonly operationClasses: number;
    readonly label?: string;
    readonly lifetimeUs?: number;
    readonly contract?: readonly number[];
  },
): Promise<ContextReady> {
  return (await call(
    session.raw.createContext(
      options.contextId,
      options.parentContextId,
      options.operationClasses,
      options.label ?? undefined,
      options.lifetimeUs ?? undefined,
      options.contract === undefined ? null : [...options.contract],
    ),
  )) as ContextReady;
}

/** Mint a bounded session lease for a worker. */
export async function createSessionLease(
  session: Session,
  options: {
    readonly contextId: number;
    readonly leaseId: number;
    readonly permittedProfiles: readonly string[];
    readonly activationTimeoutUs?: number;
    readonly disconnectGraceUs?: number;
    readonly cleanupPolicy?: number;
    readonly contract?: readonly number[];
  },
): Promise<SessionLeaseReady> {
  const ready = (await call(
    session.raw.createSessionLease(
      options.contextId,
      options.leaseId,
      [...new Set(options.permittedProfiles)].sort(),
      options.activationTimeoutUs ?? undefined,
      options.disconnectGraceUs ?? undefined,
      options.cleanupPolicy ?? undefined,
      options.contract === undefined ? null : [...options.contract],
    ),
  )) as SessionLeaseReady;
  // Hide the secret from every reflection path, not just `toJSON`.
  Object.defineProperty(ready, "activationSecretHex", {
    value: ready.activationSecretHex,
    enumerable: false,
    writable: false,
    configurable: false,
  });
  return ready;
}

/** Revoke a lease, releasing everything scoped to it. */
export async function revokeSessionLease(
  session: Session,
  contextId: number,
  leaseId: number,
): Promise<void> {
  await call(session.raw.revokeSessionLease(contextId, leaseId));
}

/**
 * Choose which observation classes the presenter should report.
 *
 * Asking for fewer is a privacy choice as much as a bandwidth one, so the mask is explicit
 * rather than a default.
 */
export async function setObservation(session: Session, mask: number): Promise<void> {
  await call(session.raw.setObservation(mask));
}

/**
 * The identity a resuming producer needs, for sessions established through a lease.
 *
 * Root sessions are deliberately non-resumable; the resume key itself never crosses this
 * boundary.
 */
export async function prepareResume(session: Session): Promise<{
  contextId: number;
  leaseId: number;
  sessionId: number;
  resumeGeneration: number;
}> {
  return (await call(session.raw.prepareResume())) as {
    contextId: number;
    leaseId: number;
    sessionId: number;
    resumeGeneration: number;
  };
}

/** The constant table, re-exported so callers can read a value by its SDK name. */
export const table = (): ReturnType<ReturnType<typeof native>["constantTable"]> =>
  native().constantTable();

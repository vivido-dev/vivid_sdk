/**
 * Control payload shapes.
 *
 * A payload is an integer-keyed map. The SDK hands back the scalar entries it can read directly
 * and the rest as their own deterministic-CBOR encodings, so nothing a relay must preserve
 * byte-for-byte is lost at this boundary. Sending runs the same idea in reverse.
 */

/** A control payload: scalar entries plus raw deterministic-CBOR bytes for everything else. */
export interface Payload {
  readonly scalars: readonly PayloadScalar[];
  readonly raw: readonly PayloadRaw[];
}

export interface PayloadScalar {
  readonly key: number;
  readonly unsigned?: number;
  readonly text?: string;
}

export interface PayloadRaw {
  readonly key: number;
  readonly value: Uint8Array;
}

/**
 * Scene geometry as a sendable payload.
 *
 * Geometry keys are the ones the surface's coordinate model defines, and the values are the
 * integers, text, or booleans that model uses. Anything else would be re-encoded as raw CBOR,
 * which a presenter would reject as the wrong shape rather than silently misread.
 */
export function encodeSceneGeometry(
  geometry: Readonly<Record<number, number | string | boolean>>,
): Payload {
  const scalars: PayloadScalar[] = [];
  for (const [rawKey, value] of Object.entries(geometry)) {
    const key = Number(rawKey);
    if (!Number.isSafeInteger(key) || key < 0) {
      throw new RangeError(`scene geometry key ${rawKey} is not a non-negative integer`);
    }
    if (typeof value === "string") {
      scalars.push({ key, text: value });
    } else if (typeof value === "boolean") {
      scalars.push({ key, unsigned: value ? 1 : 0 });
    } else if (Number.isSafeInteger(value) && value >= 0) {
      scalars.push({ key, unsigned: value });
    } else {
      throw new RangeError(
        `scene geometry value for key ${key} must be a non-negative safe integer, text, or a boolean`,
      );
    }
  }
  scalars.sort((left, right) => left.key - right.key);
  return { scalars, raw: [] };
}

/** The scalar entries of a payload as a plain object, for the fields a host reads by name. */
export function scalarFields(payload: Payload): Record<number, number | string> {
  const fields: Record<number, number | string> = {};
  for (const scalar of payload.scalars) {
    if (scalar.unsigned !== undefined) {
      fields[scalar.key] = scalar.unsigned;
    } else if (scalar.text !== undefined) {
      fields[scalar.key] = scalar.text;
    }
  }
  return fields;
}

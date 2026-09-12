/**
 * Senders that keep media identity continuous across channel recovery.
 *
 * A recovered channel is a new authenticated generation, but it is still the same track: packet
 * IDs must keep increasing and the media epoch must never move backward. `TrackSender` owns that
 * continuity, so a producer that reconnects does not have to reconstruct it.
 */

export { TrackSender, VideoRateControl } from "./index.js";

/** One microphone packet: 20 ms of 48 kHz mono s16LE. */
export const MIC_PACKET_US = 20_000;
/** Bytes in one microphone packet. */
export const MIC_PACKET_BYTES = 960;

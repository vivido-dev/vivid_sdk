/**
 * Protocol constants, read from the SDK's table at import.
 *
 * The names are this package's API; the values are not. Reading them from the addon is what
 * keeps them from drifting from `vivid_protocol`: a rename there fails this import rather than
 * silently changing a wire value here. `constants.test.mjs` pins the property from the other
 * side, by checking that every name the table carries is re-exported.
 */

import { native } from "./native.js";
import type { ConstantEntry } from "./native.js";

const table: ConstantEntry[] = native().constantTable();

const byName = new Map<string, ConstantEntry>(table.map((entry) => [entry.name, entry]));

function textOf(name: string): string {
  const entry = byName.get(name);
  if (entry === undefined || entry.text === null) {
    throw new Error(`${name} is not a profile constant in the SDK table`);
  }
  return entry.text;
}

function numberOf(name: string): number {
  const entry = byName.get(name);
  if (entry === undefined || entry.number === null) {
    throw new Error(`${name} is not a numeric constant in the SDK table`);
  }
  return entry.number;
}

// -- Negotiated profiles ----------------------------------------------------
export const PROFILE_CORE: string = textOf("PROFILE_CORE");
export const PROFILE_TERMINAL_SURFACE: string = textOf("PROFILE_TERMINAL_SURFACE");
export const PROFILE_DESKTOP_SURFACE: string = textOf("PROFILE_DESKTOP_SURFACE");
export const PROFILE_CANVAS_SURFACE: string = textOf("PROFILE_CANVAS_SURFACE");
export const PROFILE_LIVE_MEDIA: string = textOf("PROFILE_LIVE_MEDIA");
export const PROFILE_TIMED_MEDIA: string = textOf("PROFILE_TIMED_MEDIA");
export const PROFILE_TIMED_MEDIA_SYNC: string = textOf("PROFILE_TIMED_MEDIA_SYNC");
export const PROFILE_AUDIO_GAIN: string = textOf("PROFILE_AUDIO_GAIN");
export const PROFILE_AUDIO_INPUT: string = textOf("PROFILE_AUDIO_INPUT");
export const PROFILE_DESKTOP_INPUT: string = textOf("PROFILE_DESKTOP_INPUT");
export const PROFILE_FILE_DROP: string = textOf("PROFILE_FILE_DROP");
export const PROFILE_FILE_DROP_PATH: string = textOf("PROFILE_FILE_DROP_PATH");
export const PROFILE_OBSERVABILITY: string = textOf("PROFILE_OBSERVABILITY");
export const PROFILE_WEB_CARRIER: string = textOf("PROFILE_WEB_CARRIER");
export const PROFILE_TERMINAL_OVERLAY: string = textOf("PROFILE_TERMINAL_OVERLAY");
export const PROFILE_VECTOR_SCENE: string = textOf("PROFILE_VECTOR_SCENE");
export const PROFILE_OVERLAY_INPUT: string = textOf("PROFILE_OVERLAY_INPUT");
export const PROFILE_OVERLAY_TEXT: string = textOf("PROFILE_OVERLAY_TEXT");
export const PROFILE_OVERLAY_TEXT_LAYOUT: string = textOf("PROFILE_OVERLAY_TEXT_LAYOUT");
export const PROFILE_OVERLAY_TYPOGRAPHY: string = textOf("PROFILE_OVERLAY_TYPOGRAPHY");
export const PROFILE_OVERLAY_PAINT: string = textOf("PROFILE_OVERLAY_PAINT");
export const PROFILE_OVERLAY_POINTER: string = textOf("PROFILE_OVERLAY_POINTER");
export const PROFILE_OVERLAY_CLIPBOARD: string = textOf("PROFILE_OVERLAY_CLIPBOARD");
export const PROFILE_OVERLAY_ENV: string = textOf("PROFILE_OVERLAY_ENV");

// -- Surface semantic profiles ---------------------------------------------
export const SURFACE_GENERIC: string = textOf("SURFACE_GENERIC");
export const SURFACE_TERMINAL: string = textOf("SURFACE_TERMINAL");
export const SURFACE_DESKTOP: string = textOf("SURFACE_DESKTOP");
export const SURFACE_CANVAS: string = textOf("SURFACE_CANVAS");

// -- Coordinate models -----------------------------------------------------
export const COORDINATE_DESKTOP_LOGICAL_PIXELS: number = numberOf("COORDINATE_DESKTOP_LOGICAL_PIXELS");
export const COORDINATE_NORMALIZED: number = numberOf("COORDINATE_NORMALIZED");
export const COORDINATE_CANVAS_LOGICAL_UNITS: number = numberOf("COORDINATE_CANVAS_LOGICAL_UNITS");
export const COORDINATE_TERMINAL_CONTENT_CELLS: number = numberOf("COORDINATE_TERMINAL_CONTENT_CELLS");

// -- Descriptor roles ------------------------------------------------------
export const ROLE_UNSPECIFIED: number = numberOf("ROLE_UNSPECIFIED");
export const ROLE_DOCUMENT: number = numberOf("ROLE_DOCUMENT");
export const ROLE_DESKTOP: number = numberOf("ROLE_DESKTOP");
export const ROLE_TIMED_MEDIA: number = numberOf("ROLE_TIMED_MEDIA");
export const ROLE_FIGURE: number = numberOf("ROLE_FIGURE");
export const ROLE_TERMINAL: number = numberOf("ROLE_TERMINAL");
export const ROLE_CANVAS: number = numberOf("ROLE_CANVAS");

// -- Capture and export policies -------------------------------------------
export const POLICY_DENY_CAPTURE: number = numberOf("POLICY_DENY_CAPTURE");
export const POLICY_DENY_DESCRIPTOR_EXPORT: number = numberOf("POLICY_DENY_DESCRIPTOR_EXPORT");
export const POLICY_DENY_POSTER_RETENTION: number = numberOf("POLICY_DENY_POSTER_RETENTION");
export const POLICY_DENY_IMAGE_CACHE: number = numberOf("POLICY_DENY_IMAGE_CACHE");
export const POLICY_REDUCED_DIAGNOSTICS: number = numberOf("POLICY_REDUCED_DIAGNOSTICS");
export const POLICY_KNOWN_MASK: number = numberOf("POLICY_KNOWN_MASK");

// -- Track modes, directions, kinds, lanes, slots ---------------------------
export const TRACK_MODE_LIVE: number = numberOf("TRACK_MODE_LIVE");
export const TRACK_MODE_TIMED: number = numberOf("TRACK_MODE_TIMED");
export const TRACK_DIRECTION_DOWNLINK: number = numberOf("TRACK_DIRECTION_DOWNLINK");
export const TRACK_DIRECTION_UPLINK: number = numberOf("TRACK_DIRECTION_UPLINK");
export const TRACK_KIND_VIDEO: number = numberOf("TRACK_KIND_VIDEO");
export const TRACK_KIND_AUDIO: number = numberOf("TRACK_KIND_AUDIO");
export const TRACK_KIND_RASTER: number = numberOf("TRACK_KIND_RASTER");
export const TRACK_KIND_IMAGE: number = numberOf("TRACK_KIND_IMAGE");
export const TRACK_KIND_VECTOR: number = numberOf("TRACK_KIND_VECTOR");
export const LANE_CONTROL: number = numberOf("LANE_CONTROL");
export const LANE_INTERACTIVE: number = numberOf("LANE_INTERACTIVE");
export const LANE_REALTIME: number = numberOf("LANE_REALTIME");
export const LANE_BULK: number = numberOf("LANE_BULK");
export const SLOT_NONE: number = numberOf("SLOT_NONE");
export const SLOT_PRIMARY_VIDEO: number = numberOf("SLOT_PRIMARY_VIDEO");
export const SLOT_AUDIO: number = numberOf("SLOT_AUDIO");
export const SLOT_RASTER: number = numberOf("SLOT_RASTER");
export const SLOT_POSTER: number = numberOf("SLOT_POSTER");
export const SLOT_VECTOR: number = numberOf("SLOT_VECTOR");

// -- Scene node fit --------------------------------------------------------
export const FIT_FILL: number = numberOf("FIT_FILL");
export const FIT_CONTAIN: number = numberOf("FIT_CONTAIN");
export const FIT_COVER: number = numberOf("FIT_COVER");
export const FIT_NONE: number = numberOf("FIT_NONE");

// -- Encoded-image encodings ----------------------------------------------
export const IMAGE_PNG: number = numberOf("IMAGE_PNG");
export const IMAGE_JPEG: number = numberOf("IMAGE_JPEG");

// -- Readiness milestones --------------------------------------------------
export const MILESTONE_CHANNEL_ACCEPTED: number = numberOf("MILESTONE_CHANNEL_ACCEPTED");
export const MILESTONE_FIRST_MEDIA: number = numberOf("MILESTONE_FIRST_MEDIA");
export const MILESTONE_DECODER_INITIALIZED: number = numberOf("MILESTONE_DECODER_INITIALIZED");
export const MILESTONE_RANDOM_ACCESS: number = numberOf("MILESTONE_RANDOM_ACCESS");
export const MILESTONE_OUTPUT_READY: number = numberOf("MILESTONE_OUTPUT_READY");
export const MILESTONE_PRESENTED: number = numberOf("MILESTONE_PRESENTED");
export const MILESTONE_CLOCK_STARTED: number = numberOf("MILESTONE_CLOCK_STARTED");
export const MILESTONE_EOS_ACCEPTED: number = numberOf("MILESTONE_EOS_ACCEPTED");
export const MILESTONE_BUFFERED_ENDED: number = numberOf("MILESTONE_BUFFERED_ENDED");
export const MILESTONE_CHANNEL_DETACHED: number = numberOf("MILESTONE_CHANNEL_DETACHED");
export const MILESTONE_TRACK_LOST: number = numberOf("MILESTONE_TRACK_LOST");
export const MILESTONE_KNOWN_MASK: number = numberOf("MILESTONE_KNOWN_MASK");

// -- Track wait conditions -------------------------------------------------
export const WAIT_REVISION_GREATER: number = numberOf("WAIT_REVISION_GREATER");
export const WAIT_MILESTONE_SET: number = numberOf("WAIT_MILESTONE_SET");
export const WAIT_RASTER_FRAME_PRESENTED: number = numberOf("WAIT_RASTER_FRAME_PRESENTED");
export const WAIT_VIDEO_PTS_PRESENTED: number = numberOf("WAIT_VIDEO_PTS_PRESENTED");
export const WAIT_PLAYBACK_STARTED: number = numberOf("WAIT_PLAYBACK_STARTED");
export const WAIT_PLAYBACK_ENDED: number = numberOf("WAIT_PLAYBACK_ENDED");
export const WAIT_CHANNEL_ACCEPTED: number = numberOf("WAIT_CHANNEL_ACCEPTED");
export const WAIT_CHANNEL_CLOSED: number = numberOf("WAIT_CHANNEL_CLOSED");
export const WAIT_TRACK_LOST: number = numberOf("WAIT_TRACK_LOST");
export const MAX_TRACK_WAIT_TIMEOUT_US: number = numberOf("MAX_TRACK_WAIT_TIMEOUT_US");

// -- Context operation classes --------------------------------------------
export const OP_OBSERVE: number = numberOf("OP_OBSERVE");
export const OP_SURFACE_TRACK_MEDIA: number = numberOf("OP_SURFACE_TRACK_MEDIA");
export const OP_SCENE: number = numberOf("OP_SCENE");
export const OP_TERMINAL_ANCHOR: number = numberOf("OP_TERMINAL_ANCHOR");
export const OP_DESKTOP_INPUT: number = numberOf("OP_DESKTOP_INPUT");
export const OP_DELEGATE: number = numberOf("OP_DELEGATE");
export const OP_RECEIVE_FILE_DROP: number = numberOf("OP_RECEIVE_FILE_DROP");
export const OP_KNOWN_MASK: number = numberOf("OP_KNOWN_MASK");

// -- Input classes and watchdog bounds ------------------------------------
export const INPUT_CLASS_KEYBOARD: number = numberOf("INPUT_CLASS_KEYBOARD");
export const INPUT_CLASS_POINTER_MOTION: number = numberOf("INPUT_CLASS_POINTER_MOTION");
export const INPUT_CLASS_POINTER_BUTTON: number = numberOf("INPUT_CLASS_POINTER_BUTTON");
export const INPUT_CLASS_POINTER_AXIS: number = numberOf("INPUT_CLASS_POINTER_AXIS");
export const INPUT_CLASS_KNOWN_MASK: number = numberOf("INPUT_CLASS_KNOWN_MASK");
export const MIN_WATCHDOG_US: number = numberOf("MIN_WATCHDOG_US");
export const MAX_WATCHDOG_US: number = numberOf("MAX_WATCHDOG_US");

// -- Terminal placement and rate control ----------------------------------
export const COORDINATE_SPACE_GRID_CELL: number = numberOf("COORDINATE_SPACE_GRID_CELL");
export const TEXT_LAYER_BETWEEN_BACKGROUND_AND_GLYPH: number = numberOf("TEXT_LAYER_BETWEEN_BACKGROUND_AND_GLYPH");
export const MINIMUM_TARGET_BITS_PER_SECOND: number = numberOf("MINIMUM_TARGET_BITS_PER_SECOND");
export const DEFAULT_ACTIVATION_TIMEOUT_US: number = numberOf("DEFAULT_ACTIVATION_TIMEOUT_US");
export const MAX_ACTIVATION_TIMEOUT_US: number = numberOf("MAX_ACTIVATION_TIMEOUT_US");

/** Every constant's name, for tests that check the package re-exports the table. */
export function constantNames(): readonly string[] {
  return table.map((entry) => entry.name);
}

// Delegation, file drop, and microphone packet shape.
export const CLEANUP_IMMEDIATE: number = numberOf("CLEANUP_IMMEDIATE");
export const CLEANUP_SUSPEND_ON_UNCLEAN_LOSS: number = numberOf("CLEANUP_SUSPEND_ON_UNCLEAN_LOSS");
export const DESTINATION_SHELL_CWD: number = numberOf("DESTINATION_SHELL_CWD");
export const DESTINATION_DESKTOP_FOLDER: number = numberOf("DESTINATION_DESKTOP_FOLDER");
export const DROP_OFFERED: number = numberOf("DROP_OFFERED");
export const DROP_ACCEPTED: number = numberOf("DROP_ACCEPTED");
export const DROP_TRANSFERRING: number = numberOf("DROP_TRANSFERRING");
export const DROP_COMMITTED: number = numberOf("DROP_COMMITTED");
export const DROP_CANCELLED: number = numberOf("DROP_CANCELLED");
export const DROP_FAILED: number = numberOf("DROP_FAILED");
export const MIC_PACKET_US: number = numberOf("MIC_PACKET_US");
export const MIC_PACKET_BYTES: number = numberOf("MIC_PACKET_BYTES");

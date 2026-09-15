/** Pane-local overlays. Transport uses native workers; callbacks stay on the application loop. */
import { call, callSync, native } from "./native.js";
import { Session } from "./index.js";
import type { ConnectOptions } from "./index.js";
import { PROFILE_CORE, PROFILE_LIVE_MEDIA, PROFILE_TERMINAL_SURFACE, PROFILE_TERMINAL_OVERLAY, PROFILE_VECTOR_SCENE, PROFILE_OVERLAY_INPUT, PROFILE_OVERLAY_TEXT, PROFILE_OVERLAY_TEXT_LAYOUT, PROFILE_OVERLAY_TYPOGRAPHY, PROFILE_OVERLAY_PAINT, PROFILE_OVERLAY_POINTER } from "./constants.js";

export interface Point { readonly x: number; readonly y: number }
export interface Rect extends Point { readonly width: number; readonly height: number }
export interface Viewport { readonly width: number; readonly height: number; readonly scaleNumerator: number; readonly scaleDenominator: number }
export type WindowMode = "floating" | "popup" | "modal";
export type HitRole = "input" | "drag" | "resize" | "transparent";
export type ScrollPhase = "none" | "began" | "changed" | "ended" | "cancelled";
export type CursorShape =
  | "default" | "pointer" | "text" | "move" | "crosshair" | "not-allowed"
  | "grab" | "grabbing" | "wait" | "progress"
  | "resize-left" | "resize-right" | "resize-up" | "resize-down"
  | "resize-up-left" | "resize-up-right" | "resize-down-left" | "resize-down-right"
  | "resize-left-right" | "resize-up-down";
const SCROLL_PHASES: readonly ScrollPhase[] = ["none", "began", "changed", "ended", "cancelled"];

/** Normative overlay modifier bits. A host never forwards its platform bitmask. */
export const Modifiers = { shift: 1, control: 2, alt: 4, super: 8, capsLock: 16, numLock: 32, knownMask: 63 } as const;

/** Normative overlay pointer buttons, shared with desktop-surface-v1. */
export const MouseButton = { primary: 0, auxiliary: 1, secondary: 2, back: 3, forward: 4, maximum: 31 } as const;

/** Physical keys are USB HID keyboard-page usages; zero is a key the page does not name. */
export const Key = { unmapped: 0, firstUsage: 0x04, lastUsage: 0xe7 } as const;

export interface OverlayWindowOptions { readonly bounds: Rect; readonly mode?: WindowMode; readonly title?: string; readonly visible?: boolean; readonly minWidth?: number; readonly minHeight?: number }
export interface GradientStop { readonly offset: number; readonly color: number }
interface NativeCanvas {
  snapshot(): NativeCanvas;
  draw(path: readonly (readonly number[])[], evenOdd: boolean, kind: string, geometry: readonly number[], colors: readonly number[], offsets: readonly number[], width?: number): void;
  drawPaint(path: readonly (readonly number[])[], evenOdd: boolean, kind: string, geometry: readonly number[], colors: readonly number[], offsets: readonly number[], space: string, imageAsset: bigint | undefined, imageTransform: readonly number[] | undefined, extend: string, width: number | undefined, cap: string, join: string, miter: number, dashes: readonly number[], dashOffset: number): void;
  shadow(values: readonly number[], color: number): void;
  state(kind: string, values: number[]): void;
  clip(path: readonly (readonly number[])[], evenOdd: boolean): void;
  text(text: string, x: number, y: number, size: number, color: number, family: string, weight: number, italic: boolean, maxWidth?: number): void;
  hit(path: readonly (readonly number[])[], evenOdd: boolean, id: bigint, role: string, edges: number, cursor: string): void;
  validate(): void;
}
interface CanvasConstructor { new(): NativeCanvas; shape(kind: string, bounds: number[], radius: number): number[][] }
/** A retained image handle. The asset identity stays a bigint all the way to the wire. */
interface NativeImage { readonly id: bigint }
interface NativeWindow {
  textBatch(texts: object[], retain: boolean): Promise<NativeTextLayout[]>;
  drawTextLayout(canvas: NativeCanvas, layout: NativeTextLayout, x: number, y: number): Promise<void>;
  releaseTextLayout(layout: NativeTextLayout): Promise<void>;
  measureText(canvas: NativeCanvas): Promise<{ width: number; height: number; lines: number[][]; clusters: number[][] }>;
  setEditorGeometry(sceneRevision: bigint, caret?: number[]): Promise<void>;
  submit(canvas: NativeCanvas): Promise<NativeSubmission>;
  replaceTrack(canvas: NativeCanvas): Promise<NativeSubmission>;
  releaseImage(image: object): Promise<void>;
  reconcile(): Promise<Omit<OverlayWindowStatus, "bounds" | "viewport"> & { bounds: number[]; viewport: number[] }>;
  present(canvas: NativeCanvas): Promise<void>;
  setBounds(bounds: number[]): Promise<void>;
  setVisible(visible: boolean): Promise<void>;
  action(action: string): Promise<void>;
  bounds(): Promise<number[]>;
  viewport(): Promise<number[]>;
  uploadRgba(width: number, height: number, rgba: Buffer): Promise<NativeImage>;
  drawImage(canvas: NativeCanvas, image: object, bounds: number[], opacity: number): Promise<void>;
}
interface NativeTextLayout { measurement(): { width: number; height: number; lines: number[][]; clusters: number[][]; truncatedAt?: number } }
export interface TextStyle {
  readonly size?: number; readonly family?: string; readonly weight?: number;
  readonly italic?: boolean; readonly color?: number; readonly underline?: boolean; readonly strikethrough?: boolean;
}
export interface TextRun { readonly text: string; readonly style?: TextStyle }
export interface StyledText {
  readonly overflow?: "clip" | "ellipsis";
  readonly letterSpacing?: number; readonly wordSpacing?: number; readonly lineHeight?: number;
  readonly ligatures?: boolean; readonly kerning?: boolean;
  readonly runs: readonly TextRun[]; readonly maxWidth?: number;
  readonly alignment?: "start" | "center" | "end" | "justify";
  readonly wrap?: boolean;
  /** Clip after this many complete lines. */
  readonly maxLines?: number;
}
function nativeStyledText(text: StyledText): object {
  if (text.maxLines !== undefined && (!Number.isInteger(text.maxLines) || text.maxLines < 1 || text.maxLines > 1024)) throw new RangeError("maxLines must be an integer in [1, 1024]");
  const canvas = new Canvas();
  const decorations: number[] = [];
  for (const run of text.runs) {
    const style = run.style ?? {};
    canvas.text(run.text, { x: 0, y: 0 }, style.size ?? 16, style.color ?? 0xffffffff,
      { family: style.family ?? "", weight: style.weight ?? 400, italic: style.italic ?? false });
    decorations.push(Number(Boolean(style.underline)) | (Number(Boolean(style.strikethrough)) << 1));
  }
  const Type = native().OverlayStyledText as { new(canvas: NativeCanvas, decorations: number[], maxWidth: number | undefined, alignment: string, wrap: boolean, maxLines: number | undefined): { typography(overflow: string, letterSpacing: number, wordSpacing: number, lineHeight: number | undefined, ligatures: boolean, kerning: boolean): void } };
  const result = new Type(canvas.raw, decorations, text.maxWidth, text.alignment ?? "start", text.wrap ?? true, text.maxLines);
  result.typography(text.overflow ?? "clip", text.letterSpacing ?? 0, text.wordSpacing ?? 0, text.lineHeight, text.ligatures ?? true, text.kerning ?? true);
  return result;
}
function layoutMeasurement(raw: NativeTextLayout): TextMeasurement {
  const measured = raw.measurement();
  return { ...measured, lines: measured.lines.map(textGeometry), clusters: measured.clusters.map(textGeometry) };
}
export class RetainedTextLayout {
  readonly measurement: TextMeasurement;
  /** @internal */ constructor(readonly raw: NativeTextLayout) { this.measurement = layoutMeasurement(raw); }
}
interface NativeSession {
  createWindow(bounds: number[], mode: string, title: string, visible: boolean, minWidth: number, minHeight: number, parent?: NativeWindow): Promise<NativeWindow>;
  capturePointer(window: NativeWindow, capture: boolean): Promise<void>;
  waitEvent(timeout: number): Promise<NativeEvent | null>;
  close(): Promise<void>;
}
interface EventData { kind: string; revision: bigint; region: bigint; values: number[]; text: string }
interface NativeEvent { targets(window: NativeWindow): boolean; data(): EventData }
const values = (rect: Rect): number[] => [rect.x, rect.y, rect.width, rect.height];
const asRect = (v: number[]): Rect => ({ x: v[0]!, y: v[1]!, width: v[2]!, height: v[3]! });
const canvasType = (): CanvasConstructor => native().OverlayCanvas as CanvasConstructor;

export class Path {
  /** @internal Native conversion data; no protocol encoding occurs in JavaScript. */
  readonly segments: number[][] = [];
  constructor(readonly evenOdd = false) {}
  private add(...segment: number[]): this {
    if (this.segments.length >= 4096) throw new RangeError("path segment limit exceeded");
    this.segments.push(segment); return this;
  }
  moveTo(x: number, y: number): this { return this.add(0, x, y); }
  lineTo(x: number, y: number): this { return this.add(1, x, y); }
  quadTo(cx: number, cy: number, x: number, y: number): this { return this.add(2, cx, cy, x, y); }
  cubicTo(ax: number, ay: number, bx: number, by: number, x: number, y: number): this { return this.add(3, ax, ay, bx, by, x, y); }
  close(): this { return this.add(4); }
  private static shape(kind: string, bounds: Rect, radius = 0): Path {
    const path = new Path(); path.segments.push(...callSync(() => canvasType().shape(kind, values(bounds), radius))); return path;
  }
  static rectangle(bounds: Rect): Path { return Path.shape("rectangle", bounds); }
  static roundedRectangle(bounds: Rect, radius: number): Path { return Path.shape("rounded", bounds, radius); }
  /** Radii are clockwise from the top left and scale down together when they overrun their sides. */
  static roundedRectangleCorners(bounds: Rect, radii: readonly [number, number, number, number]): Path {
    const path = new Path();
    path.segments.push(...callSync(() => canvasType().shape("rounded-corners", [...values(bounds), ...radii], 0)));
    return path;
  }
  static ellipse(bounds: Rect): Path { return Path.shape("ellipse", bounds); }
}

/** Colors are straight-alpha sRGB 0xRRGGBBAA; offsets are in [0, 1]. */
export type GradientSpace = "srgb" | "oklab";
export type ImageExtend = "pad" | "repeat" | "reflect";
export class Brush {
  private constructor(
    readonly kind: string,
    readonly geometry: readonly number[],
    readonly colors: readonly number[],
    readonly offsets: readonly number[],
    readonly space: GradientSpace = "srgb",
    readonly image?: { readonly asset: bigint; readonly transform?: readonly number[]; readonly extend: ImageExtend },
  ) {}
  static solid(color: number): Brush { return new Brush("solid", [], [color], []); }
  static linear(start: Point, end: Point, stops: readonly GradientStop[], space: GradientSpace = "srgb"): Brush { return new Brush("linear", [start.x, start.y, end.x, end.y], stops.map(s => s.color), stops.map(s => s.offset), space); }
  static radial(center: Point, radius: number, stops: readonly GradientStop[], space: GradientSpace = "srgb"): Brush { return new Brush("radial", [center.x, center.y, radius], stops.map(s => s.color), stops.map(s => s.offset), space); }
  /** Fill any path with an uploaded image. `transform` is six affine terms. */
  static image(image: RetainedImage, transform?: readonly number[], extend: ImageExtend = "pad"): Brush {
    return new Brush("image", [], [], [], "srgb", { asset: image.id, transform, extend });
  }
}
/** One blurred rounded rectangle, as CSS box-shadow defines it. */
export interface Shadow {
  readonly rect: Rect;
  /** Clockwise from the top left. */
  readonly radii?: readonly [number, number, number, number];
  readonly color?: number;
  readonly offset?: Point;
  readonly blur?: number;
  readonly spread?: number;
  readonly inset?: boolean;
}
/** Caps are butt/round/square; joins are miter/bevel/round; dashes alternate on/off lengths. */
export interface StrokeStyle {
  readonly width: number;
  readonly cap?: "butt" | "round" | "square";
  readonly join?: "miter" | "bevel" | "round";
  readonly miterLimit?: number;
  readonly dashes?: readonly number[];
  readonly dashOffset?: number;
}
export interface TextOptions { readonly family?: string; readonly weight?: number; readonly italic?: boolean; readonly maxWidth?: number }
/** Ranges use JavaScript UTF-16 indexes. Geometry is layout-local logical pixels. */
export interface TextGeometry { readonly start: number; readonly end: number; readonly bounds: Rect; readonly baseline: number; readonly rtl: boolean }
export interface TextMeasurement { readonly width: number; readonly height: number; readonly lines: readonly TextGeometry[]; readonly clusters: readonly TextGeometry[]; readonly truncatedAt?: number }
const textGeometry = (v: number[]): TextGeometry => ({ start: v[0]!, end: v[1]!, bounds: asRect(v.slice(2, 6)), baseline: v[6]!, rtl: Boolean(v[7]) });
export class Canvas {
  /** @internal */ readonly raw: NativeCanvas;
  constructor(raw?: NativeCanvas) { this.raw = raw ?? callSync(() => new (canvasType())()); }
  snapshot(): Canvas { return new Canvas(callSync(() => this.raw.snapshot())); }
  private paint(path: Path, brush: Brush, width: number | undefined, style: Partial<StrokeStyle> = {}): void {
    callSync(() => this.raw.drawPaint(
      path.segments, path.evenOdd, brush.kind, brush.geometry, brush.colors, brush.offsets, brush.space,
      brush.image?.asset, brush.image?.transform, brush.image?.extend ?? "pad",
      width, style.cap ?? "butt", style.join ?? "miter", style.miterLimit ?? 4, style.dashes ?? [], style.dashOffset ?? 0,
    ));
  }
  fill(path: Path, brush: Brush): this { this.paint(path, brush, undefined); return this; }
  stroke(path: Path, brush: Brush, width: number): this { this.paint(path, brush, width); return this; }
  /** Cast a blurred rounded rectangle. Draw the element over its own shadow separately. */
  shadow(value: Shadow): this {
    const r = value.radii ?? [0, 0, 0, 0];
    const offset = value.offset ?? { x: 0, y: 0 };
    callSync(() => this.raw.shadow(
      [...values(value.rect), ...r, offset.x, offset.y, value.blur ?? 0, value.spread ?? 0, value.inset ? 1 : 0],
      value.color ?? 0x000000ff,
    ));
    return this;
  }
  /** Stroke with caps, joins, and dashes. */
  strokeStyled(path: Path, brush: Brush, style: StrokeStyle): this { this.paint(path, brush, style.width, style); return this; }
  save(): this { callSync(() => this.raw.state("save", [])); return this; }
  restore(): this { callSync(() => this.raw.state("restore", [])); return this; }
  opacity(value: number): this { callSync(() => this.raw.state("opacity", [value])); return this; }
  transform(a: number, b: number, c: number, d: number, e: number, f: number): this { callSync(() => this.raw.state("transform", [a, b, c, d, e, f])); return this; }
  clip(path: Path): this { callSync(() => this.raw.clip(path.segments, path.evenOdd)); return this; }
  text(text: string, origin: Point, size: number, color: number, options: TextOptions = {}): this {
    callSync(() => this.raw.text(text, origin.x, origin.y, size, color, options.family ?? "", options.weight ?? 400, options.italic ?? false, options.maxWidth)); return this;
  }
  /**
   * Resize edge mask: left=1, right=2, top=4, bottom=8. IDs never pass through Number.
   *
   * `cursor` is the shape shown while this region is hovered; omitting it leaves the host's own.
   */
  hit(applicationId: bigint, path: Path, role: HitRole = "input", edges = 0, cursor?: CursorShape): this {
    // An omitted cursor is no cursor, not a default one: declaring a shape is what makes the
    // region require the pointer profile.
    callSync(() => this.raw.hit(path.segments, path.evenOdd, applicationId, role, edges, cursor ?? ""));
    return this;
  }
  validate(): void { callSync(() => this.raw.validate()); }
}
export class RetainedImage {
  /** @internal */ constructor(readonly raw: NativeImage) {}
  /** The channel-qualified asset identity, at full unsigned width. */
  get id(): bigint { return this.raw.id; }
}

export type PresentationOutcome = "presented" | "superseded";
interface NativeSubmission { readonly revision: bigint; wait(timeout: number): Promise<PresentationOutcome | null> }
export class OverlaySubmission {
  /** @internal */ constructor(private readonly raw: NativeSubmission) {}
  get revision(): bigint { return this.raw.revision; }
  /** Timeout returns undefined and leaves the receipt usable. Lane loss rejects the wait. */
  async wait(timeout = 0.25): Promise<PresentationOutcome | undefined> { return (await call(this.raw.wait(timeout))) ?? undefined; }
}
export interface OverlayWindowStatus {
  readonly bounds: Rect; readonly viewport: Viewport; readonly viewportRevision: bigint;
  readonly windowRevision: bigint; readonly presentedRevision: bigint; readonly acceptedRevision: bigint;
  readonly activeRevision?: bigint; readonly focused: boolean;
}
const asViewport = (v: number[]): Viewport => ({ width: v[0]!, height: v[1]!, scaleNumerator: v[2]!, scaleDenominator: v[3]! });

interface EventBase { readonly sceneRevision: bigint; targets(window: OverlayWindow): boolean }
export type OverlayEvent = EventBase & (
  | { readonly kind: "pointer"; readonly position: Point; readonly applicationId: bigint; readonly modifiers: number; readonly button?: number; readonly down?: boolean; readonly clicks: number; readonly pressure?: number }
  | { readonly kind: "hover"; readonly applicationId: bigint; readonly entered: boolean }
  | { readonly kind: "wheel"; readonly position: Point; readonly dx: number; readonly dy: number; readonly modifiers: number; readonly precise: boolean; readonly phase: ScrollPhase }
  | { readonly kind: "key"; readonly physical: number; readonly down: boolean; readonly repeat: boolean; readonly modifiers: number }
  | { readonly kind: "text"; readonly text: string }
  | { readonly kind: "ime"; readonly preedit: string; /** UTF-16 offsets into preedit. */ readonly selection?: readonly [number, number] }
  | { readonly kind: "focus"; readonly focused: boolean }
  | { readonly kind: "geometry"; readonly bounds: Rect; readonly settled: boolean }
  | { readonly kind: "dismissed"; readonly reason: "escape" | "outside-press" | "closed" | "owner-lost" | "parent-closed" }
  | { readonly kind: "cancel" }
  | { readonly kind: "connection-lost"; readonly diagnostic: string }
  | { readonly kind: "viewport"; readonly revision: bigint; readonly viewport: Viewport }
  | { readonly kind: "submission-outcome"; readonly outcome: PresentationOutcome }
);
/** @internal Native conversion is exported only for binding regression tests. */
export function decodeOverlayEvent(raw: NativeEvent): OverlayEvent {
  const data = callSync(() => raw.data()), v = data.values;
  const base: EventBase = { sceneRevision: data.revision, targets: window => callSync(() => raw.targets(window.raw)) };
  const position = { x: v[0]!, y: v[1]! };
  switch (data.kind) {
    case "viewport": return { ...base, sceneRevision: 0n, kind: "viewport", revision: data.revision, viewport: asViewport(v) };
    case "submission-outcome": {
      if (data.text !== "presented" && data.text !== "superseded") throw new Error("unknown submission outcome");
      return { ...base, kind: "submission-outcome", outcome: data.text };
    }
    // Values are [x, y, modifiers, clicks] with an optional [button, down] pair, then the
    // pressure, which is negative when the device reported none.
    case "pointer": return {
      ...base, kind: "pointer", position, applicationId: data.region, modifiers: v[2]!,
      clicks: v[3]!, button: v[4],
      down: v.length > 5 ? Boolean(v[5]) : undefined,
      pressure: v[v.length - 1]! >= 0 ? v[v.length - 1] : undefined,
    };
    case "hover": return { ...base, kind: "hover", applicationId: data.region, entered: Boolean(v[0]) };
    case "wheel": return { ...base, kind: "wheel", position, dx: v[2]!, dy: v[3]!, modifiers: v[4]!, precise: v[5] !== 0, phase: SCROLL_PHASES[v[6]!]! };
    case "key": return { ...base, kind: "key", physical: v[0]!, down: Boolean(v[1]), repeat: Boolean(v[2]), modifiers: v[3]! };
    case "text": return { ...base, kind: "text", text: data.text };
    case "ime": {
      const bytes = Buffer.from(data.text, "utf8");
      return { ...base, kind: "ime", preedit: data.text, selection: v.length ? [bytes.subarray(0, v[0]).toString("utf8").length, bytes.subarray(0, v[1]).toString("utf8").length] : undefined };
    }
    case "geometry": return { ...base, kind: "geometry", bounds: asRect(v), settled: Boolean(v[4]) };
    case "focus": return { ...base, kind: "focus", focused: Boolean(v[0]) };
    case "dismissed": {
      const reason = data.text;
      if (reason !== "escape" && reason !== "outside-press" && reason !== "closed" && reason !== "owner-lost" && reason !== "parent-closed") throw new Error("unknown dismissal reason");
      return { ...base, kind: "dismissed", reason };
    }
    case "cancel": return { ...base, kind: "cancel" };
    case "connection-lost": return { ...base, kind: "connection-lost", diagnostic: data.text };
    default: throw new Error("unknown native overlay event");
  }
}

export class OverlaySession {
  private stopped = false;
  private constructor(private readonly raw: NativeSession) {}
  get closed(): boolean { return this.stopped; }
  static async connect(options: ConnectOptions = {}): Promise<OverlaySession> {
    const required = [...new Set([...(options.requiredProfiles ?? []), PROFILE_CORE, PROFILE_LIVE_MEDIA, PROFILE_TERMINAL_SURFACE, PROFILE_TERMINAL_OVERLAY, PROFILE_VECTOR_SCENE, PROFILE_OVERLAY_INPUT])].sort();
    const optional = [...new Set([...(options.optionalProfiles ?? []), PROFILE_OVERLAY_TEXT, PROFILE_OVERLAY_TEXT_LAYOUT, PROFILE_OVERLAY_TYPOGRAPHY, PROFILE_OVERLAY_PAINT, PROFILE_OVERLAY_POINTER].filter(p => !required.includes(p)))].sort();
    const session = await Session.connect({ ...options, targetProfile: PROFILE_TERMINAL_SURFACE, requiredProfiles: required, optionalProfiles: optional });
    const type = native().OverlaySession as { adopt(session: unknown): Promise<NativeSession> };
    return new OverlaySession(await call(type.adopt(session.raw)));
  }
  static async fromEnv(): Promise<OverlaySession> { return OverlaySession.connect(); }
  async createWindow(options: OverlayWindowOptions, parent?: OverlayWindow): Promise<OverlayWindow> {
    return new OverlayWindow(await call(this.raw.createWindow(values(options.bounds), options.mode ?? "floating", options.title ?? "", options.visible ?? true, options.minWidth ?? 1, options.minHeight ?? 1, parent?.raw)));
  }
  async capturePointer(window: OverlayWindow, capture = true): Promise<void> { await call(this.raw.capturePointer(window.raw, capture)); }
  /** timeout is seconds, bounded to [0, 60]. */
  async waitEvent(timeout = 0.25): Promise<OverlayEvent | undefined> {
    if (this.stopped) return undefined;
    const event = await call(this.raw.waitEvent(timeout));
    return event ? decodeOverlayEvent(event) : undefined;
  }
  async *events(timeout = 0.25): AsyncIterableIterator<OverlayEvent> {
    while (!this.stopped) {
      const event = await this.waitEvent(timeout);
      if (event) { yield event; if (event.kind === "connection-lost") return; }
    }
  }
  async close(): Promise<void> { if (!this.stopped) { this.stopped = true; await call(this.raw.close()); } }
  async [Symbol.asyncDispose](): Promise<void> { await this.close(); }
}
export class OverlayWindow {
  async measureTextBatch(texts: readonly StyledText[]): Promise<readonly TextMeasurement[]> {
    return (await call(this.raw.textBatch(texts.map(nativeStyledText), false))).map(layoutMeasurement);
  }
  async layoutTextBatch(texts: readonly StyledText[]): Promise<readonly RetainedTextLayout[]> {
    return (await call(this.raw.textBatch(texts.map(nativeStyledText), true))).map(raw => new RetainedTextLayout(raw));
  }
  async layoutText(text: StyledText): Promise<RetainedTextLayout> { return (await this.layoutTextBatch([text]))[0]!; }
  /** Await before modifying or submitting the Canvas. */
  async drawTextLayout(canvas: Canvas, layout: RetainedTextLayout, origin: Point): Promise<void> {
    await call(this.raw.drawTextLayout(canvas.raw, layout.raw, origin.x, origin.y));
  }
  async releaseTextLayout(layout: RetainedTextLayout): Promise<void> { await call(this.raw.releaseTextLayout(layout.raw)); }
  async measureText(text: string, size: number, options: TextOptions = {}): Promise<TextMeasurement> {
    const canvas = new Canvas().text(text, { x: 0, y: 0 }, size, 0xffffffff, options);
    const measured = await call(this.raw.measureText(canvas.raw));
    return { ...measured, lines: measured.lines.map(textGeometry), clusters: measured.clusters.map(textGeometry) };
  }
  async setEditorGeometry(sceneRevision: bigint, caret?: Rect): Promise<void> {
    await call(this.raw.setEditorGeometry(sceneRevision, caret ? values(caret) : undefined));
  }
  private stopped = false;
  /** @internal */ constructor(readonly raw: NativeWindow) {}
  get closed(): boolean { return this.stopped; }
  /** Success acknowledges submission and initial activation, not GPU presentation. */
  async present(canvas: Canvas): Promise<void> { await call(this.raw.present(canvas.raw)); }
  async submit(canvas: Canvas): Promise<OverlaySubmission> { return new OverlaySubmission(await call(this.raw.submit(canvas.raw))); }
  /** Prime and activate a fresh track. The replacement cannot reference old retained images. */
  async replaceTrack(canvas: Canvas): Promise<OverlaySubmission> { return new OverlaySubmission(await call(this.raw.replaceTrack(canvas.raw))); }
  async releaseImage(image: RetainedImage): Promise<void> { await call(this.raw.releaseImage(image.raw)); }
  async reconcile(): Promise<OverlayWindowStatus> {
    const state = await call(this.raw.reconcile()); return { ...state, bounds: asRect(state.bounds), viewport: asViewport(state.viewport) };
  }
  async setBounds(bounds: Rect): Promise<void> { await call(this.raw.setBounds(values(bounds))); }
  async setVisible(visible: boolean): Promise<void> { await call(this.raw.setVisible(visible)); }
  async center(): Promise<void> { await call(this.raw.action("center")); }
  async requestFocus(): Promise<void> { await call(this.raw.action("focus")); }
  async raise(): Promise<void> { await call(this.raw.action("raise")); }
  async lower(): Promise<void> { await call(this.raw.action("lower")); }
  async bounds(): Promise<Rect> { return asRect(await call(this.raw.bounds())); }
  async viewport(): Promise<Viewport> {
    const v = await call(this.raw.viewport()); return { width: v[0]!, height: v[1]!, scaleNumerator: v[2]!, scaleDenominator: v[3]! };
  }
  async uploadRgba(width: number, height: number, rgba: Uint8Array): Promise<RetainedImage> { return new RetainedImage(await call(this.raw.uploadRgba(width, height, Buffer.from(rgba)))); }
  /** Await before submitting or modifying this Canvas. Asset ownership is checked natively. */
  async drawImage(canvas: Canvas, image: RetainedImage, bounds: Rect, opacity = 1): Promise<void> { await call(this.raw.drawImage(canvas.raw, image.raw, values(bounds), opacity)); }
  async close(): Promise<void> { if (!this.stopped) { await call(this.raw.action("close")); this.stopped = true; } }
  async [Symbol.asyncDispose](): Promise<void> { await this.close(); }
}

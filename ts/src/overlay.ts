/** Pane-local overlays. Transport uses native workers; callbacks stay on the application loop. */
import { call, callSync, native } from "./native.js";
import { Session } from "./index.js";
import type { ConnectOptions } from "./index.js";
import { PROFILE_CORE, PROFILE_LIVE_MEDIA, PROFILE_TERMINAL_SURFACE, PROFILE_TERMINAL_OVERLAY, PROFILE_VECTOR_SCENE, PROFILE_OVERLAY_INPUT } from "./constants.js";

export interface Point { readonly x: number; readonly y: number }
export interface Rect extends Point { readonly width: number; readonly height: number }
export interface Viewport { readonly width: number; readonly height: number; readonly scaleNumerator: number; readonly scaleDenominator: number }
export type WindowMode = "floating" | "popup" | "modal";
export type HitRole = "input" | "drag" | "resize" | "transparent";
export interface OverlayWindowOptions { readonly bounds: Rect; readonly mode?: WindowMode; readonly title?: string; readonly visible?: boolean }
export interface GradientStop { readonly offset: number; readonly color: number }
interface NativeCanvas {
  snapshot(): NativeCanvas;
  draw(path: readonly (readonly number[])[], evenOdd: boolean, kind: string, geometry: readonly number[], colors: readonly number[], offsets: readonly number[], width?: number): void;
  state(kind: string, values: number[]): void;
  clip(path: readonly (readonly number[])[], evenOdd: boolean): void;
  text(text: string, x: number, y: number, size: number, color: number, family: string, weight: number, italic: boolean, maxWidth?: number): void;
  hit(path: readonly (readonly number[])[], evenOdd: boolean, id: bigint, role: string, edges: number): void;
  validate(): void;
}
interface CanvasConstructor { new(): NativeCanvas; shape(kind: string, bounds: number[], radius: number): number[][] }
interface NativeWindow {
  present(canvas: NativeCanvas): Promise<void>;
  setBounds(bounds: number[]): Promise<void>;
  setVisible(visible: boolean): Promise<void>;
  action(action: string): Promise<void>;
  bounds(): Promise<number[]>;
  viewport(): Promise<number[]>;
  uploadRgba(width: number, height: number, rgba: Buffer): Promise<object>;
  drawImage(canvas: NativeCanvas, image: object, bounds: number[], opacity: number): Promise<void>;
}
interface NativeSession {
  createWindow(bounds: number[], mode: string, title: string, visible: boolean, parent?: NativeWindow): Promise<NativeWindow>;
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
  static ellipse(bounds: Rect): Path { return Path.shape("ellipse", bounds); }
}

/** Colors are straight-alpha sRGB 0xRRGGBBAA; offsets are in [0, 1]. */
export class Brush {
  private constructor(readonly kind: string, readonly geometry: readonly number[], readonly colors: readonly number[], readonly offsets: readonly number[]) {}
  static solid(color: number): Brush { return new Brush("solid", [], [color], []); }
  static linear(start: Point, end: Point, stops: readonly GradientStop[]): Brush { return new Brush("linear", [start.x, start.y, end.x, end.y], stops.map(s => s.color), stops.map(s => s.offset)); }
  static radial(center: Point, radius: number, stops: readonly GradientStop[]): Brush { return new Brush("radial", [center.x, center.y, radius], stops.map(s => s.color), stops.map(s => s.offset)); }
}
export interface TextOptions { readonly family?: string; readonly weight?: number; readonly italic?: boolean; readonly maxWidth?: number }
export class Canvas {
  /** @internal */ readonly raw: NativeCanvas;
  constructor(raw?: NativeCanvas) { this.raw = raw ?? callSync(() => new (canvasType())()); }
  snapshot(): Canvas { return new Canvas(callSync(() => this.raw.snapshot())); }
  fill(path: Path, brush: Brush): this { callSync(() => this.raw.draw(path.segments, path.evenOdd, brush.kind, brush.geometry, brush.colors, brush.offsets)); return this; }
  stroke(path: Path, brush: Brush, width: number): this { callSync(() => this.raw.draw(path.segments, path.evenOdd, brush.kind, brush.geometry, brush.colors, brush.offsets, width)); return this; }
  save(): this { callSync(() => this.raw.state("save", [])); return this; }
  restore(): this { callSync(() => this.raw.state("restore", [])); return this; }
  opacity(value: number): this { callSync(() => this.raw.state("opacity", [value])); return this; }
  transform(a: number, b: number, c: number, d: number, e: number, f: number): this { callSync(() => this.raw.state("transform", [a, b, c, d, e, f])); return this; }
  clip(path: Path): this { callSync(() => this.raw.clip(path.segments, path.evenOdd)); return this; }
  text(text: string, origin: Point, size: number, color: number, options: TextOptions = {}): this {
    callSync(() => this.raw.text(text, origin.x, origin.y, size, color, options.family ?? "", options.weight ?? 400, options.italic ?? false, options.maxWidth)); return this;
  }
  /** Resize edge mask: left=1, right=2, top=4, bottom=8. IDs never pass through Number. */
  hit(applicationId: bigint, path: Path, role: HitRole = "input", edges = 0): this { callSync(() => this.raw.hit(path.segments, path.evenOdd, applicationId, role, edges)); return this; }
  validate(): void { callSync(() => this.raw.validate()); }
}
export class RetainedImage { /** @internal */ constructor(readonly raw: object) {} }

interface EventBase { readonly sceneRevision: bigint; targets(window: OverlayWindow): boolean }
export type OverlayEvent = EventBase & (
  | { readonly kind: "pointer"; readonly position: Point; readonly applicationId: bigint; readonly modifiers: number; readonly button?: number; readonly down?: boolean }
  | { readonly kind: "wheel"; readonly position: Point; readonly dx: number; readonly dy: number; readonly modifiers: number }
  | { readonly kind: "key"; readonly physical: number; readonly down: boolean; readonly repeat: boolean; readonly modifiers: number }
  | { readonly kind: "text"; readonly text: string }
  | { readonly kind: "ime"; readonly preedit: string; /** UTF-16 offsets into preedit. */ readonly selection?: readonly [number, number] }
  | { readonly kind: "focus"; readonly focused: boolean }
  | { readonly kind: "geometry"; readonly bounds: Rect; readonly settled: boolean }
  | { readonly kind: "dismissed"; readonly reason: "escape" | "outside-press" | "closed" | "owner-lost" | "parent-closed" }
  | { readonly kind: "cancel" }
  | { readonly kind: "connection-lost"; readonly diagnostic: string }
);
/** @internal Native conversion is exported only for binding regression tests. */
export function decodeOverlayEvent(raw: NativeEvent): OverlayEvent {
  const data = callSync(() => raw.data()), v = data.values;
  const base: EventBase = { sceneRevision: data.revision, targets: window => callSync(() => raw.targets(window.raw)) };
  const position = { x: v[0]!, y: v[1]! };
  switch (data.kind) {
    case "pointer": return { ...base, kind: "pointer", position, applicationId: data.region, modifiers: v[2]!, button: v[3], down: v.length > 3 ? Boolean(v[4]) : undefined };
    case "wheel": return { ...base, kind: "wheel", position, dx: v[2]!, dy: v[3]!, modifiers: v[4]! };
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
    const optional = [...new Set((options.optionalProfiles ?? []).filter(p => !required.includes(p)))].sort();
    const session = await Session.connect({ ...options, targetProfile: PROFILE_TERMINAL_SURFACE, requiredProfiles: required, optionalProfiles: optional });
    const type = native().OverlaySession as { adopt(session: unknown): Promise<NativeSession> };
    return new OverlaySession(await call(type.adopt(session.raw)));
  }
  static async fromEnv(): Promise<OverlaySession> { return OverlaySession.connect(); }
  async createWindow(options: OverlayWindowOptions, parent?: OverlayWindow): Promise<OverlayWindow> {
    return new OverlayWindow(await call(this.raw.createWindow(values(options.bounds), options.mode ?? "floating", options.title ?? "", options.visible ?? true, parent?.raw)));
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
  private stopped = false;
  /** @internal */ constructor(readonly raw: NativeWindow) {}
  get closed(): boolean { return this.stopped; }
  /** Success acknowledges submission and initial activation, not GPU presentation. */
  async present(canvas: Canvas): Promise<void> { await call(this.raw.present(canvas.raw)); }
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

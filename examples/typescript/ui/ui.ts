// The shared vocabulary of the vivid_ui example ports: tokens, hand-computed geometry, the
// interaction state machine, and the repaint loop.
//
// The Rust originals (`vivid_ui/examples`) sit on a toolkit: taffy layout, an element tree,
// retained images, an animation clock. TypeScript has none of that here, and adding a
// framework is not this directory's job — so every port computes its own geometry and drives
// the raw overlay API through this module. What it keeps from the toolkit is the *contract*,
// not the machinery:
//
// - The host does the hit testing. A frame paints hit regions; events come back naming one.
// - A press is active exactly while it is held over the element it started on; a release
//   anywhere ends it; a click is a release over the element the press started on.
// - Nothing repaints until something asks; anything that moves is a function of the window's
//   elapsed time, and a wait is a deadline the loop sleeps to — never a timer.
//
// Region ids and revisions are bigint, as the wire carries them. Colors are straight-alpha
// sRGB 0xRRGGBBAA numbers.
import { pathToFileURL } from "node:url";

import { overlay, OverlaySession } from "@vivido/vivid-sdk";
import { duration } from "../support.js";

const { Brush, Canvas, Path } = overlay;
type Rect = overlay.Rect;
type Point = overlay.Point;
type OverlayEvent = overlay.OverlayEvent;
type PointerEventT = Extract<OverlayEvent, { kind: "pointer" }>;
type HoverEventT = Extract<OverlayEvent, { kind: "hover" }>;
type OverlayWindow = overlay.OverlayWindow;
type CursorShape = overlay.CursorShape;
type HitRole = overlay.HitRole;
type TextMeasurement = overlay.TextMeasurement;
type RetainedImage = overlay.RetainedImage;
type RetainedTextLayout = overlay.RetainedTextLayout;
type StyledText = overlay.StyledText;
type SemanticNode = overlay.SemanticNode;

// --------------------------------------------------------------------------- tokens

export const PANEL_BG = 0x101018ff;
export const INSET_BG = 0x1a1a24ff;
export const CAPTION = 0x8080a0ff;
export const TEXT = 0xe6e6f0ff;
export const WHITE = 0xffffffff;
export const BUTTON = 0x4a5fd0ff;
export const BUTTON_HOVER = 0x5a6fe0ff;
export const BUTTON_ACTIVE = 0x3a4fc0ff;
export const FOCUS_RING = 0x8ecbff;

/** Resize edge bits, as the host's gesture handler reads them: left 1, top 2, right 4,
 *  bottom 8. (The protocol is the authority: vivid_protocol/src/overlay.rs `fn gesture`.) */
export const EDGE_LEFT = 1;
export const EDGE_TOP = 2;
export const EDGE_RIGHT = 4;
export const EDGE_BOTTOM = 8;

/** How long the loop sleeps when nothing is moving and nothing is pending. */
const IDLE_TICK = 0.2;

/** USB HID key usages, which is what a key event carries. */
export const Keys = {
  enter: 0x28,
  escape: 0x29,
  backspace: 0x2a,
  tab: 0x2b,
  space: 0x2c,
  left: 0x50,
  right: 0x4f,
  up: 0x52,
  down: 0x51,
  home: 0x4a,
  end: 0x4d,
  delete: 0x4c,
  /** The usage for a letter, so `Keys.letter("c")` is what cmd-C presses. */
  letter(ch: string): number {
    return 0x04 + (ch.toUpperCase().charCodeAt(0) - 65);
  },
} as const;

export const Mods = { shift: 1, control: 2, alt: 4, super: 8 } as const;

/** The platform's command modifier: super on macOS, control elsewhere. */
export function platform(): number {
  return process.platform === "darwin" ? Mods.super : Mods.control;
}

// ------------------------------------------------------------------------- geometry

export function rect(x: number, y: number, width: number, height: number): Rect {
  return { x, y, width, height };
}

export function inset(r: Rect, by: number): Rect {
  return rect(r.x + by, r.y + by, Math.max(r.width - 2 * by, 0), Math.max(r.height - 2 * by, 0));
}

/** The largest centered rect of this aspect that fits in `box` ("contain"). */
export function fitRect(width: number, height: number, box: Rect): Rect {
  if (width <= 0 || height <= 0) return rect(box.x, box.y, 0, 0);
  const scale = Math.min(box.width / width, box.height / height);
  const w = width * scale;
  const h = height * scale;
  return rect(box.x + (box.width - w) / 2, box.y + (box.height - h) / 2, w, h);
}

/** A linear gradient across `r` at `angle` degrees — the CSS convention, so 0 is upward and
 *  90 is rightward. The endpoint span is the box's extent along the gradient's own
 *  direction, exactly as the Rust toolkit computes it (`Style::gradient_line`). */
export function gradientBrush(
  r: Rect,
  angleDeg: number,
  stops: readonly overlay.GradientStop[],
): overlay.Brush {
  const radians = (angleDeg * Math.PI) / 180;
  const dx = Math.sin(radians);
  const dy = -Math.cos(radians);
  const extent = (Math.abs(dx) * r.width) / 2 + (Math.abs(dy) * r.height) / 2;
  const cx = r.x + r.width / 2;
  const cy = r.y + r.height / 2;
  return Brush.linear(
    { x: cx - dx * extent, y: cy - dy * extent },
    { x: cx + dx * extent, y: cy + dy * extent },
    stops,
  );
}

/** The shadow token scale, as the Rust toolkit names it: `[offsetX, offsetY, blur, spread]`. */
const SHADOW_TOKENS: Record<string, readonly [number, number, number, number]> = {
  xs: [0, 1, 2, 0],
  sm: [0, 1, 3, 0],
  base: [0, 4, 6, -1],
  md: [0, 6, 12, -2],
  lg: [0, 10, 15, -3],
  xl: [0, 20, 25, -5],
};

// ------------------------------------------------------------------------ animation

export type EasingKind = "linear" | "ease-in" | "ease-out" | "ease-in-out" | "back-out";

/** The five curves the Rust toolkit carries, ported exactly. */
export const Easing = {
  /** The eased value at progress `t` in 0..=1. The ends are exact for every curve. */
  apply(kind: EasingKind, t: number): number {
    const clamped = Math.min(Math.max(t, 0), 1);
    if (kind === "linear") return clamped;
    if (kind === "ease-in") return clamped * clamped * clamped;
    if (kind === "ease-out") return 1 - (1 - clamped) ** 3;
    if (kind === "ease-in-out") {
      return clamped < 0.5 ? 4 * clamped ** 3 : 1 - (-2 * clamped + 2) ** 3 / 2;
    }
    const c = 1.70158;
    return 1 + (c + 1) * (clamped - 1) ** 3 + c * (clamped - 1) ** 2;
  },
} as const;

export interface Animation {
  readonly duration: number;
  readonly easing?: EasingKind;
  readonly repeat?: "once" | "loop" | "ping-pong";
}

/** One animation, started at a moment on the window's clock. */
export class Running {
  constructor(readonly animation: Animation, public started: number) {}

  /** Raw progress through the whole run, 0..=1 for `once`, 0..=repeat-span otherwise. */
  progress(now: number): number {
    const duration = Math.max(this.animation.duration, 1e-9);
    const elapsed = Math.max(now - this.started, 0);
    const repeat = this.animation.repeat ?? "once";
    if (repeat === "once") return Math.min(elapsed / duration, 1);
    if (repeat === "loop") return (elapsed / duration) % 1;
    // ping-pong: there and back, so the second half runs the curve reversed.
    return 1 - Math.abs(1 - ((elapsed / duration) % 2));
  }

  value(now: number): number {
    return Easing.apply(this.animation.easing ?? "linear", this.progress(now));
  }

  finished(now: number): boolean {
    if ((this.animation.repeat ?? "once") !== "once") return false;
    return now - this.started >= this.animation.duration;
  }

  restart(now: number): void {
    this.started = now;
  }
}

export class Spring {
  constructor(
    readonly stiffness: number,
    readonly damping: number,
    readonly mass: number,
  ) {}
  static bouncy(): Spring {
    return new Spring(120, 10, 1);
  }
  static stiff(): Spring {
    return new Spring(300, 40, 1);
  }
}

/** The physics substep, and the largest step taken at once — as the Rust toolkit paces them. */
const SPRING_STEP = 0.008;
const SPRING_MAX_STEP = 0.25;

/** Where a spring is and where it is going. A retarget keeps the velocity. */
export class SpringState {
  constructor(
    public value = 0,
    public velocity = 0,
    public target = 0,
  ) {}

  retarget(target: number): void {
    this.target = target;
  }

  advance(spring: Spring, dt: number): void {
    let remaining = Math.min(Math.max(dt, 0), SPRING_MAX_STEP);
    while (remaining > 0) {
      const step = Math.min(remaining, SPRING_STEP);
      const force = -spring.stiffness * (this.value - this.target);
      const drag = -spring.damping * this.velocity;
      this.velocity += ((force + drag) / spring.mass) * step;
      this.value += this.velocity * step;
      remaining -= step;
    }
  }

  atRest(): boolean {
    return Math.abs(this.value - this.target) < 1e-3 && Math.abs(this.velocity) < 1e-3;
  }
}

// ---------------------------------------------------------------------------- lists

/** Where a list is scrolled to, and what it is scrolling through.
 *
 *  Ported exactly from the Rust toolkit's own arithmetic (`vivid_ui/src/list.rs`), including
 *  the rule that the visible range holds one row more than the box shows — the one sliding
 *  in — and that there is no overscroll past the last row. */
export class ListState {
  constructor(
    public count: number,
    readonly rowHeight: number,
    public offset = 0,
  ) {}

  contentHeight(): number {
    return this.count * this.rowHeight;
  }

  maximumOffset(viewportHeight: number): number {
    return Math.max(this.contentHeight() - viewportHeight, 0);
  }

  setCount(count: number, viewportHeight: number): void {
    this.count = count;
    this.scrollTo(this.offset, viewportHeight);
  }

  scrollTo(offset: number, viewportHeight: number): void {
    this.offset = Math.min(Math.max(offset, 0), this.maximumOffset(viewportHeight));
  }

  scrollBy(dy: number, viewportHeight: number): void {
    this.scrollTo(this.offset + dy, viewportHeight);
  }

  visibleRange(viewportHeight: number): readonly [number, number] {
    const first = Math.floor(this.offset / this.rowHeight);
    const shown = Math.ceil(viewportHeight / this.rowHeight) + 1;
    return [Math.max(first, 0), Math.min(first + shown, this.count)];
  }

  /** The scrollbar thumb's `[top, length]`, or undefined when the content fits. Length is
   *  never shorter than 16 logical pixels. */
  thumb(viewportHeight: number): readonly [number, number] | undefined {
    const content = this.contentHeight();
    if (content <= viewportHeight) return undefined;
    const length = Math.max((viewportHeight * viewportHeight) / content, 16);
    const travel = viewportHeight - length;
    return [(travel * this.offset) / this.maximumOffset(viewportHeight), length];
  }

  /** Where a row sits against the top of the viewport. */
  rowTop(row: number): number {
    return row * this.rowHeight - this.offset;
  }
}

// ---------------------------------------------------------------------- interactions

/** What an element is right now. Hover, press, and focus are remembered against the
 *  element's identity between frames, exactly as the toolkit remembers them by region. */
export class ElementState {
  hovered = false;
  pressed = false;
  focused = false;
  focusVisible = false;
}

/** Handlers are written against the event they expect, so the parameter is the event type
 *  the example named, not the union. */
export type Handler = (event: any) => void | Promise<void>;

interface Region {
  readonly id: bigint;
  readonly name: string;
  readonly bounds: Rect;
  readonly handlers: Map<string, Handler>;
}

export interface RegionOptions {
  readonly radius?: number;
  readonly cursor?: CursorShape;
  readonly role?: HitRole;
  readonly edges?: number;
  readonly focusable?: boolean;
  onClick?: Handler;
  onMouseDown?: Handler;
  onMouseUp?: Handler;
  onMouseMove?: Handler;
  onWheel?: Handler;
  onKey?: Handler;
  onText?: Handler;
  onIme?: Handler;
  onPressure?: Handler;
  onAccessibility?: Handler;
}

export interface LabelOptions {
  readonly size?: number;
  readonly color?: number;
  readonly align?: "start" | "center" | "end";
  readonly vcenter?: boolean;
  readonly weight?: number;
  readonly italic?: boolean;
  readonly family?: string;
  readonly maxWidth?: number;
}

/** What one paint of a view draws through.
 *
 *  Regions are registered as they are painted; `boundsOf` reads the *previous* frame's
 *  table, which is the frame the host hit-tested against and the only one whose geometry
 *  an event could be referring to. */
export class Frame {
  readonly regions = new Map<bigint, Region>();
  readonly focusOrder: string[] = [];
  semantics: readonly SemanticNode[] = [];
  caret: Rect | undefined;

  constructor(
    readonly ui: Ui<any>,
    readonly canvas: overlay.Canvas,
    readonly elapsed: number,
    readonly dt: number,
    readonly width: number,
    readonly height: number,
  ) {}

  // -- identity --------------------------------------------------------------

  /** Paint a hit region and register everything about it. Returns the element's live state
   *  so the caller can restyle around hover, press, and focus. */
  region(name: string, bounds: Rect, options: RegionOptions = {}): ElementState {
    const id = this.ui.regionId(name);
    const handlers = new Map<string, Handler>();
    const slots: readonly (readonly [string, Handler | undefined])[] = [
      ["click", options.onClick],
      ["mouse_down", options.onMouseDown],
      ["mouse_up", options.onMouseUp],
      ["mouse_move", options.onMouseMove],
      ["wheel", options.onWheel],
      ["key", options.onKey],
      ["text", options.onText],
      ["ime", options.onIme],
      ["pressure", options.onPressure],
      ["accessibility", options.onAccessibility],
    ];
    for (const [slot, handler] of slots) if (handler) handlers.set(slot, handler);
    this.regions.set(id, { id, name, bounds, handlers });
    if (options.focusable) this.focusOrder.push(name);
    const radius = options.radius ?? 0;
    const path =
      radius > 0 ? Path.roundedRectangle(bounds, radius) : Path.rectangle(bounds);
    this.canvas.hit(
      id,
      path,
      options.role ?? "input",
      options.edges ?? 0,
      options.cursor,
    );
    const state = this.ui.stateOf(name);
    state.focused = this.ui.focused() === name;
    state.focusVisible = state.focused && this.ui.focusVisible();
    return state;
  }

  /** Name the region the host should move the window by when dragged. */
  dragRegion(name: string, bounds: Rect, cursor: CursorShape = "grab"): void {
    this.region(name, bounds, { role: "drag", cursor });
  }

  /** Name the region the host should resize the window from, on those edges. */
  resizeRegion(name: string, bounds: Rect, edges: number, cursor: CursorShape): void {
    this.region(name, bounds, { role: "resize", edges, cursor });
  }

  /** Where an element was in the frame before this one. */
  boundsOf(name: string): Rect | undefined {
    return this.ui.boundsOf(name);
  }

  notify(): void {
    this.ui.notify();
  }

  /** Ask to be painted again in `seconds` — how anything that moves is scheduled. */
  requestFrameAfter(seconds: number): void {
    this.ui.requestFrameAfter(seconds);
  }

  // -- drawing ---------------------------------------------------------------

  /** A box: an optional fill, an optional outline that draws inside the edge. */
  box(
    r: Rect,
    options: { bg?: number; radius?: number; border?: number; borderColor?: number } = {},
  ): void {
    const radius = options.radius ?? 0;
    const path = radius > 0 ? Path.roundedRectangle(r, radius) : Path.rectangle(r);
    if (options.bg !== undefined) this.canvas.fill(path, Brush.solid(options.bg));
    if ((options.border ?? 0) > 0) {
      this.canvas.stroke(path, Brush.solid(options.borderColor ?? 0), options.border!);
    }
  }

  /** The 12px grey label an example window opens with. */
  caption(text: string, origin: Point): void {
    this.canvas.text(text, origin, 12, CAPTION);
  }

  /** One run of text, placed in a box: horizontally by `align` (measured, for center and
   *  end), vertically centred on the measured line unless told not to.
   *
   *  Measuring is a round trip to the host, so this is awaited — the same rule the binding
   *  applies to everything it asks the host for. */
  async label(text: string, r: Rect, options: LabelOptions = {}): Promise<void> {
    const size = options.size ?? 13;
    const measured = await this.measure(text, size, options);
    let x = r.x;
    if (options.align === "center") x += (r.width - measured.width) / 2;
    else if (options.align === "end") x += r.width - measured.width;
    const y = options.vcenter === false ? r.y : r.y + (r.height - measured.height) / 2;
    this.canvas.text(text, { x, y }, size, options.color ?? TEXT, {
      family: options.family,
      weight: options.weight,
      italic: options.italic,
      maxWidth: options.maxWidth,
    });
  }

  /** A button: hover and press restyle it, the host shows the pointer cursor, and a focus
   *  ring appears only when the keyboard put the focus there. */
  async button(
    name: string,
    r: Rect,
    text: string,
    options: RegionOptions & {
      base?: number;
      hover?: number;
      active?: number;
      textColor?: number;
      size?: number;
    } = {},
  ): Promise<ElementState> {
    const state = this.region(name, r, {
      radius: options.radius ?? 6,
      cursor: "pointer",
      focusable: options.focusable,
      onClick: options.onClick,
    });
    const fill = state.pressed
      ? (options.active ?? BUTTON_ACTIVE)
      : state.hovered
        ? (options.hover ?? BUTTON_HOVER)
        : (options.base ?? BUTTON);
    this.box(r, { bg: fill, radius: options.radius ?? 6 });
    if (state.focused && state.focusVisible) {
      this.focusRing(r, options.radius ?? 6);
    }
    await this.label(text, r, {
      size: options.size ?? 13,
      color: options.textColor ?? WHITE,
      align: "center",
    });
    return state;
  }

  focusRing(
    r: Rect,
    radius = 0,
    color: number = FOCUS_RING,
    width = 2,
  ): void {
    const ring = inset(r, width / 2);
    const path =
      radius > 0
        ? Path.roundedRectangle(ring, Math.max(radius - width / 2, 0))
        : Path.rectangle(ring);
    this.canvas.stroke(path, Brush.solid(color), width);
  }

  /** Cast one of the token shadows behind a box. */
  shadowBox(r: Rect, token: keyof typeof SHADOW_TOKENS = "md", radius = 0): void {
    const [ox, oy, blur, spread] = SHADOW_TOKENS[token]!;
    this.canvas.shadow({
      rect: r,
      radii: [radius, radius, radius, radius],
      color: 0x0000000a,
      offset: { x: ox, y: oy },
      blur,
      spread,
    });
  }

  /** An 8px scrollbar for a list: nothing at all when the content fits. */
  scrollbar(r: Rect, list: ListState, viewportHeight: number): void {
    const thumb = list.thumb(viewportHeight);
    if (!thumb) return;
    const [top, length] = thumb;
    this.box(r, { bg: 0x16161eff, radius: 4 });
    this.box(rect(r.x, r.y + top, r.width, length), { bg: BUTTON, radius: 4 });
  }

  clip(r: Rect, radius = 0): void {
    const path = radius > 0 ? Path.roundedRectangle(r, radius) : Path.rectangle(r);
    this.canvas.clip(path);
  }

  // -- text and media --------------------------------------------------------

  /** What the host says about a run. Cached: a run that has not changed is not measured
   *  again, which is the toolkit's rule and the round trip's whole cost. */
  async measure(
    text: string,
    size: number,
    options: LabelOptions = {},
  ): Promise<TextMeasurement> {
    return this.ui.measure(text, size, options);
  }

  /** Where the baseline of a run sits below its top, for baseline alignment. */
  async baselineOf(text: string, size: number): Promise<number> {
    const measured = await this.measure(text, size);
    return measured.clusters[0]?.baseline ?? measured.height * 0.8;
  }

  /** Draw a shaped paragraph: several runs, decorations, wrapping, and the typography the
   *  plain text command cannot carry.
   *
   *  Shaping is the host's, through a retained layout cached under the caller's own key —
   *  the layout is host memory, so one per paragraph is the budget. A host that cannot
   *  shape at all gets the runs drawn plainly instead, which loses the styling that needed
   *  shaping and keeps the words where they were. That is the toolkit's own rule for a
   *  frame past the retention ceiling, and it is what makes these examples draw against a
   *  session with no text service. */
  async paragraph(key: string, styled: StyledText, origin: Point): Promise<void> {
    if (!this.ui.layouts.has(key)) {
      try {
        this.ui.layouts.set(key, await this.ui.window.layoutText(styled));
      } catch {
        // Cache the refusal too: one attempt per paragraph, not one per frame.
        this.ui.layouts.set(key, undefined);
      }
    }
    const layout = this.ui.layouts.get(key);
    if (layout) {
      // Awaited before the canvas is submitted: the binding's ordering rule for anything
      // that appends to a canvas through the host.
      await this.ui.window.drawTextLayout(this.canvas, layout, origin);
      return;
    }
    let x = origin.x;
    for (const run of styled.runs) {
      const style = run.style ?? {};
      const size = style.size ?? 16;
      this.canvas.text(run.text, { x, y: origin.y }, size, style.color ?? TEXT, {
        family: style.family,
        weight: style.weight,
        italic: style.italic,
      });
      x += (await this.measure(run.text, size, {
        family: style.family,
        weight: style.weight,
        italic: style.italic,
      })).width;
    }
  }

  /** Upload pixels once under a name; drawing is what keeps them. */
  async upload(name: string, width: number, height: number, rgba: Uint8Array): Promise<RetainedImage> {
    const cached = this.ui.uploads.get(name);
    if (cached) return cached;
    const image = await this.ui.window.uploadRgba(width, height, rgba);
    this.ui.uploads.set(name, image);
    return image;
  }

  async image(image: RetainedImage, bounds: Rect, opacity = 1): Promise<void> {
    await this.ui.window.drawImage(this.canvas, image, bounds, opacity);
  }

  /** What this frame says about itself, published once it is on screen. */
  publishSemantics(nodes: readonly SemanticNode[]): void {
    this.semantics = nodes;
  }

  /** Where the focused field's caret is, so the host can place its candidate window. */
  publishCaret(caret: Rect): void {
    this.caret = caret;
  }
}

/** A window, the host behind it, and the loop that connects them.
 *
 *  The view is a function of state: `render(state, frame)` draws a whole frame through
 *  `Frame` and registers what is interactive. Everything else here is the contract —
 *  press, click, hover, focus, wheel, keys — reproduced from the Rust toolkit's routing. */
export class Ui<S> {
  width: number;
  height: number;
  // Root handlers: whatever no region wanted comes here. They are plain attributes so an
  // example can wire one to the `Ui` it is still building — a key handler usually needs
  // to call `focus` on the very object that will run it.
  onKey: Handler | undefined;
  onText: Handler | undefined;
  onIme: Handler | undefined;
  onWheel: Handler | undefined;
  onMouseMove: Handler | undefined;
  // A press or a release that no region wanted still happened somewhere — a drag ends
  // wherever the pointer was let go, which is usually not the element it began on.
  onMouseDown: Handler | undefined;
  onMouseUp: Handler | undefined;
  onDismissed: Handler | undefined;

  private session: OverlaySession | undefined;
  private _window: OverlayWindow | undefined;
  private readonly ids = new Map<string, bigint>();
  private nextId = 1n;
  private readonly states = new Map<string, ElementState>();
  private prev = new Map<bigint, Region>();
  private focusList: string[] = [];
  private pressed: readonly [bigint, number] | undefined;
  private hovered: bigint | undefined;
  private focusedName: string | undefined;
  private focusFlag = false;
  private dirty = true;
  private wake: number | undefined;
  private t0 = now();
  private lastPaint = this.t0;
  private quitFlag = false;
  // Caches the frame's services sit in front of. They belong to the window, not to any
  // one frame, so the frame reaches them through its ui.
  readonly measures = new Map<string, TextMeasurement>();
  // An undefined means the host refused to shape this one; it is drawn plainly.
  readonly layouts = new Map<string, RetainedTextLayout | undefined>();
  readonly uploads = new Map<string, RetainedImage>();

  constructor(
    private readonly state: S,
    private readonly render: (state: S, frame: Frame) => void | Promise<void>,
    options: { width: number; height: number; title?: string },
  ) {
    this.width = options.width;
    this.height = options.height;
  }

  /** The window, when there is one — measurement and uploads need it. */
  get window(): OverlayWindow {
    if (!this._window) throw new Error("the ui has no window yet");
    return this._window;
  }

  // -- identity and state ----------------------------------------------------

  /** The stable region number a name paints as, so the host's memory of a region survives
   *  the frame that is rebuilding it. */
  regionId(name: string): bigint {
    let id = this.ids.get(name);
    if (id === undefined) {
      id = BigInt(this.nextId++);
      this.ids.set(name, id);
    }
    return id;
  }

  stateOf(name: string): ElementState {
    let state = this.states.get(name);
    if (!state) {
      state = new ElementState();
      this.states.set(name, state);
    }
    return state;
  }

  /** Where an element was in the last painted frame. */
  boundsOf(name: string): Rect | undefined {
    for (const region of this.prev.values()) {
      if (region.name === name) return region.bounds;
    }
    return undefined;
  }

  focused(): string | undefined {
    return this.focusedName;
  }

  focusVisible(): boolean {
    return this.focusFlag;
  }

  // -- the loop --------------------------------------------------------------

  /** Open the window against the environment's host and pump until dismissed. */
  async run(seconds?: number): Promise<void> {
    await using session = await OverlaySession.fromEnv();
    await using window = await session.createWindow({
      bounds: { x: 60, y: 60, width: this.width, height: this.height },
    });
    await this.attach(session, window);
    try {
      await window.center();
    } catch {
      /* best effort, as the Rust toolkit's `let _ =` is */
    }
    try {
      await window.requestFocus();
    } catch {
      /* same */
    }
    await this.loop(seconds ?? duration(process.argv.slice(2)));
  }

  /** Connect to a session the caller owns and paint the first frame. The caller then
   *  drives `dispatch` and `paint` — which is how a test runs the loop. */
  async attach(session: OverlaySession, window: OverlayWindow): Promise<void> {
    this.session = session;
    this._window = window;
    this.t0 = now();
    this.lastPaint = this.t0;
    await this.paint();
  }

  quit(): void {
    this.quitFlag = true;
  }

  notify(): void {
    this.dirty = true;
  }

  /** Ask for the frame that is `seconds` away. The loop sleeps to the soonest one. */
  requestFrameAfter(seconds: number): void {
    const deadline = now() + seconds;
    this.wake = this.wake === undefined ? deadline : Math.min(this.wake, deadline);
  }

  /** Keep pointer events arriving after the pointer leaves the region a drag began on,
   *  which is what a drag is. */
  capture(on = true): void {
    // Best-effort, as the Rust toolkit's `let _ = cx.capture_pointer(..)` is: a host that
    // will not capture still delivers what happens over its own regions.
    if (this.session && this._window) {
      void this.session.capturePointer(this._window, on).catch(() => undefined);
    }
  }

  /** Move focus to an element, saying whether the keyboard put it there. */
  focus(name: string, options: { visible?: boolean } = {}): void {
    const visible = options.visible ?? true;
    this.focusedName = name;
    this.focusFlag = visible;
    // Focus is a property of the element the moment it moves, not one paint later —
    // hover and press work the same way.
    for (const state of this.states.values()) {
      state.focused = false;
      state.focusVisible = false;
    }
    const state = this.stateOf(name);
    state.focused = true;
    state.focusVisible = visible;
    if (this._window) void this._window.requestFocus().catch(() => undefined);
    this.notify();
  }

  /** Tab order is paint order; focus moves through the focusable elements. */
  focusNext(options: { backwards?: boolean; visible?: boolean } = {}): void {
    const order = this.focusList.filter((name) => this.ids.has(name));
    if (order.length === 0) return;
    const current = this.focusedName !== undefined && order.includes(this.focusedName)
      ? this.focusedName
      : undefined;
    const step = options.backwards ? -1 : 1;
    const index = current === undefined ? (step > 0 ? 0 : order.length - 1) : order.indexOf(current);
    this.focus(order[(index + step + order.length) % order.length]!, {
      visible: options.visible ?? true,
    });
  }

  /** Write to the clipboard. Only the host can, and only after a recent gesture. */
  copy(text: string): void {
    if (this._window) void this._window.setClipboard(text).catch(() => undefined);
  }

  /** Give a picture back. The host's memory is bounded, and an example that caches its
   *  own uploads is the one that decides what is no longer on screen. */
  releaseImage(image: RetainedImage): void {
    if (this._window) void this._window.releaseImage(image).catch(() => undefined);
    for (const [name, held] of this.uploads) {
      if (held === image) this.uploads.delete(name);
    }
  }

  /** What the host says about a run, cached — a run that has not changed is not measured
   *  again, which is the toolkit's rule and the round trip's whole cost.
   *
   *  This is on the window rather than the frame because a field measures when a key
   *  arrives, which is not inside a paint. */
  async measure(text: string, size: number, options: LabelOptions = {}): Promise<TextMeasurement> {
    const key = `${text}\u{0}|${size}|${options.family ?? ""}|${options.weight ?? 400}|${options.italic ?? false}|${options.maxWidth ?? ""}`;
    const cached = this.measures.get(key);
    if (cached) return cached;
    let measured: TextMeasurement;
    try {
      measured = await this.window.measureText(text, size, {
        family: options.family,
        weight: options.weight,
        italic: options.italic,
        maxWidth: options.maxWidth,
      });
    } catch {
      // A host with no text service — an offline session, a test — still draws. The
      // estimate keeps boxes laid out and clicks routable, the way the Rust toolkit draws
      // a run nobody measured at its own line height rather than collapsed. A live pane
      // measures for real; nothing else changes.
      measured = { width: size * 0.6 * text.length, height: size * 1.2, lines: [], clusters: [] };
    }
    this.measures.set(key, measured);
    return measured;
  }

  /** Look at the state without handing it out — the test seam. */
  read<R>(f: (state: S) => R): R {
    return f(this.state);
  }

  /** Whether the loop is on its way out — escape, `q`, a dismissal, or a lost host. */
  quitting(): boolean {
    return this.quitFlag;
  }

  elapsed(): number {
    return now() - this.t0;
  }

  /** Build one frame and send it, publishing semantics and the caret once the frame they
   *  describe is the one on screen. */
  async paint(): Promise<void> {
    const window = this.window;
    const canvas = new Canvas();
    const at = now();
    const frame = new Frame(this, canvas, at - this.t0, at - this.lastPaint, this.width, this.height);
    // A wake is asked for by the frame that wants it; painting re-arms nothing.
    this.wake = undefined;
    await this.render(this.state, frame);
    canvas.validate();
    if (frame.semantics.length > 0 || frame.caret !== undefined) {
      const receipt = await window.submit(canvas);
      if ((await receipt.wait(1)) === "presented") {
        // Best-effort, like every host call the frame can survive without: an offline
        // session refuses semantics outright, and a host refusing a description of a
        // scene it is not showing is the rule that stops a screen reader announcing a
        // control that has gone — not an error worth a frame.
        try {
          if (frame.semantics.length > 0) {
            await window.setSemantics({ sceneRevision: receipt.revision, nodes: frame.semantics });
          }
          if (frame.caret !== undefined) {
            await window.setEditorGeometry(receipt.revision, frame.caret);
          }
        } catch {
          /* refused: nothing here needs it */
        }
      }
    } else {
      await window.present(canvas);
    }
    this.prev = frame.regions;
    this.focusList = [...frame.focusOrder];
    this.lastPaint = at;
    this.dirty = false;
  }

  private async loop(seconds: number | undefined): Promise<void> {
    if (!this.session) throw new Error("the ui has no session yet");
    const session = this.session;
    const deadline = seconds === undefined ? undefined : now() + seconds;
    while (!this.quitFlag) {
      const at = now();
      if (deadline !== undefined && at >= deadline) break;
      let timeout = IDLE_TICK;
      if (this.wake !== undefined) timeout = Math.max(0, Math.min(timeout, this.wake - at));
      if (deadline !== undefined) timeout = Math.max(0, Math.min(timeout, deadline - at));
      const event = await session.waitEvent(timeout);
      if (event !== undefined) {
        await this.dispatch(event);
        if (event.kind === "connection-lost") break;
      }
      const after = now();
      if (this.dirty || (this.wake !== undefined && after >= this.wake)) {
        await this.paint();
      }
    }
  }

  // -- dispatch ----------------------------------------------------------------
  //
  // Transcribed from the Rust toolkit's routing (vivid_ui/src/window.rs): the host does
  // the hit testing, a click is a release on the element the press started on, leaving a
  // pressed element cancels the press, the wheel goes to whatever is hovered, and keys go
  // to the focused element before the window's own handlers.

  async dispatch(event: OverlayEvent): Promise<void> {
    if (this._window && !event.targets(this._window)) return;
    switch (event.kind) {
      case "connection-lost":
        this.quitFlag = true;
        break;
      case "pointer":
        await this.pointer(event);
        break;
      case "hover":
        this.hover(event);
        break;
      case "wheel":
        await this.fire(this.hovered, "wheel", event);
        break;
      case "key":
        if (this.isQuitKey(event)) {
          this.quitFlag = true;
          break;
        }
        await this.focusRouted(event);
        break;
      case "text":
      case "ime":
        await this.focusRouted(event);
        break;
      case "focus":
        if (!event.focused) {
          this.focusFlag = false;
          this.cancelPress();
          this.notify();
        }
        break;
      case "geometry":
        this.width = event.bounds.width;
        this.height = event.bounds.height;
        this.notify();
        break;
      case "dismissed":
        if (this.onDismissed) await this.onDismissed(event);
        else this.quitFlag = true;
        break;
      case "cancel":
        this.notify();
        break;
      case "accessibility":
        await this.fire(event.applicationId, "accessibility", event);
        break;
      default:
        // environment, viewport, submission-outcome: nothing here listens.
        break;
    }
  }

  private async pointer(event: PointerEventT): Promise<void> {
    const region = event.applicationId;
    if (event.button === undefined) {
      // A move. Pressure is separate and optional: a host with no sensor never says
      // zero, it says nothing.
      await this.fire(region, "mouse_move", event);
      if (event.pressure !== undefined) await this.fire(region, "pressure", event);
      return;
    }
    if (event.down) {
      // Best-effort, as the Rust toolkit's `let _ =` is: a host that cannot take the
      // request still gets the press.
      await this._window?.requestFocus().catch(() => undefined);
      this.pressed = [region, event.clicks];
      this.setPressed(region, true);
      await this.fire(region, "mouse_down", event);
    } else {
      await this.fire(region, "mouse_up", event);
      const pressed = this.pressed;
      this.pressed = undefined;
      if (pressed) {
        const [start, clicks] = pressed;
        this.setPressed(start, false);
        if (start === region && clicks > 0) await this.fire(region, "click", event);
      }
    }
  }

  private hover(event: HoverEventT): void {
    const region = event.applicationId;
    const name = this.nameOf(region);
    const state = name ? this.states.get(name) : undefined;
    if (state && state.hovered !== event.entered) {
      state.hovered = event.entered;
      this.notify();
    }
    if (event.entered) this.hovered = region;
    else if (this.hovered === region) this.hovered = undefined;
    // Leaving a pressed element cancels the press: a release over nothing is not
    // reported at all, so the press must not wait for one.
    if (!event.entered && this.pressed && this.pressed[0] === region) {
      this.cancelPress();
    }
  }

  private async focusRouted(event: OverlayEvent): Promise<void> {
    const slot = event.kind === "key" ? "key" : event.kind === "text" ? "text" : "ime";
    const focusedId = this.focusedName !== undefined ? this.regionId(this.focusedName) : undefined;
    const region = focusedId !== undefined ? this.prev.get(focusedId) : undefined;
    const handler = region?.handlers.get(slot) ?? this.rootHandler(slot);
    if (handler) await handler(event);
  }

  private rootHandler(slot: string): Handler | undefined {
    return ({
      key: this.onKey,
      text: this.onText,
      ime: this.onIme,
      wheel: this.onWheel,
      mouse_move: this.onMouseMove,
      mouse_down: this.onMouseDown,
      mouse_up: this.onMouseUp,
    } as Record<string, Handler | undefined>)[slot];
  }

  /** Whether this key ends the example.
   *
   *  These windows are floating, and the protocol only dismisses a *popup* on escape or an
   *  outside press — so a floating example has no way out of its own unless it gives itself
   *  one. Escape always ends it. `q` does too, except where something in the window accepts
   *  typed text: a window with a field in it cannot spend a letter on quitting, so there
   *  escape is the only way. */
  private isQuitKey(event: Extract<OverlayEvent, { kind: "key" }>): boolean {
    if (!event.down || event.repeat) return false;
    if (event.physical === Keys.escape) return true;
    if (event.physical !== Keys.letter("q")) return false;
    // A chord is somebody else's shortcut, not this.
    if (event.modifiers & (Mods.control | Mods.alt | Mods.super)) return false;
    return this.onText === undefined;
  }

  private cancelPress(): void {
    if (this.pressed) {
      this.setPressed(this.pressed[0], false);
      this.pressed = undefined;
      this.notify();
    }
  }

  private setPressed(regionId: bigint, pressed: boolean): void {
    const name = this.nameOf(regionId);
    const state = name ? this.states.get(name) : undefined;
    if (state && state.pressed !== pressed) {
      state.pressed = pressed;
      this.notify();
    }
  }

  private nameOf(regionId: bigint): string | undefined {
    return this.prev.get(regionId)?.name;
  }

  /** Deliver to the region the host named, falling to the window's own handler. */
  private async fire(
    regionId: bigint | undefined,
    slot: string,
    event: OverlayEvent,
  ): Promise<void> {
    const region = regionId !== undefined ? this.prev.get(regionId) : undefined;
    const handler = region?.handlers.get(slot) ?? this.rootHandler(slot);
    if (handler) await handler(event);
  }
}

/** A single-line text field: text, a caret, a selection anchor, and the composition an
 *  input method is still deciding about.
 *
 *  Offsets are UTF-16 code units end to end — native to JavaScript strings, and the same
 *  units the binding's cluster geometry and IME selection arrive in. Python's twin uses
 *  character indexes for the same reason: each language is self-consistent with its own
 *  binding, and nothing mixes the two. */
/** Anything that can ask the host how big a run is: a `Frame`, or the `Ui` itself when a
 *  key arrives between paints. */
export interface Measurer {
  measure(text: string, size: number, options?: LabelOptions): Promise<TextMeasurement>;
}

export class TextField {
  caret = 0;
  anchor = 0;
  preedit: string | undefined;

  constructor(
    public text = "",
    readonly size = 14,
  ) {
    this.caret = Math.min(this.caret, text.length);
    this.anchor = Math.min(this.anchor, text.length);
  }

  get selected(): boolean {
    return this.caret !== this.anchor;
  }

  selectedText(): string {
    return this.text.slice(Math.min(this.caret, this.anchor), Math.max(this.caret, this.anchor));
  }

  private deleteSelection(): void {
    const lo = Math.min(this.caret, this.anchor);
    const hi = Math.max(this.caret, this.anchor);
    this.text = this.text.slice(0, lo) + this.text.slice(hi);
    this.caret = this.anchor = lo;
  }

  /** Replace the selection with `added`, leaving the caret after it. */
  insert(added: string): void {
    if (this.selected) this.deleteSelection();
    this.text = this.text.slice(0, this.caret) + added + this.text.slice(this.caret);
    this.caret = this.anchor = this.caret + added.length;
  }

  /** Every caret position the text offers: cluster edges, or every index when the host
   *  could not be asked (an offline session, a test). */
  async boundaries(f: Measurer): Promise<number[]> {
    const measured = await f.measure(this.text, this.size);
    const edges = new Set<number>([0, this.text.length]);
    for (const cluster of measured.clusters) {
      edges.add(cluster.start);
      edges.add(cluster.end);
    }
    if (measured.clusters.length === 0) {
      for (let i = 0; i <= this.text.length; i += 1) edges.add(i);
    }
    return [...edges].sort((a, b) => a - b);
  }

  /** Arrows by cluster, home and end, backspace and delete. Shift moves the caret and
   *  keeps the anchor, which is what a selection is. Returns whether anything changed. */
  async handleKey(event: Extract<OverlayEvent, { kind: "key" }>, f: Measurer): Promise<boolean> {
    if (!event.down || event.repeat) return false;
    const edges = await this.boundaries(f);
    if (event.physical === Keys.left) {
      const target = [...edges].reverse().find((e) => e < this.caret) ?? this.caret;
      if (event.modifiers & Mods.shift) this.caret = target;
      else this.caret = this.anchor = target;
      return true;
    }
    if (event.physical === Keys.right) {
      const target = edges.find((e) => e > this.caret) ?? this.caret;
      if (event.modifiers & Mods.shift) this.caret = target;
      else this.caret = this.anchor = target;
      return true;
    }
    if (event.physical === Keys.home) {
      this.caret = 0;
      if (!(event.modifiers & Mods.shift)) this.anchor = 0;
      return true;
    }
    if (event.physical === Keys.end) {
      this.caret = this.text.length;
      if (!(event.modifiers & Mods.shift)) this.anchor = this.text.length;
      return true;
    }
    if (event.physical === Keys.backspace) {
      if (this.selected) this.deleteSelection();
      else if (this.caret > 0) {
        const edge = [...edges].reverse().find((e) => e < this.caret)!;
        this.text = this.text.slice(0, edge) + this.text.slice(this.caret);
        this.caret = this.anchor = edge;
      }
      return true;
    }
    if (event.physical === Keys.delete) {
      if (this.selected) this.deleteSelection();
      else if (this.caret < this.text.length) {
        const edge = edges.find((e) => e > this.caret)!;
        this.text = this.text.slice(0, this.caret) + this.text.slice(edge);
        this.anchor = this.caret;
      }
      return true;
    }
    return false;
  }

  /** Committed text — typed characters and paste alike, a terminal host being unable to
   *  read a clipboard, only write one. */
  handleText(event: Extract<OverlayEvent, { kind: "text" }>): boolean {
    if (event.text) {
      this.insert(event.text);
      return true;
    }
    return false;
  }

  /** The composition an input method is still deciding about. It is drawn inline and the
   *  caret sits after it; committing arrives as ordinary text. */
  handleIme(event: Extract<OverlayEvent, { kind: "ime" }>): boolean {
    const composing = event.preedit || undefined;
    const changed = composing !== this.preedit;
    this.preedit = composing;
    return changed;
  }

  /** What the field renders: the text with the composition where the caret is. */
  drawn(): string {
    if (!this.preedit) return this.text;
    return this.text.slice(0, this.caret) + this.preedit + this.text.slice(this.caret);
  }

  async caretX(f: Measurer): Promise<number> {
    return (await f.measure(this.text.slice(0, this.caret), this.size)).width;
  }

  /** Where the caret is drawn, and what the host places its candidate window from. A
   *  composition in flight is drawn before it, so the caret sits after the preedit. */
  async caretRect(f: Measurer, box: Rect): Promise<Rect> {
    const measured = await f.measure(this.drawn(), this.size);
    const preeditWidth = this.preedit ? (await f.measure(this.preedit, this.size)).width : 0;
    const x = box.x + (await this.caretX(f)) + preeditWidth;
    return rect(x, box.y + (box.height - measured.height) / 2, 1.5, measured.height);
  }

  async selectionRect(f: Measurer, box: Rect): Promise<Rect | undefined> {
    if (!this.selected) return undefined;
    const measured = await f.measure(this.text, this.size);
    const lo = Math.min(this.caret, this.anchor);
    const hi = Math.max(this.caret, this.anchor);
    const loX = (await f.measure(this.text.slice(0, lo), this.size)).width;
    const hiX = (await f.measure(this.text.slice(0, hi), this.size)).width;
    return rect(
      box.x + loX,
      box.y + (box.height - measured.height) / 2,
      Math.max(hiX - loX, 1),
      measured.height,
    );
  }
}

function now(): number {
  return Date.now() / 1000;
}

/** Run an example's ui when its file is the one node was pointed at. Importing the module
 *  from a test does nothing — the same protection `if __name__ == "__main__"` gives the
 *  Python twins. */
export async function runMain(make: () => Ui<any>): Promise<void> {
  const entry = process.argv[1] !== undefined ? pathToFileURL(process.argv[1]).href : "";
  if (entry === import.meta.url) await make().run();
}

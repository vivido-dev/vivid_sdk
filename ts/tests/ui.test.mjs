// The shared helper behind the `vivid_ui` example ports, and the contracts it reproduces.
//
// This imports the *compiled* helper, so `npm run build:examples` has to have run — the same
// build-first rule the other suites follow for `dist/`. What is checked here is what can be
// checked without a pane: the arithmetic, and the interaction contract driven with synthetic
// events against an offline session.
import { existsSync } from "node:fs";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import test from "node:test";
import assert from "node:assert/strict";

import { OverlaySession } from "../../dist/index.js";

const BUILT = "target/examples-typescript/ui";
if (!existsSync(resolve(BUILT, "ui.js"))) {
  throw new Error("run `npm run build:examples` first: these tests drive the compiled helper");
}

const load = (name) => import(pathToFileURL(resolve(BUILT, `${name}.js`)).href);
const { Easing, ListState, Running, Spring, SpringState, TextField, fitRect, gradientBrush, inset, Keys, Mods } =
  await load("ui");

// Synthetic events: a raw handle whose `targets` is always the window under test.
const raw = { targets: () => true, sceneRevision: 0n };
const pointer = (applicationId, x = 0, y = 0, button, down, clicks = 0, pressure) =>
  ({ kind: "pointer", position: { x, y }, applicationId, modifiers: 0, button, down, clicks, pressure, ...raw });
const hover = (applicationId, entered) => ({ kind: "hover", applicationId, entered, ...raw });
const wheel = (dy) =>
  ({ kind: "wheel", position: { x: 0, y: 0 }, dx: 0, dy, modifiers: 0, precise: true, phase: "changed", ...raw });
const key = (physical, modifiers = 0, down = true, repeat = false) =>
  ({ kind: "key", physical, down, repeat, modifiers, ...raw });
const text = (value) => ({ kind: "text", text: value, ...raw });
const accessibility = (applicationId, action) => ({ kind: "accessibility", applicationId, action, ...raw });

/** One example, attached to an offline session and painted once. */
async function started(name) {
  const module = await load(name);
  const session = await OverlaySession.connect({ offline: true });
  const ui = module.makeUi();
  const window = await session.createWindow({
    bounds: { x: 60, y: 60, width: ui.width, height: ui.height },
  });
  await ui.attach(session, window);
  return { ui, module, session };
}

// --------------------------------------------------------------------------- arithmetic

test("a list shows the rows its box can hold and one more", () => {
  const state = new ListState(1000, 20);
  // A 100 pixel box holds five whole rows; the sixth is the one sliding in.
  assert.deepEqual(state.visibleRange(100), [0, 6]);
  state.scrollTo(200, 100);
  assert.deepEqual(state.visibleRange(100), [10, 16]);
  // Part way through a row: the row half off the top is still the first one.
  state.scrollTo(205, 100);
  assert.deepEqual(state.visibleRange(100), [10, 16]);
});

test("a list does not scroll past its own end", () => {
  const state = new ListState(10, 20);
  assert.equal(state.maximumOffset(100), 100);
  state.scrollBy(1000, 100);
  assert.equal(state.offset, 100);
  state.scrollBy(-1000, 100);
  assert.equal(state.offset, 0);

  // Content shorter than the box does not scroll at all.
  const short = new ListState(2, 20);
  short.scrollBy(50, 100);
  assert.equal(short.offset, 0);
  assert.deepEqual(short.visibleRange(100), [0, 2]);
});

test("a scrollbar thumb is the fraction on screen and never a sliver", () => {
  const state = new ListState(100, 20);
  const [top, length] = state.thumb(100);
  assert.equal(top, 0);
  // A twentieth of the content is on screen, but a thumb is never shorter than 16.
  assert.equal(length, 16);

  state.scrollTo(state.maximumOffset(100), 100);
  const [bottom, sameLength] = state.thumb(100);
  assert.ok(Math.abs(bottom - (100 - sameLength)) < 1e-9);

  // Content that fits has no thumb at all, rather than a full-length one.
  assert.equal(new ListState(2, 20).thumb(100), undefined);
});

/** Curves are arithmetic, so an end lands within a rounding error of it rather than on it. */
const close = (value, expected) =>
  assert.ok(Math.abs(value - expected) < 1e-12, `${value} is not ${expected}`);

test("every easing starts at nothing and ends at everything", () => {
  for (const kind of ["linear", "ease-in", "ease-out", "ease-in-out", "back-out"]) {
    close(Easing.apply(kind, 0), 0);
    close(Easing.apply(kind, 1), 1);
    // Past the ends is clamped, not extrapolated.
    close(Easing.apply(kind, -1), 0);
    close(Easing.apply(kind, 2), 1);
  }
  close(Easing.apply("linear", 0.5), 0.5);
  close(Easing.apply("ease-in-out", 0.5), 0.5);
  close(Easing.apply("ease-out", 0.5), 0.875);
  close(Easing.apply("ease-in", 0.5), 0.125);
  // Back-out overshoots before it settles, which is the whole point of it.
  assert.ok(Easing.apply("back-out", 0.7) > 1);
});

test("a ping-pong animation goes there and comes back", () => {
  const running = new Running({ duration: 2, repeat: "ping-pong" }, 0);
  close(running.progress(0), 0);
  close(running.progress(2), 1);
  close(running.progress(4), 0);
  // It never finishes, which is what keeps it asking for frames.
  assert.equal(running.finished(100), false);

  const once = new Running({ duration: 1 }, 0);
  assert.equal(once.progress(2), 1);
  assert.equal(once.finished(1), true);
});

test("a spring settles and keeps its speed when it is sent somewhere else", () => {
  const spring = Spring.bouncy();
  const state = new SpringState();
  state.retarget(1);
  for (let step = 0; step < 250; step += 1) state.advance(spring, 1 / 60);
  assert.ok(state.atRest());
  assert.ok(Math.abs(state.value - 1) < 0.01);

  // Retargeted in flight, it carries its velocity into the new target rather than
  // restarting — which is the whole reason this is a spring and not a duration.
  const moving = new SpringState();
  moving.retarget(1);
  for (let step = 0; step < 6; step += 1) moving.advance(spring, 1 / 60);
  const speed = moving.velocity;
  assert.ok(speed > 0);
  moving.retarget(0.5);
  assert.equal(moving.velocity, speed);
});

test("a gradient spans its box along its own direction", () => {
  const box = { x: 0, y: 0, width: 100, height: 80 };
  const stops = [{ offset: 0, color: 0x000000ff }, { offset: 1, color: 0xffffffff }];
  // `|| 0` folds a negative zero back to zero: the same point, and deepStrictEqual
  // distinguishes them.
  const geometry = (angle) =>
    gradientBrush(box, angle, stops).geometry.map((value) => Math.round(value) || 0);

  // 0 is upward, so the gradient runs from the bottom edge to the top one.
  assert.deepEqual(geometry(0), [50, 80, 50, 0]);
  // 90 is rightward.
  assert.deepEqual(geometry(90), [0, 40, 100, 40]);
  // 180 is downward: the reverse of 0.
  assert.deepEqual(geometry(180), [50, 0, 50, 80]);
});

test("a fitted picture keeps its proportions and its centre", () => {
  // Wider than its box: the width binds and the result is centred vertically.
  assert.deepEqual(fitRect(96, 48, { x: 0, y: 0, width: 72, height: 72 }), {
    x: 0, y: 18, width: 72, height: 36,
  });
  // Taller than its box: the height binds.
  assert.deepEqual(fitRect(48, 96, { x: 0, y: 0, width: 72, height: 72 }), {
    x: 18, y: 0, width: 36, height: 72,
  });
  // A picture with no pixels fits nothing rather than dividing by zero.
  assert.equal(fitRect(0, 10, { x: 0, y: 0, width: 72, height: 72 }).width, 0);
});

test("an inset never shrinks past nothing", () => {
  assert.deepEqual(inset({ x: 0, y: 0, width: 100, height: 100 }, 10), {
    x: 10, y: 10, width: 80, height: 80,
  });
  assert.equal(inset({ x: 0, y: 0, width: 10, height: 10 }, 40).width, 0);
});

// ------------------------------------------------------------------- the text field

test("a field types, selects and deletes by cluster", async () => {
  // With no window behind it the estimate stands in for measurement, which is exactly what
  // a field on a text-less host gets.
  const { Ui } = await load("ui");
  const ui = new Ui({}, () => {}, { width: 100, height: 100 });
  const field = new TextField("hello");
  field.caret = field.anchor = 5;

  field.insert(" world");
  assert.equal(field.text, "hello world");
  assert.equal(field.caret, 11);

  // Shift-left extends the selection; the anchor stays where it was.
  await field.handleKey(key(Keys.left, Mods.shift), ui);
  await field.handleKey(key(Keys.left, Mods.shift), ui);
  assert.equal(field.selectedText(), "ld");
  assert.equal(field.anchor, 11);

  // Backspace takes the selection rather than one character.
  await field.handleKey(key(Keys.backspace), ui);
  assert.equal(field.text, "hello wor");
  assert.equal(field.selected, false);
});

test("a composition draws inline and commits as ordinary text", () => {
  const field = new TextField("ab");
  field.caret = field.anchor = 1;
  field.handleIme({ kind: "ime", preedit: "み", ...raw });
  // The composition is where the caret is, and the text underneath has not changed yet.
  assert.equal(field.drawn(), "aみb");
  assert.equal(field.text, "ab");

  field.handleIme({ kind: "ime", preedit: "", ...raw });
  field.handleText(text("見"));
  assert.equal(field.text, "a見b");
});

// --------------------------------------------------------- the interaction contract

test("a click is a release over the element the press began on", async () => {
  const { ui } = await started("hello_world");
  const region = ui.regionId("increment");
  await ui.dispatch(pointer(region, 0, 0, 0, true, 1));
  await ui.dispatch(pointer(region, 0, 0, 0, false));
  assert.equal(ui.read((state) => state.count), 1);

  // A release over nothing is not a click, however it started.
  await ui.dispatch(pointer(region, 0, 0, 0, true, 1));
  await ui.dispatch(pointer(0n, 0, 0, 0, false));
  assert.equal(ui.read((state) => state.count), 1);
});

test("a release over a different element clicks neither of them", async () => {
  // Two elements that both answer clicks. Pressing one and releasing over the other must
  // click neither: the press's own element never got the release, and the other one never
  // had a press.
  const { ui } = await started("focus_visible");
  await ui.dispatch(pointer(ui.regionId("button-1"), 0, 0, 0, true, 1));
  await ui.dispatch(pointer(ui.regionId("button-2"), 0, 0, 0, false));
  assert.equal(ui.read((state) => state.focused), 0);
  assert.equal(ui.focused(), undefined);
});

test("leaving a pressed element cancels the press", async () => {
  // The whole lesson of `active_state_bug`: a release over nothing is never reported, so a
  // press that waited for one would stay stuck for good.
  const { ui } = await started("active_state_bug");
  const region = ui.regionId("press");
  await ui.dispatch(pointer(region, 0, 0, 0, true, 1));
  assert.equal(ui.stateOf("press").pressed, true);

  await ui.dispatch(hover(region, false));
  assert.equal(ui.stateOf("press").pressed, false);

  await ui.dispatch(pointer(0n, 0, 0, 0, false));
  assert.equal(ui.read((state) => state.presses), 0);
});

test("a ring is shown by the keyboard and not by a click", async () => {
  const { ui } = await started("focus_visible");
  await ui.dispatch(key(Keys.tab));
  assert.equal(ui.focused(), "button-1");
  assert.equal(ui.stateOf("button-1").focusVisible, true);

  const region = ui.regionId("button-1");
  await ui.dispatch(pointer(region, 0, 0, 0, true, 1));
  await ui.dispatch(pointer(region, 0, 0, 0, false));
  assert.equal(ui.focused(), "button-1");
  assert.equal(ui.stateOf("button-1").focusVisible, false);
});

test("the wheel goes to whatever is hovered", async () => {
  const { ui } = await started("uniform_list");
  // Nothing is hovered, so the wheel reaches no region and the list stays put.
  await ui.dispatch(wheel(120));
  assert.equal(ui.read((state) => state.list.offset), 0);

  await ui.dispatch(hover(ui.regionId("list"), true));
  await ui.dispatch(wheel(120));
  assert.equal(ui.read((state) => state.list.offset), 120);
});

test("a drag drops where it was let go", async () => {
  const { ui } = await started("drag_drop");
  const tray = ui.boundsOf("tray");
  await ui.dispatch(pointer(ui.regionId("tile-0"), 40, 66, 0, true, 1));
  assert.notEqual(ui.read((state) => state.dragging), undefined);
  await ui.dispatch(pointer(0n, 200, tray.y + 20));
  await ui.dispatch(pointer(0n, 200, tray.y + 20, 0, false));
  assert.deepEqual(ui.read((state) => state.dropped), [0]);

  // Released off the tray, a tile goes back rather than being dropped.
  await ui.paint();
  await ui.dispatch(pointer(ui.regionId("tile-1"), 110, 66, 0, true, 1));
  await ui.dispatch(pointer(0n, 300, 8));
  await ui.dispatch(pointer(0n, 300, 8, 0, false));
  assert.deepEqual(ui.read((state) => state.dropped), [0]);
});

test("an accessibility action arrives by the same identity a click uses", async () => {
  const { ui } = await started("a11y");
  await ui.dispatch(accessibility(ui.regionId("volume"), "increment"));
  assert.equal(ui.read((state) => state.volume), 45);
  await ui.dispatch(accessibility(ui.regionId("volume"), "decrement"));
  assert.equal(ui.read((state) => state.volume), 40);

  await ui.dispatch(accessibility(ui.regionId("notifications"), "default"));
  assert.equal(ui.read((state) => state.notifications), false);

  await ui.dispatch(accessibility(ui.regionId("apply"), "click"));
  assert.equal(ui.read((state) => state.applied), 1);
});

test("escape and q end an example", async () => {
  // Escape ends it.
  let started_ = await started("hello_world");
  await started_.ui.dispatch(key(Keys.escape));
  assert.equal(started_.ui.quitting(), true);

  // So does `q`, which is what a hand on the keyboard reaches for first.
  started_ = await started("hello_world");
  await started_.ui.dispatch(key(Keys.letter("q")));
  assert.equal(started_.ui.quitting(), true);

  // A chord is somebody else's shortcut: control-q is not this.
  started_ = await started("hello_world");
  await started_.ui.dispatch(key(Keys.letter("q"), Mods.control));
  assert.equal(started_.ui.quitting(), false);

  // Neither is a release, or a key repeat.
  started_ = await started("hello_world");
  await started_.ui.dispatch(key(Keys.escape, 0, false));
  await started_.ui.dispatch(key(Keys.letter("q"), 0, true, true));
  assert.equal(started_.ui.quitting(), false);
});

test("a window with a field in it spends q on typing", async () => {
  // An example that accepts typed text cannot spend a letter on quitting: `q` goes into the
  // field, and escape is the way out.
  const { ui } = await started("input");
  await ui.dispatch(pointer(ui.regionId("field"), 0, 0, 0, true, 1));
  await ui.dispatch(pointer(ui.regionId("field"), 0, 0, 0, false));
  await ui.dispatch(text("q"));
  assert.equal(ui.quitting(), false);
  assert.equal(ui.read((state) => state.field.text), "q");

  await ui.dispatch(key(Keys.letter("q")));
  assert.equal(ui.quitting(), false);

  await ui.dispatch(key(Keys.escape));
  assert.equal(ui.quitting(), true);
});

test("every example builds a frame the protocol accepts", async () => {
  // Each example, painted twice: once cold, once with the caches warm. A frame the wire
  // would refuse fails inside `paint`, so reaching the end is the assertion.
  const { readdirSync } = await import("node:fs");
  const names = readdirSync(BUILT)
    .filter((file) => file.endsWith(".js") && file !== "ui.js")
    .map((file) => file.slice(0, -3))
    .sort();
  assert.equal(names.length, 32);

  const session = await OverlaySession.connect({ offline: true });
  for (const name of names) {
    const { makeUi } = await load(name);
    const ui = makeUi();
    const window = await session.createWindow({
      bounds: { x: 60, y: 60, width: ui.width, height: ui.height },
    });
    await ui.attach(session, window);
    await ui.paint();
  }
});

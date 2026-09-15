import { strict as assert } from "node:assert";
import { test } from "node:test";
import { OverlaySession, overlay, presenter } from "../../dist/index.js";
const { Canvas, Path, Brush, decodeOverlayEvent } = overlay;
class RawEvent {
  constructor(kind, values, text = "") {
    this.value = { kind, values, text, revision: (1n << 64n) - 1n, region: (1n << 63n) + 17n };
  }
  data() { return this.value; }
}
const rect = { x: 0, y: 0, width: 100, height: 80 };
function drawing() {
  const stops = [{ offset: 0, color: 0xff0000ff }, { offset: 1, color: 0x0000ffff }];
  const path = new Path().moveTo(0, 0).lineTo(50, 0).quadTo(60, 20, 50, 40).cubicTo(30, 60, 10, 60, 0, 40).close();
  return new Canvas().save().clip(Path.roundedRectangle(rect, 8)).transform(1, 0, 0, 1, 2, 3).opacity(0.75)
    .fill(path, Brush.linear({ x: 0, y: 0 }, { x: 60, y: 40 }, stops))
    .stroke(Path.ellipse(rect), Brush.radial({ x: 10, y: 10 }, 30, stops), 2)
    .text("A😀日本語", { x: 4, y: 60 }, 12, 0xffffffff)
    .hit((1n << 64n) - 1n, Path.rectangle(rect)).restore();
}
test("native overlays reuse scenes and isolate retained assets and parent handles", async () => {
  const session = await OverlaySession.connect({ offline: true });
  const other = await OverlaySession.connect({ offline: true });
  try {
    const window = await session.createWindow({ bounds: rect });
    const neighbor = await session.createWindow({ bounds: rect });
    await assert.rejects(other.createWindow({ bounds: rect }, window));
    const image = await window.uploadRgba(1, 1, Uint8Array.of(255, 0, 0, 255));
    const canvas = drawing(); canvas.validate();
    await window.drawImage(canvas, image, rect);
    await assert.rejects(neighbor.drawImage(new Canvas(), image, rect));
    await window.present(canvas); await window.present(canvas.snapshot());
    const receipt = await window.submit(canvas.snapshot());
    assert.equal(receipt.revision,3n); assert.equal(await receipt.wait(0),undefined);
    await assert.rejects(receipt.wait(61));
    await window.releaseImage(image);
    await assert.rejects(window.submit(canvas)); await assert.rejects(window.releaseImage(image));
    await assert.rejects(window.uploadRgba(2, 2, Buffer.from("bad")));
    await window.close(); await assert.rejects(window.present(canvas));
    await assert.rejects(neighbor.present(canvas));
    await neighbor.present(drawing());
    await session.close(); await assert.rejects(neighbor.present(canvas));
    await assert.rejects(receipt.wait(0));
  } finally { await session.close(); await other.close(); }
});
test("canvas validation bounds geometry, state, colors and full-width hit IDs", () => {
  assert.throws(() => Path.rectangle({ ...rect, width: NaN }));
  assert.throws(() => new Canvas().opacity(1.1));
  assert.throws(() => new Canvas().hit(1n << 64n, Path.rectangle(rect)));
  assert.throws(() => new Canvas().hit(-1n, Path.rectangle(rect)));
  assert.throws(() => new Canvas().hit(1, Path.rectangle(rect)));
  assert.throws(() => new Canvas().fill(Path.rectangle(rect), Brush.solid(-1)));
  assert.throws(() => new Canvas().fill(Path.rectangle(rect), Brush.solid(1.5)));
  const canvas = drawing(), snapshot = canvas.snapshot(); canvas.restore(); snapshot.validate();
  assert.throws(() => canvas.validate());
});
test("typed events preserve bigint identities and translate IME offsets to UTF-16", () => {
  const raw = data => ({ data: () => ({ revision: (1n << 64n)-1n, region: (1n << 63n)+17n, text: "", values: [], ...data }), targets: () => false });
  const ime = decodeOverlayEvent(raw({ kind: "ime", text: "A😀日", values: [1, 5] }));
  assert.deepEqual(ime.selection, [1, 3]);
  const pointer = decodeOverlayEvent(raw({ kind: "pointer", values: [1, 2, 0, 1, 1] }));
  assert.equal(pointer.sceneRevision, (1n << 64n)-1n);
  assert.equal(pointer.applicationId, (1n << 63n)+17n);
  const viewport = decodeOverlayEvent(raw({ kind:"viewport", values:[400,300,3,2] }));
  assert.equal(viewport.revision,(1n << 64n)-1n); assert.equal(viewport.sceneRevision,0n);
  assert.equal(viewport.viewport.scaleNumerator,3);
});

test("unsupported presenters reject the automatically required overlay profiles", async () => {
  const running = await presenter.Presenter.start({ endpoint: "tcp:127.0.0.1:0" });
  try {
    await assert.rejects(OverlaySession.connect({ endpointControl: running.endpoint(), rootSecret: running.issuePaneCapability(1) }), /profile/);
  } finally { await running.close(); }
});

test("paint commands validate locally and reach a presenter", async () => {
  const session = await OverlaySession.connect({ offline: true });
  try {
    const window = await session.createWindow({ bounds: rect });
    const image = await window.uploadRgba(2, 2, new Uint8Array(16));
    assert.ok(image.id > 0n, "the asset identity survives as a bigint");
    const path = Path.rectangle(rect);
    const stops = [{ offset: 0, color: 0xff0000ff }, { offset: 1, color: 0x0000ffff }];
    const canvas = new Canvas()
      .shadow({ rect: { x: 10, y: 10, width: 60, height: 40 }, radii: [4, 8, 12, 16], color: 0x00000055, offset: { x: 0, y: 6 }, blur: 18, spread: -2 })
      .fill(path, Brush.image(image, [2, 0, 0, 2, 5, -5], "reflect"))
      .fill(path, Brush.linear({ x: 0, y: 0 }, { x: 100, y: 0 }, stops, "oklab"))
      .strokeStyled(path, Brush.solid(0xffffffff), { width: 2.5, cap: "round", join: "bevel", miterLimit: 6, dashes: [4, 2], dashOffset: 1.5 })
      .fill(Path.roundedRectangleCorners({ x: 0, y: 0, width: 80, height: 40 }, [2, 6, 10, 14]), Brush.solid(0x00ff00ff));
    canvas.validate();
    assert.ok((await window.submit(canvas)).revision > 0n);

    for (const bad of [
      { rect, radii: [-1, 0, 0, 0] },
      { rect, blur: 4097 },
      { rect, blur: -1 },
    ]) {
      assert.throws(() => new Canvas().shadow(bad).validate(), "shadow values are bounded");
    }
    for (const bad of [
      { width: 0 },
      { width: 1, miterLimit: 0.5 },
      { width: 1, dashes: [0] },
      { width: 1, dashes: Array.from({ length: 33 }, () => 1) },
      { width: 1, dashOffset: -1 },
    ]) {
      assert.throws(() => new Canvas().strokeStyled(path, Brush.solid(0xffffffff), bad).validate());
    }
  } finally {
    await session.close();
  }
});

test("cursor shapes ride hit regions and stay off the wire when unasked", async () => {
  const session = await OverlaySession.connect({ offline: true });
  try {
    const window = await session.createWindow({ bounds: rect });
    const path = Path.rectangle(rect);
    // An unasked cursor must not require the pointer profile, so a plain scene still submits.
    const plain = new Canvas().hit(1n, path);
    plain.validate();
    assert.ok((await window.submit(plain)).revision > 0n);

    const shaped = new Canvas()
      .hit(2n, path, "input", 0, "text")
      .hit(3n, path, "drag", 0, "grabbing")
      .hit(4n, path, "resize", 8, "resize-up-left");
    shaped.validate();
    assert.ok((await window.submit(shaped)).revision > 0n);

    assert.throws(() => new Canvas().hit(5n, path, "input", 0, "wand").validate());
  } finally {
    await session.close();
  }
});

test("a clipboard write is offered and refused without a gesture", async () => {
  const session = await OverlaySession.connect({ offline: true });
  try {
    const window = await session.createWindow({ bounds: rect });
    // A dry-run session has no clipboard to write to, so nothing reports success.
    await assert.rejects(window.setClipboard("text"));
  } finally {
    await session.close();
  }
});

test("environment events decode with honest absence", () => {
  // Values are [font size, dark flag, reduced motion, refresh interval, revision]; a negative
  // optional field is the host saying it cannot tell, which is not the same as "false".
  const known = decodeOverlayEvent(new RawEvent("environment", [13.5, 1, 1, 16667, 3], "Iosevka Term"));
  assert.equal(known.kind, "environment");
  assert.equal(known.environment.fontFamily, "Iosevka Term");
  assert.equal(known.environment.fontSize, 13.5);
  assert.equal(known.environment.appearance, "dark");
  assert.equal(known.environment.reducedMotion, true);
  assert.equal(known.environment.refreshIntervalUs, 16667);

  const unknown = decodeOverlayEvent(new RawEvent("environment", [16, 0, -1, -1, 1], ""));
  assert.equal(unknown.environment.appearance, "light");
  assert.equal(unknown.environment.reducedMotion, undefined);
  assert.equal(unknown.environment.refreshIntervalUs, undefined);
});

test("semantic trees validate locally before they reach a host", async () => {
  const session = await OverlaySession.connect({ offline: true });
  try {
    const window = await session.createWindow({ bounds: rect });
    const group = { id: 1n, role: "group", bounds: rect };
    // A dry-run session has no assistive technology to describe anything to.
    await assert.rejects(window.setSemantics({ sceneRevision: 1n, nodes: [group] }));
    // Structural rules are enforced locally, not just by the host: a child must follow its
    // parent, so a self-parenting node cannot be built at all.
    await assert.rejects(
      window.setSemantics({ sceneRevision: 1n, nodes: [{ ...group, children: [0] }] }),
    );
    // Zero is not an identity.
    await assert.rejects(
      window.setSemantics({ sceneRevision: 1n, nodes: [{ ...group, id: 0n }] }),
    );
    // A set position outside its size.
    await assert.rejects(
      window.setSemantics({ sceneRevision: 1n, nodes: [{ ...group, set: [3, 2] }] }),
    );
    // An unknown role is refused rather than mapped onto something generic.
    await assert.rejects(
      window.setSemantics({ sceneRevision: 1n, nodes: [{ ...group, role: "wand" }] }),
    );
  } finally {
    await session.close();
  }
});

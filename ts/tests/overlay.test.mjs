import { strict as assert } from "node:assert";
import { test } from "node:test";
import { OverlaySession, overlay, presenter } from "../../dist/index.js";
const { Canvas, Path, Brush, decodeOverlayEvent } = overlay;
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

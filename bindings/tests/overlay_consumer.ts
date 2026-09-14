// Compile-only consumer of the generated package declarations.
import { OverlaySession, overlay } from "../../dist/index.js";
const { Canvas, Path, Brush } = overlay;
async function producer(): Promise<void> {
  await using session = await OverlaySession.fromEnv();
  await using window = await session.createWindow({ bounds: { x: 10, y: 20, width: 100, height: 80 }, mode: "floating" });
  const path = Path.rectangle({ x: 0, y: 0, width: 100, height: 80 });
  const canvas = new Canvas().fill(path, Brush.solid(0xff0000ff)).hit(1n << 63n, path);
  // @ts-expect-error Hit IDs must not be narrowed to JavaScript numbers.
  canvas.hit(123, path);
  await window.present(canvas);
  const image = await window.uploadRgba(1, 1, Uint8Array.of(255, 0, 0, 255));
  await window.drawImage(canvas, image, { x: 0, y: 0, width: 1, height: 1 });
  const receipt = await window.submit(canvas);
  const submittedRevision: bigint = receipt.revision;
  const measured: overlay.TextMeasurement = await window.measureText("A😀日", 18, { maxWidth: 100 });
  await window.setEditorGeometry(submittedRevision, { x: measured.width, y: 0, width: 1, height: measured.height });
  // @ts-expect-error Scene revisions must retain their full u64 precision.
  await window.setEditorGeometry(123);
  const outcome: overlay.PresentationOutcome | undefined = await receipt.wait(0);
  await window.releaseImage(image);
  const replacement = await window.replaceTrack(new Canvas());
  const status: overlay.OverlayWindowStatus = await window.reconcile();
  const acceptedRevision: bigint = status.acceptedRevision;
  void [submittedRevision, outcome, replacement, acceptedRevision];
  for await (const event of session.events()) {
    const revision: bigint = event.sceneRevision;
    if (event.kind === "pointer" && event.targets(window)) {
      const id: bigint = event.applicationId;
      void id;
    }
    if (event.kind === "ime") {
      const selection: readonly [number, number] | undefined = event.selection;
      void selection;
    }
    if (event.kind === "viewport") {
      const viewportRevision: bigint = event.revision;
      const viewport: overlay.Viewport = event.viewport;
      void [viewportRevision, viewport];
    }
    if (event.kind === "connection-lost") break;
    void revision;
  }
}
void producer;
async function textServices(window: import("../../dist/overlay.js").OverlayWindow): Promise<void> {
  const text: overlay.StyledText = { runs: [{ text: "Hello", style: { size: 20, underline: true } }], maxWidth: 120, maxLines: 2 };
  const measured: readonly overlay.TextMeasurement[] = await window.measureTextBatch([text]);
  const layouts: readonly overlay.RetainedTextLayout[] = await window.layoutTextBatch([text]);
  const scene = new overlay.Canvas();
  await window.drawTextLayout(scene, layouts[0]!, { x: 4, y: 8 });
  await window.releaseTextLayout(layouts[0]!);
  void measured;
}
void textServices;

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
    if (event.kind === "connection-lost") break;
    void revision;
  }
}
void producer;

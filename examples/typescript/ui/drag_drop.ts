// Tiles dragged onto a tray, with pointer capture.
//
// TypeScript twin of `vivid_ui/examples/drag_drop`. A drag is three events and one piece of
// state: a press records what is moving, moves carry it, and the release decides where it
// landed. Capture is what makes the middle one work — without it the moves stop the moment
// the pointer leaves the tile it started on, and the release is never heard at all.
//
// Where the tray *is* comes from the frame before this one: the host hit-tested against that
// frame, so it is the only geometry an event can be talking about.
import { overlay } from "@vivido/vivid-sdk";
import { WHITE, rect, runMain, Ui, type Frame } from "./ui.js";

const TILES = ["alpha", "beta", "gamma"] as const;

class DragDrop {
  // The tile being dragged, and where the pointer has it.
  dragging: readonly [number, overlay.Point] | undefined;
  dropped: number[] = [];

  isDropped(index: number): boolean {
    return this.dropped.includes(index);
  }
}

async function render(state: DragDrop, f: Frame): Promise<void> {
  f.box(rect(0, 0, f.width, f.height), { bg: 0x101018ff });
  f.caption("Drag a tile into the tray", { x: 20, y: 20 });

  const rowY = 46;
  for (const [index, label] of TILES.entries()) {
    const tile = rect(20 + index * 74, rowY, 64, 40);
    const held = state.dragging?.[0] === index;
    f.region(`tile-${index}`, tile, {
      radius: 6,
      cursor: "grab",
      onMouseDown: (event) => {
        state.dragging = [index, event.position];
        // Capture keeps the moves coming after the pointer leaves the tile, so the release
        // is heard wherever it happens.
        f.ui.capture(true);
        f.notify();
      },
    });
    f.box(tile, { bg: held ? 0x8a6a20ff : 0x3a4a6aff, radius: 6 });
    await f.label(label, tile, { size: 12, color: WHITE, align: "center" });
  }

  const tray = rect(20, rowY + 52, f.width - 40, 64);
  f.region("tray", tray, { radius: 8 });
  f.box(tray, { radius: 8, border: 1, borderColor: 0x50506aff });
  for (const [slot, index] of state.dropped.entries()) {
    const chip = rect(tray.x + 10 + slot * 56, tray.y + (tray.height - 28) / 2, 48, 28);
    f.box(chip, { bg: 0x2a4a3aff, radius: 4 });
    await f.label(TILES[index]!, chip, { size: 11, color: 0xc0e0d0ff, align: "center" });
  }

  // The tile being dragged follows the pointer, drawn last so it is over everything.
  if (state.dragging) {
    const [index, position] = state.dragging;
    const ghost = rect(position.x, position.y, 64, 40);
    f.box(ghost, { bg: 0x6a7ab0ff, radius: 6 });
    await f.label(TILES[index]!, ghost, { size: 12, color: WHITE, align: "center" });
  }
}

export function makeUi(): Ui<DragDrop> {
  const state = new DragDrop();
  const ui = new Ui(state, render, { width: 420, height: 260 });

  ui.onMouseMove = (event) => {
    if (state.dragging) {
      state.dragging = [state.dragging[0], event.position];
      ui.notify();
    }
  };

  // A release outside every region is reported to nobody, which is what capture is for:
  // with it the release arrives here, naming whatever region it happened over.
  ui.onMouseUp = () => {
    if (!state.dragging) return;
    const [index, position] = state.dragging;
    state.dragging = undefined;
    // Dropping on the tray is dropping inside it; anywhere else puts the tile back.
    const tray = ui.boundsOf("tray");
    const inside =
      tray !== undefined &&
      position.x >= tray.x &&
      position.x < tray.x + tray.width &&
      position.y >= tray.y &&
      position.y < tray.y + tray.height;
    if (inside && !state.isDropped(index)) state.dropped.push(index);
    ui.capture(false);
    ui.notify();
  };

  return ui;
}

await runMain(makeUi, import.meta.url);

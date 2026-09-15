// Marks as wide as the device was pressed.
//
// TypeScript twin of `vivid_ui/examples/mouse_pressure`. Pressure is only sent by hardware
// that reports it: a host with no such device sends nothing at all rather than zero, so a
// mark made by an ordinary mouse falls back to the base width instead of collapsing to
// nothing.
//
// That distinction is the example. It is also why pressure arrives as its own event beside
// the move rather than as a field nobody can trust.
import { overlay } from "@vivido/vivid-sdk";
import { rect, runMain, Ui, type Frame } from "./ui.js";

const BASE_WIDTH = 6;

/** How wide a mark is at this pressure. A host that reports none gives the base width. */
function widthFor(pressure: number): number {
  return BASE_WIDTH * (0.4 + Math.min(Math.max(pressure, 0), 1) * 1.6);
}

class Pressure {
  // Each mark is a position and the pressure the device reported with it.
  readonly points: (readonly [overlay.Point, number])[] = [];
  sawPressure = false;

  push(position: overlay.Point, pressure: number): void {
    this.points.push([position, pressure]);
    if (pressure > 0) this.sawPressure = true;
  }
}

async function render(state: Pressure, f: Frame): Promise<void> {
  f.box(rect(0, 0, f.width, f.height), { bg: 0x101018ff });
  await f.label(
    state.sawPressure ? "pressure device detected" : "press and draw",
    rect(20, 20, f.width - 40, 14),
    { size: 12, color: 0x8080a0ff, vcenter: false },
  );

  const pad = rect(20, 44, 380, 200);
  f.region("pad", pad, {
    radius: 8,
    cursor: "crosshair",
    // A plain click has no pressure to report, so it draws at half — the value the toolkit
    // uses when a press arrives without a sensor behind it.
    onMouseDown: (event) => {
      state.push(event.position, 0.5);
      f.notify();
    },
    onPressure: (event) => {
      if (event.pressure !== undefined) {
        state.push(event.position, event.pressure);
        f.notify();
      }
    },
  });
  f.box(pad, { bg: 0x14141cff, radius: 8 });

  // Each mark is a dot as wide as the device was pressed, drawn over the pad it is on.
  f.canvas.save();
  f.clip(pad, 8);
  for (const [position, value] of state.points) {
    const width = widthFor(value);
    f.box(rect(position.x - width / 2, position.y - width / 2, width, width), {
      bg: 0x8ecbffff,
      radius: width / 2,
    });
  }
  f.canvas.restore();
}

export function makeUi(): Ui<Pressure> {
  return new Ui(new Pressure(), render, { width: 420, height: 280 });
}

await runMain(makeUi);

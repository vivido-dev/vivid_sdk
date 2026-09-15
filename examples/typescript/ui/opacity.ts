// A subtree painted as one translucent group.
//
// TypeScript twin of `vivid_ui/examples/opacity`: the group's opacity follows where the
// pointer is — left is transparent, right is solid — and everything inside the group fades
// together, because the display list carries it as one layer rather than as parts.
import { rect, runMain, Ui, type Frame } from "./ui.js";

const WIDTH = 360;
const HEIGHT = 260;

class Fade {
  opacity = 1;

  /** Opacity from a pointer position across the window: left is transparent, right solid. */
  static fromPointer(x: number, width: number): number {
    return Math.min(Math.max(x / width, 0), 1);
  }
}

async function render(state: Fade, f: Frame): Promise<void> {
  f.region("surface", rect(0, 0, f.width, f.height), {
    onMouseMove: (event) => {
      const next = Fade.fromPointer(event.position.x, WIDTH);
      if (Math.abs(next - state.opacity) > 0.001) {
        state.opacity = next;
        f.notify();
      }
    },
  });
  f.box(rect(0, 0, f.width, f.height), { bg: 0x141420ff });
  await f.label("Move the pointer left and right", rect(20, 20, f.width - 40, 16), {
    size: 13,
    color: 0xa0a0b8ff,
    vcenter: false,
  });

  // One translucent group: everything between the save and the restore is a layer the host
  // composites at this opacity, so the bar and both runs of text fade together.
  f.canvas.save();
  f.canvas.opacity(state.opacity);
  f.box(rect(20, 46, f.width - 40, 100), { bg: 0x4a5fd0ff, radius: 8 });
  await f.label("Fading group", rect(32, 58, 200, 24), { size: 20, color: 0xffffffff, vcenter: false });
  await f.label("Everything in here is one group", rect(32, 88, 260, 16), {
    size: 13,
    color: 0xd0d0e0ff,
    vcenter: false,
  });
  f.box(rect(32, 110, 120, 24), { bg: 0x9ecbffff, radius: 4 });
  f.canvas.restore();
}

export function makeUi(): Ui<Fade> {
  return new Ui(new Fade(), render, { width: WIDTH, height: HEIGHT });
}

await runMain(makeUi);

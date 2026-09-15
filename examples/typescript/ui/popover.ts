// A floating panel anchored to the button that opened it.
//
// TypeScript twin of `vivid_ui/examples/popover`: the panel paints over everything it
// overlaps (late in the display list is on top), and a scrim behind it takes the clicks
// meant for the rest of the window — which is how clicking outside closes it. The scrim is
// an ordinary region rather than a blocking one: it has to *hear* the click that closes the
// panel, and a blocking region reports nothing by design.
import { rect, runMain, Ui, type Frame } from "./ui.js";

class Popover {
  open = false;
}

async function render(state: Popover, f: Frame): Promise<void> {
  f.box(rect(0, 0, f.width, f.height), { bg: 0x101018ff });
  f.caption("Anchored to the button above", { x: 20, y: 20 });

  const close = (): void => {
    state.open = false;
    f.notify();
  };

  await f.button("menu-button", rect(20, 44, 140, 36), "Open menu", {
    onClick: () => {
      state.open = !state.open;
      f.notify();
    },
  });

  if (!state.open) return;

  // The scrim covers the window, painted over the content it silences. Clicking it is what
  // closes the panel — so it is a region that hears clicks, not one that blocks.
  f.region("scrim", rect(0, 0, f.width, f.height), { onClick: close });
  f.box(rect(0, 0, f.width, f.height), { bg: 0x00000066 });

  // The menu paints after the scrim, so it sits over it: paint order is z-order, and a
  // region painted later is the one the host reports. The panel needs a region of its own
  // for exactly that reason — without it a click on a row that answers nothing falls
  // through to the scrim, and the menu closes when you press one of its own entries.
  const menu = rect(0, 46, 200, 72);
  f.shadowBox(menu, "lg", 8);
  f.box(menu, { bg: 0x24243aff, radius: 8 });
  f.region("menu", menu, { radius: 8 });
  f.region("menu-copy", rect(6, 52, 120, 20), { onClick: close });
  for (const [index, label] of ["Copy", "Paste", "Select all"].entries()) {
    await f.label(label, rect(6, 52 + index * 20, 120, 20), { size: 14, color: 0xe6e6f0ff });
  }
}

export function makeUi(): Ui<Popover> {
  return new Ui(new Popover(), render, { width: 420, height: 300 });
}

await runMain(makeUi);

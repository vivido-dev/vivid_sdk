// A focus ring that a keyboard shows and a click does not.
//
// TypeScript twin of `vivid_ui/examples/focus_visible`: focus that arrived from the
// keyboard is drawn with a ring, focus that arrived from a click is not. That is the whole
// difference between "the keyboard put it here" and "the pointer put it here", and it is
// why the helper tracks the two separately.
import { Keys, Mods, WHITE, rect, type Frame, Ui, runMain } from "./ui.js";

const BUTTONS = 3;

class FocusRings {
  focused = 0;
}

async function render(state: FocusRings, f: Frame): Promise<void> {
  f.box(rect(0, 0, f.width, f.height), { bg: 0x101018ff });
  f.caption("Tab shows a ring; clicking does not", { x: 20, y: 20 });

  const rowY = 20 + 14 + 12;
  for (let index = 0; index < BUTTONS; index += 1) {
    await f.button(`button-${index}`, rect(20 + index * (96 + 10), rowY, 96, 36), `${index + 1}`, {
      base: 0x2a2a3cff,
      hover: 0x36364cff,
      textColor: WHITE,
      focusable: true,
      // A click focuses without claiming the keyboard put it there.
      onClick: () => {
        state.focused = index;
        f.ui.focus(`button-${index}`, { visible: false });
      },
    });
  }
}

export function makeUi(): Ui<FocusRings> {
  const state = new FocusRings();
  const ui = new Ui(state, render, { width: 360, height: 220 });

  ui.onKey = (event) => {
    if (!(event.down && !event.repeat && event.physical === Keys.tab)) return;
    const step = event.modifiers & Mods.shift ? -1 : 1;
    state.focused = (state.focused + step + BUTTONS) % BUTTONS;
    ui.focus(`button-${state.focused}`, { visible: true });
  };

  return ui;
}

await runMain(makeUi, import.meta.url);

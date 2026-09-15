// A button that must not stay pressed.
//
// TypeScript twin of `vivid_ui/examples/active_state_bug`, and the regression test for the
// whole press contract: a press is active exactly while the button is held over the element
// it started on, a release anywhere ends it, and leaving the element while pressed cancels
// it — because a release over nothing is not reported at all.
import { WHITE, rect, type Frame, Ui, runMain } from "./ui.js";

class Toggle {
  presses = 0;
}

async function render(state: Toggle, f: Frame): Promise<void> {
  f.box(rect(0, 0, f.width, f.height), { bg: 0x14141cff });
  await f.label(`Pressed ${state.presses} times`, rect(20, 20, f.width - 40, 18), {
    size: 14,
    color: 0xc0c0d0ff,
    vcenter: false,
  });
  await f.button("press", rect(20, 48, 160, 44), "Hold me", {
    base: 0x3a3a52ff,
    hover: 0x4a4a68ff,
    // Amber is the active colour, so a stuck press would be unmistakable.
    active: 0x8a6a20ff,
    textColor: WHITE,
    onClick: () => {
      state.presses += 1;
      f.notify();
    },
  });
}

export function makeUi(): Ui<Toggle> {
  return new Ui(new Toggle(), render, { width: 320, height: 160 });
}

await runMain(makeUi, import.meta.url);

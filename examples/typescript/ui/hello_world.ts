// A counter, a button, and a line of text that follows it — the smallest app there is.
//
// TypeScript twin of `vivid_ui/examples/hello_world`: state renders itself into one frame,
// the host does the hit testing, and nothing repaints until the state says it changed.
import { Ui, rect, type Frame, runMain } from "./ui.js";

class Counter {
  count = 0;

  /** The window's own background follows the count's parity, so a click is visible even
   *  before the label is read. */
  background(): number {
    return this.count % 2 === 0 ? 0x1e1e2eff : 0x2a2a3cff;
  }
}

async function render(state: Counter, f: Frame): Promise<void> {
  // p(20), gap(12): the label, then the button under it.
  f.box(rect(0, 0, f.width, f.height), { bg: state.background() });
  await f.label(`Clicked ${state.count} times`, rect(20, 20, f.width - 40, 28), {
    size: 24,
    vcenter: false,
  });
  const text = "Increment";
  const width = (await f.measure(text, 13)).width;
  await f.button("increment", rect(20, 60, width + 28, 32), text, {
    onClick: () => {
      state.count += 1;
      f.notify();
    },
  });
}

export function makeUi(): Ui<Counter> {
  return new Ui(new Counter(), render, { width: 420, height: 240 });
}

await runMain(makeUi);

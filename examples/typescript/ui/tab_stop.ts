// Tab and shift-tab moving focus between fields.
//
// TypeScript twin of `vivid_ui/examples/tab_stop`. Tab order is paint order, which is the
// only order a display list has — a frame draws its regions in sequence, and that sequence
// is what "next" means. Clicking a field focuses it too, but without the ring: only the
// keyboard claims to have put the focus there.
import { Keys, Mods, TextField, rect, runMain, Ui, type Frame } from "./ui.js";

const LABELS = ["first", "second", "third"] as const;

class TabStops {
  readonly fields = LABELS.map(() => new TextField("", 14));

  /** Which field the keys are going to, if any. */
  underFocus(ui: Ui<TabStops>): number {
    const focused = ui.focused();
    for (let index = 0; index < this.fields.length; index += 1) {
      if (focused === `row-${index}`) return index;
    }
    return -1;
  }
}

async function render(state: TabStops, f: Frame): Promise<void> {
  f.box(rect(0, 0, f.width, f.height), { bg: 0x101018ff });

  for (const [index, field] of state.fields.entries()) {
    const row = rect(20, 20 + index * 52, f.width - 40, 44);
    const element = f.region(`row-${index}`, row, {
      radius: 6,
      cursor: "text",
      focusable: true,
      // A click focuses without claiming the keyboard put it there.
      onClick: () => f.ui.focus(`row-${index}`, { visible: false }),
    });
    const active = element.focused;
    f.box(row, {
      bg: active ? 0x24304aff : 0x181824ff,
      radius: 6,
      border: 1,
      borderColor: active ? 0x8ecbffff : 0x2a2a3cff,
    });
    if (element.focusVisible) f.focusRing(row, 6);

    const inner = rect(row.x + 8, row.y, row.width - 16, row.height);
    const selection = await field.selectionRect(f, inner);
    if (selection) f.box(selection, { bg: 0x2a4a7aff });
    const drawn = field.drawn();
    await f.label(drawn || LABELS[index]!, inner, {
      size: field.size,
      color: drawn ? 0xe6e6f0ff : 0x50506aff,
    });
    if (active) f.box(await field.caretRect(f, inner), { bg: 0x8ecbffff });
  }
}

export function makeUi(): Ui<TabStops> {
  const state = new TabStops();
  const ui = new Ui(state, render, { width: 360, height: 240 });

  ui.onKey = async (event) => {
    if (event.down && !event.repeat && event.physical === Keys.tab) {
      ui.focusNext({ backwards: Boolean(event.modifiers & Mods.shift), visible: true });
      return;
    }
    const index = state.underFocus(ui);
    if (index >= 0 && (await state.fields[index]!.handleKey(event, ui))) ui.notify();
  };
  ui.onText = (event) => {
    const index = state.underFocus(ui);
    if (index >= 0 && state.fields[index]!.handleText(event)) ui.notify();
  };
  ui.onIme = (event) => {
    const index = state.underFocus(ui);
    if (index >= 0 && state.fields[index]!.handleIme(event)) ui.notify();
  };

  return ui;
}

await runMain(makeUi, import.meta.url);

// A text field: typing, selection, clipboard, and composition.
//
// TypeScript twin of `vivid_ui/examples/input`. Typing, selection, arrow keys, and copy/cut
// all come from the protocol's own keys — the field answers HID usages, so the same code
// types the same way on every host. Paste arrives as committed text through the host's own
// policy: a terminal-hosted editor never reads the clipboard, it only writes one.
//
// The caret is read off the measurement rather than counted in characters, and it is
// published to the host each frame so an input method knows where to put its candidate
// window.
import { Keys, TextField, platform, rect, runMain, Ui, type Frame } from "./ui.js";

const PLACEHOLDER = "a line of text";

class Editor {
  readonly field = new TextField("", 16);
  copies = 0;
}

async function render(state: Editor, f: Frame): Promise<void> {
  f.box(rect(0, 0, f.width, f.height), { bg: 0x101018ff });
  await f.label("Type here", rect(20, 20, f.width - 40, 14), {
    size: 12,
    color: 0x8080a0ff,
    vcenter: false,
  });

  const box = rect(20, 46, f.width - 40, 40);
  const focused = f.region("field", box, {
    radius: 6,
    cursor: "text",
    focusable: true,
    // A click focuses the field without claiming the keyboard put it there.
    onClick: () => f.ui.focus("field", { visible: false }),
  });
  f.box(box, { bg: 0x1c1c28ff, radius: 6, border: 1, borderColor: 0x30304aff });
  if (focused.focusVisible) f.focusRing(box, 6);

  const inner = rect(box.x + 10, box.y, box.width - 20, box.height);
  const field = state.field;
  const drawn = field.drawn();

  // The selection is drawn under the text, from the measured edges of what it covers.
  const selection = await field.selectionRect(f, inner);
  if (selection) f.box(selection, { bg: 0x2a4a7aff });

  await f.label(drawn || PLACEHOLDER, inner, {
    size: field.size,
    color: drawn ? 0xe6e6f0ff : 0x50506aff,
  });

  if (focused.focused) {
    const caret = await field.caretRect(f, inner);
    f.box(caret, { bg: 0x8ecbffff });
    // Where the host should put an input method's candidate window.
    f.publishCaret(caret);
  }

  await f.label(
    `${field.text.length} characters, ${state.copies} copied`,
    rect(20, box.y + 52, f.width - 40, 14),
    { size: 12, color: 0x8080a0ff, vcenter: false },
  );
}

export function makeUi(): Ui<Editor> {
  const state = new Editor();
  const ui = new Ui(state, render, { width: 420, height: 240 });
  const command = platform();

  ui.onKey = async (event) => {
    if (event.down && !event.repeat && event.modifiers & command) {
      // Copy and cut are the application's: the host writes the clipboard, and only after a
      // gesture in this window, which a key press is.
      if (event.physical === Keys.letter("c") || event.physical === Keys.letter("x")) {
        const selected = state.field.selectedText();
        if (selected) {
          ui.copy(selected);
          state.copies += 1;
          if (event.physical === Keys.letter("x")) state.field.insert("");
          ui.notify();
        }
        return;
      }
    }
    if (event.down && !event.repeat && event.physical === Keys.tab) {
      ui.focus("field", { visible: true });
      return;
    }
    // The field measures to move by clusters, and a key arrives between paints — so it
    // measures through the window, which is where the cache lives.
    if (await state.field.handleKey(event, ui)) ui.notify();
  };
  ui.onText = (event) => {
    if (state.field.handleText(event)) ui.notify();
  };
  ui.onIme = (event) => {
    if (state.field.handleIme(event)) ui.notify();
  };

  return ui;
}

await runMain(makeUi, import.meta.url);

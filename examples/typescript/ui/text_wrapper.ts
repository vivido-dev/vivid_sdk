// Wrapping, ellipsis, and a line ceiling.
//
// TypeScript twin of `vivid_ui/examples/text_wrapper`. None of this is arithmetic a producer
// can do: where a line breaks depends on the font the host chose, so wrapping, truncation,
// and the ellipsis that marks it are all the host's, asked for through a shaped paragraph.
//
// That is the whole difference between the plain text command and a retained layout. The
// plain one draws a run; this one is a paragraph with a width, a line ceiling, and an
// opinion about what to do when it runs out of room.
import { CAPTION, rect, runMain, Ui, type Frame } from "./ui.js";

const PARAGRAPH =
  "The quick brown fox jumps over the lazy dog, and then keeps going for long enough " +
  "that the line it is on runs out of room and has to be broken somewhere sensible.";
const WIDTH = 150;

class TextWrapper {}

async function render(_state: TextWrapper, f: Frame): Promise<void> {
  f.box(rect(0, 0, f.width, f.height), { bg: 0x101018ff });

  const samples: readonly (readonly [string, string, boolean, number | undefined])[] = [
    ["wrapped", "wrapped", true, undefined],
    ["ellipsized", "one line, ellipsized", false, 1],
    ["clamped", "two lines, ellipsized", false, 2],
  ];
  for (const [index, [key, label, wrap, maxLines]] of samples.entries()) {
    const x = 20 + index * (WIDTH + 16);
    await f.label(label, rect(x, 20, WIDTH, 13), { size: 11, color: CAPTION, vcenter: false });
    const box = rect(x, 38, WIDTH, f.height - 58);
    // Clipped, because a paragraph that overruns its box is the box's business: the ceiling
    // decides how many lines exist, the clip decides how many are seen.
    f.canvas.save();
    f.clip(box);
    await f.paragraph(
      key,
      {
        runs: [{ text: PARAGRAPH, style: { size: 13, color: 0xd0d0e0ff } }],
        maxWidth: WIDTH,
        wrap: wrap || maxLines !== undefined,
        maxLines,
        overflow: maxLines !== undefined ? "ellipsis" : "clip",
      },
      { x: box.x, y: box.y },
    );
    f.canvas.restore();
  }
}

export function makeUi(): Ui<TextWrapper> {
  return new Ui(new TextWrapper(), render, { width: 560, height: 320 });
}

await runMain(makeUi, import.meta.url);

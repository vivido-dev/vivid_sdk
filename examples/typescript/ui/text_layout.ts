// Alignment, weight, slant, and decorations.
//
// TypeScript twin of `vivid_ui/examples/text_layout`: everything here is one paragraph per
// block, so the alignment is the paragraph's own — a line centered inside a box it fills,
// not a box that was moved. Decorations and mixed-weight runs are shaped paragraphs, because
// the plain text command carries neither underlines nor several runs.
import { overlay } from "@vivido/vivid-sdk";
import { TEXT, rect, runMain, Ui, type Frame } from "./ui.js";

const BLOCK = 360;
const LABEL = 0x8080a0ff;

class TextLayout {}

async function render(_state: TextLayout, f: Frame): Promise<void> {
  f.box(rect(0, 0, f.width, f.height), { bg: 0x101018ff });
  let y = 20;

  /** A labelled 360-wide well. Returns the box the paragraph goes in. */
  const block = async (label: string): Promise<overlay.Rect> => {
    await f.label(label, rect(20, y, BLOCK, 13), { size: 11, color: LABEL, vcenter: false });
    const well = rect(20, y + 17, BLOCK, 32);
    f.box(well, { bg: 0x20202cff, radius: 4 });
    y = well.y + 44;
    return rect(well.x + 6, well.y + 6, BLOCK - 12, 20);
  };

  // The three alignments, each one run placed by its own measured width.
  for (const [label, text] of [
    ["start", "Aligned to the start edge"],
    ["center", "Aligned to the center"],
    ["end", "Aligned to the end edge"],
  ] as const) {
    await f.label(text, await block(label), {
      size: 15,
      color: TEXT,
      align: label,
      vcenter: false,
    });
  }

  // A decoration is a shaped run: the plain command carries neither underlines nor
  // strikethroughs, so each of these goes through a retained layout.
  const decorated = await block("decorations");
  await f.paragraph(
    "decorations",
    { runs: [{ text: "underlined", style: { size: 15, color: 0x8ecbffff, underline: true } }] },
    { x: decorated.x, y: decorated.y },
  );

  // Mixed emphasis is one paragraph of several runs, not several boxes beside each other.
  const weights = await block("weights");
  await f.paragraph(
    "weights",
    {
      runs: [
        { text: "regular ", style: { size: 16, color: TEXT } },
        { text: "bold ", style: { size: 16, color: TEXT, weight: 700 } },
        { text: "italic", style: { size: 16, color: TEXT, italic: true } },
      ],
    },
    { x: weights.x, y: weights.y },
  );

  const struck = await block("struck through");
  await f.paragraph(
    "struck",
    {
      runs: [
        { text: "a line that was wrong", style: { size: 15, color: TEXT, strikethrough: true } },
      ],
    },
    { x: struck.x, y: struck.y },
  );
}

export function makeUi(): Ui<TextLayout> {
  return new Ui(new TextLayout(), render, { width: 420, height: 460 });
}

await runMain(makeUi, import.meta.url);

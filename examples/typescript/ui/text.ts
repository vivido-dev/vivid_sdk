// A type specimen, and the typography the wire carries.
//
// TypeScript twin of `vivid_ui/examples/text`. The plain text command carries a size, a
// weight, a slant, a family and the two decorations — everything on the first three rows.
// Letter and word spacing, a line height, and the font's own features ride
// `overlay-typography-v1`, and a paragraph that asks for any of them must go through a
// shaped layout or the request is silently dropped.
//
// That distinction is invisible on screen and decides what the frame costs: a plain run is
// one command, a shaped paragraph is a layout the host holds until it is replaced.
import { overlay } from "@vivido/vivid-sdk";
import { rect, runMain, Ui, type Frame } from "./ui.js";

// Empty means the host's own default, which is what a terminal-friendly face is.
const FAMILIES = ["", "Menlo", "Georgia"] as const;
const RAMP = [11, 14, 18, 24, 32] as const;

class Specimen {}

async function render(_state: Specimen, f: Frame): Promise<void> {
  f.box(rect(0, 0, f.width, f.height), { bg: 0x12121aff });
  let y = 20;

  /** A labelled band. Returns the box its samples go in. */
  const section = async (label: string, height: number): Promise<overlay.Rect> => {
    await f.label(label, rect(20, y, f.width - 40, 12), {
      size: 10,
      color: 0x606078ff,
      vcenter: false,
    });
    const box = rect(20, y + 15, f.width - 40, height);
    y = box.y + height + 10;
    return box;
  };

  // The size ramp, sitting on one baseline — which is what makes it a ramp rather than a
  // row of boxes. The baseline comes from the measurement, not from the size.
  let box = await section("size", 38);
  let x = box.x;
  let baseline = 0;
  for (const size of RAMP) baseline = Math.max(baseline, await f.baselineOf(`${size}`, size));
  for (const size of RAMP) {
    const text = `${size}`;
    await f.label(text, rect(x, box.y + baseline - (await f.baselineOf(text, size)), 60, size * 1.4), {
      size,
      color: 0xd8d8e8ff,
      vcenter: false,
    });
    x += (await f.measure(text, size)).width + 14;
  }

  // Weight, slant, and the two decorations. Decorations need shaping; the rest do not.
  box = await section("weight and slant", 20);
  x = box.x;
  for (const [label, weight, italic] of [
    ["regular", 400, false],
    ["bold", 700, false],
    ["italic", 400, true],
  ] as const) {
    await f.label(label, rect(x, box.y, 90, 20), { size: 16, color: 0xd8d8e8ff, weight, italic });
    x += (await f.measure(label, 16, { weight, italic })).width + 14;
  }
  for (const [key, label, underline, strikethrough] of [
    ["under", "under", true, false],
    ["struck", "struck", false, true],
  ] as const) {
    await f.paragraph(
      key,
      { runs: [{ text: label, style: { size: 16, color: 0xd8d8e8ff, underline, strikethrough } }] },
      { x, y: box.y },
    );
    x += (await f.measure(label, 16)).width + 14;
  }

  // Families. A host without the named family substitutes its own, which is why this names
  // the family rather than promising the glyphs.
  box = await section("family", 20);
  x = box.x;
  for (const family of FAMILIES) {
    const label = family || "default";
    await f.label(label, rect(x, box.y, 100, 20), { size: 16, color: 0xd8d8e8ff, family });
    x += (await f.measure(label, 16, { family })).width + 14;
  }

  // The typography the plain command cannot carry. Each of these is shaped.
  box = await section("spacing and leading", 90);
  await f.paragraph(
    "tracked",
    {
      runs: [{ text: "loose tracking, wide word spacing", style: { size: 15, color: 0x9ecbffff } }],
      letterSpacing: 2.5,
      wordSpacing: 6,
    },
    { x: box.x, y: box.y },
  );
  // A line height is a measurement as well as a drawing instruction, so this paragraph
  // occupies the taller box the layout was told about, not only the taller box on screen.
  await f.paragraph(
    "leading",
    {
      runs: [
        {
          text: "one line\nset on a taller rhythm\nthan the font would choose",
          style: { size: 14, color: 0x9ecbffff },
        },
      ],
      lineHeight: 28,
    },
    { x: box.x, y: box.y + 26 },
  );
}

export function makeUi(): Ui<Specimen> {
  return new Ui(new Specimen(), render, { width: 520, height: 400 });
}

await runMain(makeUi);

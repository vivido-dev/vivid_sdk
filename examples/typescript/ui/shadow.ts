// The shadow token scale, and the parts of a shadow.
//
// TypeScript twin of `vivid_ui/examples/shadow`. A shadow is its own display-list command —
// a rectangle, four radii, a colour, an offset, a blur, a spread, and an inset flag — so the
// cards here are white boxes with one command behind each. This is also the one example on a
// light background, which is what makes a soft shadow visible at all.
import { overlay } from "@vivido/vivid-sdk";
import { rect, runMain, Ui, type Frame } from "./ui.js";

const TOKENS = ["xs", "sm", "base", "md", "lg", "xl"] as const;

const CARD = [120, 70] as const;
const GAP = 16;
const RADIUS = 8;

// The three past the scale: a hard offset, a spread, and one cast inward.
const CUSTOM: readonly (readonly [string, readonly [number, number], number, number, number, boolean])[] = [
  ["offset", [6, 6], 0, 0, 0x203050ff, false],
  ["spread", [0, 2], 6, 4, 0x80202080, false],
  ["inset", [0, 3], 8, 0, 0x00000060, true],
];

// The token scale, as the Rust toolkit names it: offset, blur, spread.
const SCALE: Record<string, readonly [number, number, number, number]> = {
  xs: [0, 1, 2, 0],
  sm: [0, 1, 3, 0],
  base: [0, 4, 6, -1],
  md: [0, 6, 12, -2],
  lg: [0, 10, 15, -3],
  xl: [0, 20, 25, -5],
};

class Shadows {}

/** A white card with one shadow behind it. The shadow is cast first: it is what the card
 *  sits on, not something drawn over it. */
async function card(f: Frame, box: overlay.Rect, label: string, shadow: overlay.Shadow): Promise<void> {
  f.canvas.shadow(shadow);
  f.box(box, { bg: 0xffffffff, radius: RADIUS });
  await f.label(label, box, { size: 12, color: 0x333344ff, align: "center" });
}

async function render(_state: Shadows, f: Frame): Promise<void> {
  f.box(rect(0, 0, f.width, f.height), { bg: 0xededf2ff });

  for (const [index, token] of TOKENS.entries()) {
    const box = rect(24 + index * (CARD[0] + GAP), 24, CARD[0], CARD[1]);
    const [ox, oy, blur, spread] = SCALE[token]!;
    await card(f, box, token, {
      rect: box,
      radii: [RADIUS, RADIUS, RADIUS, RADIUS],
      color: 0x0000000a,
      offset: { x: ox, y: oy },
      blur,
      spread,
    });
  }

  const rowY = 24 + CARD[1] + GAP;
  for (const [index, [label, offset, blur, spread, color, inset]] of CUSTOM.entries()) {
    const box = rect(24 + index * (CARD[0] + GAP), rowY, CARD[0], CARD[1]);
    await card(f, box, label, {
      rect: box,
      radii: [RADIUS, RADIUS, RADIUS, RADIUS],
      color,
      offset: { x: offset[0], y: offset[1] },
      blur,
      spread,
      inset,
    });
  }
}

export function makeUi(): Ui<Shadows> {
  return new Ui(new Shadows(), render, { width: 440, height: 420 });
}

await runMain(makeUi);

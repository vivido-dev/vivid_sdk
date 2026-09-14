import { createInterface } from "node:readline/promises";
import { setTimeout as sleep } from "node:timers/promises";

// Four tightly packed sRGB RGBA8 pixels: red, green, blue, white.
export const PIXELS = Uint8Array.from([
  255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
]);

export function duration(args: string[]): number | undefined {
  if (args.length === 0) return undefined;
  const seconds = Number(args[1]);
  if (args.length !== 2 || args[0] !== "--duration" || args[1]?.trim() === ""
      || !Number.isFinite(seconds) || seconds < 0 || seconds > 3600) {
    throw new Error("usage: [--duration SECONDS], with 0..3600 seconds");
  }
  return seconds;
}

export async function hold(seconds: number | undefined): Promise<void> {
  if (seconds !== undefined) {
    await sleep(seconds * 1000);
    return;
  }
  const input = createInterface({ input: process.stdin, output: process.stderr });
  try {
    await input.question("Press Enter to remove the presentation. ");
  } finally {
    input.close();
  }
}

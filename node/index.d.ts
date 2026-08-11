import type { ChildProcess } from "node:child_process";

export interface ShowOptions {
  zoom?: number;
  inline?: boolean;
  noWait?: boolean;
}

export function viviPathFromEnv(env?: NodeJS.ProcessEnv): string;

export class PaneSession {
  constructor(env?: NodeJS.ProcessEnv);
  showEncodedImage(path: string, options?: ShowOptions): Promise<void>;
  showRgba(
    width: number,
    height: number,
    rgba: Uint8Array,
    options?: ShowOptions,
  ): Promise<void>;
  clear(): void;
  readonly active: ChildProcess | undefined;
}

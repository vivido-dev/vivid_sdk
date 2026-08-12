import { spawn } from "node:child_process";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { isAbsolute, join } from "node:path";
import { tmpdir } from "node:os";
import { deflateSync } from "node:zlib";

const EXPECTED_PROTOCOL = "1.5";

export function viviPathFromEnv(env = process.env) {
  const executable = env.VVMUX_VIVI_BIN;
  if (!executable || !isAbsolute(executable)) {
    throw new Error("VVMUX_VIVI_BIN must name an absolute Vivi executable");
  }
  if (env.VVMUX_VIVI_PROTOCOL_VERSION !== EXPECTED_PROTOCOL) {
    throw new Error(
      `Vivi protocol mismatch: expected ${EXPECTED_PROTOCOL}, received ${env.VVMUX_VIVI_PROTOCOL_VERSION ?? "unset"}`,
    );
  }
  return executable;
}

export class PaneSession {
  #env;
  #active;

  constructor(env = process.env) {
    viviPathFromEnv(env);
    this.#env = env;
  }

  get active() {
    return this.#active;
  }

  async showEncodedImage(path, options = {}) {
    if (!isAbsolute(path)) {
      throw new Error("encoded image path must be absolute");
    }
    await this.#run(path, options);
  }

  async showRgba(width, height, rgba, options = {}) {
    validateRgba(width, height, rgba);
    const directory = await mkdtemp(join(tmpdir(), "vivid-sdk-"));
    const path = join(directory, "frame.png");
    try {
      await writeFile(path, encodePng(width, height, rgba), { mode: 0o600 });
      await this.#run(path, options);
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  }

  clear() {
    const child = this.#active;
    this.#active = undefined;
    if (child && child.exitCode === null && child.signalCode === null) {
      child.kill("SIGTERM");
    }
  }

  #run(path, options) {
    this.clear();
    const args = [];
    if (options.inline !== false) args.push("--inline");
    if (options.noWait === true) args.push("--no-wait");
    if (options.zoom !== undefined) {
      if (!Number.isFinite(options.zoom) || options.zoom <= 0) {
        return Promise.reject(new Error("zoom must be finite and greater than zero"));
      }
      args.push("--zoom", String(options.zoom));
    }
    args.push(path);
    return new Promise((resolve, reject) => {
      const child = spawn(viviPathFromEnv(this.#env), args, {
        env: this.#env,
        shell: false,
        stdio: ["ignore", "inherit", "inherit"],
        windowsHide: true,
      });
      this.#active = child;
      child.once("error", (error) => {
        if (this.#active === child) this.#active = undefined;
        reject(error);
      });
      child.once("exit", (code, signal) => {
        if (this.#active === child) this.#active = undefined;
        if (code === 0) {
          resolve();
        } else {
          reject(
            new Error(
              signal
                ? `Vivi terminated by ${signal}`
                : `Vivi exited with status ${code ?? "unknown"}`,
            ),
          );
        }
      });
    });
  }
}

function validateRgba(width, height, rgba) {
  if (!Number.isSafeInteger(width) || !Number.isSafeInteger(height) || width <= 0 || height <= 0) {
    throw new Error("RGBA dimensions must be positive safe integers");
  }
  const expected = width * height * 4;
  if (!Number.isSafeInteger(expected) || rgba.byteLength !== expected) {
    throw new Error("RGBA input length does not equal width * height * 4");
  }
}

function encodePng(width, height, rgba) {
  const stride = width * 4;
  const scanlines = Buffer.alloc((stride + 1) * height);
  for (let row = 0; row < height; row += 1) {
    const output = row * (stride + 1);
    scanlines[output] = 0;
    Buffer.from(rgba.buffer, rgba.byteOffset + row * stride, stride).copy(scanlines, output + 1);
  }
  const header = Buffer.alloc(13);
  header.writeUInt32BE(width, 0);
  header.writeUInt32BE(height, 4);
  header.set([8, 6, 0, 0, 0], 8);
  return Buffer.concat([
    Buffer.from("89504e470d0a1a0a", "hex"),
    pngChunk("IHDR", header),
    pngChunk("IDAT", deflateSync(scanlines)),
    pngChunk("IEND", Buffer.alloc(0)),
  ]);
}

function pngChunk(type, data) {
  const name = Buffer.from(type, "ascii");
  const length = Buffer.alloc(4);
  length.writeUInt32BE(data.length);
  const checksum = Buffer.alloc(4);
  checksum.writeUInt32BE(crc32(Buffer.concat([name, data])));
  return Buffer.concat([length, name, data, checksum]);
}

function crc32(data) {
  let crc = 0xffffffff;
  for (const byte of data) {
    crc ^= byte;
    for (let bit = 0; bit < 8; bit += 1) {
      crc = (crc >>> 1) ^ (crc & 1 ? 0xedb88320 : 0);
    }
  }
  return (crc ^ 0xffffffff) >>> 0;
}

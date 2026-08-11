import assert from "node:assert/strict";
import { chmod, mkdtemp, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import { PaneSession, viviPathFromEnv } from "./index.mjs";

test("requires an exact, protocol-compatible helper path", () => {
  assert.throws(() => viviPathFromEnv({}), /VVMUX_VIVI_BIN/);
  assert.throws(
    () => viviPathFromEnv({ VVMUX_VIVI_BIN: "/bin/vivi" }),
    /protocol mismatch/,
  );
  assert.equal(
    viviPathFromEnv({
      VVMUX_VIVI_BIN: "/bin/vivi",
      VVMUX_VIVI_PROTOCOL_VERSION: "1.5",
    }),
    "/bin/vivi",
  );
});

test("uses argv framing and inherited credentials without serializing them", async () => {
  const directory = await mkdtemp(join(tmpdir(), "vivid-node-test-"));
  const helper = join(directory, "vivi");
  const record = join(directory, "record.json");
  const image = join(directory, "image.png");
  await writeFile(
    helper,
    "#!/bin/sh\nprintf '{\"argc\":%s,\"first\":\"%s\",\"secret_present\":%s}' \"$#\" \"$1\" \"${VIVID_ROOT_SECRET:+true}\" > \"$VIVID_TEST_RECORD\"\n",
  );
  await chmod(helper, 0o700);
  await writeFile(image, "image");
  const secret = "do-not-serialize-this-secret";
  const pane = new PaneSession({
    VVMUX_VIVI_BIN: helper,
    VVMUX_VIVI_PROTOCOL_VERSION: "1.5",
    VIVID_ROOT_SECRET: secret,
    VIVID_TEST_RECORD: record,
  });
  await pane.showEncodedImage(image);
  const result = JSON.parse(await readFile(record, "utf8"));
  assert.equal(result.argc, 2);
  assert.equal(result.first, "--inline");
  assert.equal(result.secret_present, true);
  assert.doesNotMatch(await readFile(record, "utf8"), new RegExp(secret));
});

test("encodes a bounded RGBA frame for Vivi", async () => {
  const directory = await mkdtemp(join(tmpdir(), "vivid-node-rgba-"));
  const helper = join(directory, "vivi");
  const record = join(directory, "signature");
  await writeFile(
    helper,
    "#!/bin/sh\nfor value do file=$value; done\nod -An -tx1 -N8 \"$file\" | tr -d ' \\n' > \"$VIVID_TEST_RECORD\"\n",
  );
  await chmod(helper, 0o700);
  const pane = new PaneSession({
    VVMUX_VIVI_BIN: helper,
    VVMUX_VIVI_PROTOCOL_VERSION: "1.5",
    VIVID_TEST_RECORD: record,
  });
  await pane.showRgba(1, 1, new Uint8Array([1, 2, 3, 4]));
  assert.equal(await readFile(record, "utf8"), "89504e470d0a1a0a");
});

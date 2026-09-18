import { strict as assert } from "node:assert";
import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { inspect } from "node:util";

import { PaneSession, connect, lease, presenter } from "../../dist/index.js";

const SOURCE = fileURLToPath(new URL("../../ts/src/", import.meta.url));

test("no source file carries capability material into a log or a URL", () => {
  // The rule the repository states for every product: capability material stays out of logs,
  // command arguments, URLs, and diagnostics.
  const forbidden = [
    /console\.log\([^)]*secret/i,
    /console\.log\([^)]*token/i,
    /URLSearchParams/,
    /document\.cookie/,
    /localStorage/,
  ];
  for (const name of readdirSync(SOURCE)) {
    if (!name.endsWith(".ts")) continue;
    const text = readFileSync(join(SOURCE, name), "utf8");
    for (const pattern of forbidden) {
      assert.ok(!pattern.test(text), `${name} matches ${pattern}`);
    }
  }
});

test("a lease secret survives no reflection path", async () => {
  const session = await connect({ offline: true });
  try {
    const ready = await lease.createSessionLease(session, {
      contextId: 1,
      leaseId: 2,
      permittedProfiles: ["vivid-core-control-v1", "terminal-surface-v1"],
    });
    const secret = ready.activationSecretHex;
    assert.equal(secret.length, 64);
    // Every default way a value escapes a process. `inspect(..., { showHidden: true })` sees it,
    // as does `Object.getOwnPropertyNames`, and both are deliberate asks for hidden state rather
    // than something that happens to a value in passing — the same standing as `vars()` in
    // Python, where the field is likewise `repr=False`.
    assert.ok(!JSON.stringify(ready).includes(secret), "JSON.stringify");
    assert.ok(!inspect(ready).includes(secret), "util.inspect");
    assert.ok(!`${ready}`.includes(secret), "template interpolation");
    assert.ok(!String(ready).includes(secret), "String()");
    assert.ok(!Object.keys(ready).includes("activationSecretHex"), "Object.keys");
    assert.ok(!Object.values(ready).includes(secret), "Object.values");
    assert.ok(!Object.entries(ready).flat().includes(secret), "Object.entries");
    assert.ok(![...Object.getOwnPropertySymbols(ready)].length, "no symbol leaks");
    assert.ok(!{ ...ready }.activationSecretHex, "spread");
  } finally {
    await session.close();
  }
});

test("a pane capability is not retained on the presenter handle", async () => {
  const running = await presenter.Presenter.start({ endpoint: "tcp:127.0.0.1:0" });
  try {
    const capability = running.issuePaneCapability(1);
    assert.equal(capability.length, 64);
    assert.ok(!inspect(running).includes(capability), "util.inspect");
    assert.ok(!JSON.stringify(running).includes(capability), "JSON.stringify");
    assert.ok(!running.endpoint().includes(capability), "endpoint");
  } finally {
    await running.close();
  }
});

test("pane sessions do not retain the endpoint or the secret", async () => {
  const pane = await PaneSession.connect({ offline: true });
  try {
    const debug = inspect(pane);
    assert.ok(!/secret/i.test(debug));
    assert.ok(!/endpoint/i.test(debug));
  } finally {
    await pane.close();
  }
});

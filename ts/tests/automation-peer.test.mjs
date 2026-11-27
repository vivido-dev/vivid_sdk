import { strict as assert } from "node:assert";
import { createHash } from "node:crypto";
import { once } from "node:events";
import { mkdirSync, mkdtempSync, rmSync } from "node:fs";
import { createServer } from "node:net";
import { join } from "node:path";
import { test } from "node:test";

import { automation } from "../../dist/index.js";
import { native } from "../../dist/native.js";

const unix = process.platform === "linux" || process.platform === "darwin";

async function listen(path, accept) {
  const server = createServer(accept);
  const ready = once(server, "listening");
  server.listen(path);
  await ready;
  return server;
}

function close(server) {
  return new Promise((resolve) => server.close(resolve));
}

test("Vivido sends hello only after authenticating the connected Unix peer", { skip: !unix }, async () => {
  const dir = mkdtempSync("/tmp/vivid-peer-");
  const path = join(dir, "control.sock");
  const observed = [];
  const server = await listen(path, (socket) => {
    const chunks = [];
    observed.push(chunks);
    function readHello(chunk) {
      chunks.push(chunk);
      const frame = Buffer.concat(chunks);
      const newline = frame.indexOf(0x0a);
      if (newline < 0) return;
      socket.off("data", readHello);
      const request = JSON.parse(frame.subarray(0, newline).toString("utf8"));
      socket.end(JSON.stringify({ version: 2, id: request.id, ok: true, result: { test: true } }) + "\n");
    }
    socket.on("data", readHello);
  });
  const addon = native();
  const kernelUid = addon.automationPeerUid;
  try {
    try {
      const client = await automation.vividoConnect({ socket: path });
      try {
        assert.equal(client.capabilitiesDocument.test, true);
      } finally {
        client.close();
      }

      // Inject a different UID at the kernel-query boundary. The actual connection,
      // file ownership check, protocol client and server byte counts remain real.
      addon.automationPeerUid = (fd) => kernelUid(fd) + 1;
      await assert.rejects(automation.vividoConnect({ socket: path }), {
        name: "AutomationError", code: "endpoint_unsafe",
      });
      addon.automationPeerUid = undefined;
      await assert.rejects(automation.vividoConnect({ socket: path }), {
        name: "AutomationError", code: "endpoint_unsafe",
      });
    } finally {
      addon.automationPeerUid = kernelUid;
      await close(server);
    }
    assert.equal(observed.length, 3);
    assert.equal(JSON.parse(Buffer.concat(observed[0]).toString()).method, "hello");
    assert.equal(Buffer.concat(observed[1]).length, 0);
    assert.equal(Buffer.concat(observed[2]).length, 0);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("vvmux authenticates the replacement socket before its retry preface", { skip: !unix }, async () => {
  const dir = mkdtempSync("/tmp/vivid-mux-");
  const root = join(dir, "vvmux");
  mkdirSync(root, { mode: 0o700 });
  const target = "peer-test";
  const digest = createHash("sha256").update(target).digest("hex").slice(0, 32);
  const received = [];
  const server = await listen(join(root, `session-${digest}.sock`), (socket) => {
    const chunks = [];
    received.push(chunks);
    socket.on("data", (chunk) => {
      chunks.push(chunk);
      const preface = Buffer.concat(chunks);
      if (preface.length === 12) {
        preface.writeUInt16BE(automation.VVMX_VERSION + 1, 4);
        socket.end(preface);
      }
    });
  });
  const originalDir = process.env.XDG_RUNTIME_DIR;
  const addon = native();
  const kernelUid = addon.automationPeerUid;
  let queries = 0;
  try {
    try {
      process.env.XDG_RUNTIME_DIR = dir;
      addon.automationPeerUid = (fd) => {
        queries += 1;
        return kernelUid(fd) + (queries === 2 ? 1 : 0);
      };
      await assert.rejects(automation.vvmuxConnect(target), {
        name: "AutomationError", code: "endpoint_unsafe",
      });
    } finally {
      addon.automationPeerUid = kernelUid;
      if (originalDir === undefined) delete process.env.XDG_RUNTIME_DIR;
      else process.env.XDG_RUNTIME_DIR = originalDir;
      await close(server);
    }
    assert.equal(queries, 2);
    assert.equal(received.length, 2);
    assert.equal(Buffer.concat(received[0]).length, 12);
    assert.equal(Buffer.concat(received[1]).length, 0);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

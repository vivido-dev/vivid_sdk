// Run every binding's conformance report and compare them.
//
// The reports must be byte-identical. The value of comparing reports rather than comparing each
// language against a written-down expectation is that a drift in *any* of them shows up, and a
// drift in the shared Rust SDK shows up in all three at once — which is what makes the shared
// table meaningful rather than a fourth copy of the same numbers.

import { spawnSync } from "node:child_process";
import { strict as assert } from "node:assert";

const root = new URL("..", import.meta.url).pathname;

/**
 * Canonical JSON: keys sorted at every level.
 *
 * `JSON.stringify(value, arrayOfKeys)` is a property allowlist applied at *every* nesting level,
 * not a key ordering — passing the top-level keys there silently serializes every nested object
 * as `{}`, and two reports then compare equal because both are empty. Sorting by hand is longer
 * and is the only version that compares what it says it compares.
 */
function canonical(value) {
  if (Array.isArray(value)) {
    return `[${value.map(canonical).join(",")}]`;
  }
  if (value !== null && typeof value === "object") {
    const entries = Object.keys(value)
      .sort()
      .map((key) => `${JSON.stringify(key)}:${canonical(value[key])}`);
    return `{${entries.join(",")}}`;
  }
  return JSON.stringify(value);
}

/** Run one producer command and return its parsed report and canonical form. */
function report(label, command, args) {
  const result = spawnSync(command, args, {
    cwd: root,
    encoding: "utf8",
    env: process.env,
    timeout: 120_000,
  });
  if (result.status !== 0) {
    process.stderr.write(`${label} failed (${result.status}):\n${result.stderr}\n`);
    process.exit(1);
  }
  const parsed = JSON.parse(result.stdout.slice(result.stdout.indexOf("{")));
  return { label, parsed, text: canonical(parsed) };
}

/** Prefer the environment's node/uv, falling back to what npm and pip expose. */
const reports = [
  report("rust", "cargo", [
    "run",
    "--quiet",
    "--example",
    "conformance",
    "--features",
    "presenter",
  ]),
  report("python", "uv", ["run", "python", "conformance/scenario.py"]),
  report("typescript", "node", ["conformance/scenario.mjs"]),
];

const [reference, ...others] = reports;
let failed = false;
for (const other of others) {
  try {
    assert.equal(other.text, reference.text);
    console.log(`ok    ${other.label} matches ${reference.label}`);
  } catch {
    failed = true;
    console.error(`FAIL  ${other.label} differs from ${reference.label}`);
    const left = reference.parsed;
    const right = other.parsed;
    for (const section of Object.keys(left)) {
      if (canonical(left[section]) === canonical(right[section])) {
        continue;
      }
      const a = left[section];
      const b = right[section];
      if (typeof a === "object" && a !== null) {
        for (const key of Object.keys(a)) {
          if (canonical(a[key]) !== canonical(b?.[key])) {
            console.error(
              `  ${section}.${key}: ${reference.label}=${JSON.stringify(a[key])} ${other.label}=${JSON.stringify(b?.[key])}`,
            );
          }
        }
      } else {
        console.error(`  ${section}: ${reference.label}=${JSON.stringify(a)} ${other.label}=${JSON.stringify(b)}`);
      }
    }
  }
}

// The report is only meaningful if it actually carried pixels.
assert.ok(reference.parsed.raster.retained, "the reference run retained nothing");
assert.ok(reference.parsed.raster.pixels.length > 0, "the reference run captured no pixels");
assert.ok(Object.keys(reference.parsed.constants).length > 90, "the constant table is too small");
process.exit(failed ? 1 : 0);

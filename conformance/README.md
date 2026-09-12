# Cross-language conformance

Every binding runs the same scenarios and prints the same report. `compare.mjs` runs all three and
asserts the reports are identical.

```sh
# Rust, Python, and TypeScript, in that order
node conformance/compare.mjs
```

## Why comparison rather than expectation

A test that checks each language against a written-down number is three tests that can each be
wrong in the same way, and three places to update when the protocol moves. Comparing reports
configures the check the other way round: the languages have to agree with *each other*, so a
drift in any one of them shows up, and a drift in the shared Rust SDK underneath all three shows
up once.

## What the scenarios cover

- **`constants`** — every name the SDK's table exposes, with its value. This is where a
  hand-copied constant table would have diverged, and it is the check that makes the shared table
  meaningful rather than a fourth copy of the same numbers.
- **`raster`** — one frame presented through a real socket to a real presenter, then read back.
  The report carries the captured pixels rather than a summary of them, so the comparison is over
  bytes.

Both sides of the raster scenario matter: `retained` proves the presenter kept the frame, and
`pixels` proves it kept *those* pixels.

## Adding a scenario

Add it to all three producers — `examples/conformance.rs`, `conformance/scenario.py`, and
`conformance/scenario.mjs` — and to no place else. A scenario that exists in one language and not
the others is not a conformance check; it is a test with extra steps.

## A note on serialization

Reports are compared after canonicalization (keys sorted at every level). Use `JSON.stringify(v)`,
never `JSON.stringify(v, arrayOfKeys)`: the array form is a property *allowlist* applied at every
nesting level, so passing the top-level keys there serializes every nested object as `{}` and two
reports then compare equal because both are empty. That failure mode was real here, and it is why
the negative case below is worth keeping.

## Verifying the check itself

The comparator is only worth having if it fails when it should. To confirm:

```sh
cp conformance/scenario.mjs /tmp/backup.mjs
sed -i '' 's/entry.text ?? entry.number;/entry.name === "SLOT_RASTER" ? 99 : (entry.text ?? entry.number);/' conformance/scenario.mjs
node conformance/compare.mjs   # must exit non-zero and name constants.SLOT_RASTER
cp /tmp/backup.mjs conformance/scenario.mjs
```

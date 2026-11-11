# Private Parley fallback correction

This package is copied from the published `parley` 0.12.0 archive. The original
MIT/Apache licenses, authorship, manifest, lockfile and VCS metadata are retained.
`sources.json` records archive SHA-256 and revision
`6e1143dda817360dd3a2a64f9133c6e5dfabd881` from
[Linebender Parley](https://github.com/linebender/parley/tree/6e1143dda817360dd3a2a64f9133c6e5dfabd881).
The SDK owns this explicit path dependency. Vivido consumes its public
`vivid_sdk::vector_renderer::parley` reexport, as custom shapers can, so embedding
workspaces inherit the correction without private path requirements, root Cargo
patches or duplicate Parley identities.
Producer-only SDK builds continue to omit Parley.

Font selection still prefers the first completely covering font, including
canonical normalization. If every candidate is incomplete, a font covering the
cluster's first contributing character takes priority over one covering only
later marks. Coverage breaks subsequent ties, then configured family order.
This prevents an unsupported combining accent from replacing a supported emoji
base with the primary font's missing-glyph box. Complete matches avoid the
additional base charmap lookup. No font, cluster, text or measurement API changes.

This does not synthesize a grapheme from multiple fonts. A mark missing from the
selected font can still produce a missing-mark glyph; multi-font mark fallback
and physical platform acceptance remain separate work.

The published archive omits its workspace-only `parley_dev` test helper and font
assets. Its existing tests now load the exact Roboto and Noto Kufi Arabic fixtures
from that revision. Their original licenses are beside the files. The same
revision's licensed Noto Color Emoji subsets exercise COLR and CBDT selection
without depending on installed system fonts. `sources.json` pins every fixture.

`vivid-backport.patch` records the source/manifest changes. Reproduce and verify:

```sh
python3 verify_sources.py
cargo test --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
```

The package has an independent workspace for focused verification. Its custom
regressions cover complete normalization precedence, base/coverage/family ranking,
and real color-font clusters whose primary Latin font covers only the marks.

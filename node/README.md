# @vivido/vivid-sdk

Pane-oriented Vivid image helpers for Node and Bun. vvmux supplies an exact,
protocol-compatible `VVMUX_VIVI_BIN` to plugin panes with `media.produce`.
This package executes that argv directly, inherits the pane's Vivid capability
through the environment, and never puts it in argv or error text.

```js
import { PaneSession } from "@vivido/vivid-sdk";

const pane = new PaneSession();
await pane.showEncodedImage("/absolute/path/chart.png");
```

Native streaming bindings remain a future optimization. This package deliberately
reuses the release-matched Vivi producer instead of duplicating Vivid protocol logic.

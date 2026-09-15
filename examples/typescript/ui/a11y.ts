// A panel that describes itself: roles, values, states, actions.
//
// TypeScript twin of `vivid_ui/examples/a11y`. A display list is opaque — it says which
// rectangles are filled, never which one is a switch. So the panel describes itself, and
// assistive technology reads that description rather than guessing from the drawing.
//
// Nothing is inferred: the switch is announced as a switch because it says so. The
// description names the scene revision it belongs to, so it is published only once that
// scene is on screen — which is the rule that stops a screen reader announcing a control
// that has gone. An offline session refuses the publication outright, which is why this one
// is only fully itself in a live pane.
import { overlay } from "@vivido/vivid-sdk";
import { WHITE, rect, runMain, Ui, type Frame } from "./ui.js";

const ITEMS = ["Appearance", "Keyboard", "Network"] as const;
const VOLUME_STEP = 5;

class Panel {
  notifications = true;
  volume = 40;
  selected = 0;
  // How many times the panel has been applied, so a test can see an action arrive.
  applied = 0;

  adjust(by: number): void {
    this.volume = Math.min(Math.max(this.volume + by, 0), 100);
  }
}

async function render(state: Panel, f: Frame): Promise<void> {
  f.box(rect(0, 0, f.width, f.height), { bg: 0x101018ff });
  const nodes: overlay.SemanticNode[] = [];
  const width = f.width - 40;

  await f.label("Settings", rect(20, 20, width, 24), { size: 18, color: 0xe6e6f0ff, vcenter: false });

  // A switch: a state, not a value.
  const toggle = (): void => {
    state.notifications = !state.notifications;
    f.notify();
  };
  let row = rect(20, 54, width, 32);
  f.region("notifications", row, {
    radius: 6,
    cursor: "pointer",
    onClick: toggle,
    onAccessibility: toggle,
  });
  f.box(row, { bg: 0x1a1a24ff, radius: 6 });
  await f.label("Notifications", rect(row.x + 10, row.y, 200, row.height), {
    size: 13,
    color: 0xd0d0e0ff,
  });
  f.box(rect(row.x + row.width - 46, row.y + 7, 36, 18), {
    bg: state.notifications ? 0x4a5fd0ff : 0x33334aff,
    radius: 9,
  });
  nodes.push({
    id: f.ui.regionId("notifications"),
    role: "switch",
    bounds: row,
    label: "Notifications",
    toggled: state.notifications ? "on" : "off",
    actions: ["default", "click"],
  });

  // A spin button: a value in a range, which increments and decrements.
  row = rect(20, 94, width, 32);
  f.region("volume", row, {
    radius: 6,
    onAccessibility: (event) => {
      // The action says which way; the step is the application's to choose.
      if (event.action === "increment") state.adjust(VOLUME_STEP);
      else if (event.action === "decrement") state.adjust(-VOLUME_STEP);
      f.notify();
    },
  });
  f.box(row, { bg: 0x1a1a24ff, radius: 6 });
  await f.label("Volume", rect(row.x + 10, row.y, 200, row.height), { size: 13, color: 0xd0d0e0ff });
  await f.label(String(state.volume), rect(row.x, row.y, row.width - 10, row.height), {
    size: 13,
    color: 0x8ecbffff,
    align: "end",
  });
  nodes.push({
    id: f.ui.regionId("volume"),
    role: "spin-button",
    bounds: row,
    label: "Volume",
    numeric: [state.volume, 0, 100],
    actions: ["increment", "decrement"],
  });

  // A list, whose items say which of how many they are.
  const listTop = 134;
  for (const [index, label] of ITEMS.entries()) {
    const item = rect(20, listTop + index * 30, width, 26);
    const chosen = state.selected === index;
    const choose = (): void => {
      state.selected = index;
      f.notify();
    };
    const element = f.region(`item-${index}`, item, {
      radius: 4,
      cursor: "pointer",
      onClick: choose,
      onAccessibility: choose,
    });
    f.box(item, {
      bg: element.hovered ? 0x2a2a3cff : chosen ? 0x24304aff : 0x181824ff,
      radius: 4,
    });
    await f.label(label, rect(item.x + 8, item.y, item.width - 16, item.height), {
      size: 13,
      color: 0xd0d0e0ff,
    });
    nodes.push({
      id: f.ui.regionId(`item-${index}`),
      role: "list-item",
      bounds: item,
      label,
      // "The second of three", which is what a screen reader reads out.
      set: [index + 1, ITEMS.length],
      actions: ["default", "click"],
    });
  }

  // Drawn by hand rather than through the button helper, because this one answers an
  // accessibility action as well as a click — and one element is one region.
  const applied = (): void => {
    state.applied += 1;
    f.notify();
  };
  const applyBox = rect(20, listTop + ITEMS.length * 30 + 8, width, 32);
  const button = f.region("apply", applyBox, {
    radius: 6,
    cursor: "pointer",
    onClick: applied,
    onAccessibility: applied,
  });
  f.box(applyBox, { bg: button.hovered ? 0x5a6fe0ff : 0x4a5fd0ff, radius: 6 });
  await f.label("Apply", applyBox, { size: 13, color: WHITE, align: "center" });
  nodes.push({
    id: f.ui.regionId("apply"),
    role: "button",
    bounds: applyBox,
    label: "Apply settings",
    actions: ["default", "click"],
  });

  f.publishSemantics(nodes);
}

export function makeUi(): Ui<Panel> {
  return new Ui(new Panel(), render, { width: 360, height: 340 });
}

await runMain(makeUi, import.meta.url);

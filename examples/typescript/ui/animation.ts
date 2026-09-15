// Easings side by side, and a spring that follows clicks.
//
// TypeScript twin of `vivid_ui/examples/animation`. No timer runs here: every moving thing
// is a function of how long the window has been up, and `requestFrameAfter` asks for the
// next frame while there is still one worth drawing — so the loop wakes at the display's
// rate while something is moving and not at all when nothing is.
//
// (The Rust view also honours the environment's reduced-motion preference; the helper has
// no environment plumbing, so this port always animates.)
import { Easing, Keys, Running, Spring, SpringState, rect, runMain, Ui, type EasingKind, type Frame } from "./ui.js";

const TRACK = 300;
const DOT = 14;
const SWEEP = 1.4;

// The easings shown, in the order they are drawn.
const EASINGS: readonly (readonly [string, EasingKind])[] = [
  ["linear", "linear"],
  ["ease in", "ease-in"],
  ["ease out", "ease-out"],
  ["ease in out", "ease-in-out"],
];

class Animated {
  // One animation, read by every row: the easings differ, not the clock.
  readonly sweep = new Running({ duration: SWEEP, repeat: "ping-pong" }, 0);
  // A spring that follows wherever it was last sent.
  readonly spring = new SpringState();
  readonly config = Spring.bouncy();
}

async function render(state: Animated, f: Frame): Promise<void> {
  f.box(rect(0, 0, f.width, f.height), { bg: 0x101018ff });

  // The sweep reads the clock and asks for the next frame in one place, so a moving thing
  // cannot be drawn without being scheduled. It ping-pongs, so it never stops asking.
  const progress = state.sweep.value(f.elapsed);
  f.requestFrameAfter(1 / 60);
  const travel = TRACK - DOT;

  let y = 20;
  for (const [label, easing] of EASINGS) {
    // The same progress, shaped differently: the easings are the comparison.
    await f.label(label, rect(20, y, TRACK, 12), { size: 10, color: 0x8080a0ff, vcenter: false });
    const track = rect(20, y + 14, TRACK, DOT);
    const eased = Easing.apply(easing, progress);
    f.box(track, { bg: 0x1a1a24ff, radius: DOT / 2 });
    f.box(rect(track.x + eased * travel, track.y, DOT, DOT), { bg: 0x8ecbffff, radius: DOT / 2 });
    y += 12 + 2 + DOT + 10;
  }

  await f.label("click the track to send the spring there", rect(20, y, TRACK, 12), {
    size: 10,
    color: 0x8080a0ff,
    vcenter: false,
  });
  y += 22;

  // The spring is advanced to now and asks for frames until it settles.
  state.spring.advance(state.config, f.dt);
  if (!state.spring.atRest()) f.requestFrameAfter(1 / 60);
  const springTrack = rect(20, y, TRACK, DOT + 6);
  f.box(springTrack, { bg: 0x1a1a24ff, radius: (DOT + 6) / 2 });
  f.box(rect(springTrack.x + state.spring.value * travel, springTrack.y + 3, DOT, DOT), {
    bg: 0x70d0a0ff,
    radius: DOT / 2,
  });
  f.region("spring-track", springTrack, {
    radius: (DOT + 6) / 2,
    cursor: "pointer",
    onClick: (event) => {
      // Where the click landed as a fraction of the track — read off the frame before this
      // one, which is the frame the host hit-tested against.
      const track = f.boundsOf("spring-track");
      const within = track ? (event.position.x - track.x) / Math.max(track.width, 1) : 0;
      // Retargeting mid-flight keeps whatever speed it had, which is the whole reason this
      // is a spring and not a duration.
      state.spring.retarget(Math.min(Math.max(within, 0), 1));
      f.notify();
    },
  });
}

export function makeUi(): Ui<Animated> {
  const state = new Animated();
  const ui = new Ui(state, render, { width: 360, height: 320 });

  ui.onKey = (event) => {
    if (!(event.down && !event.repeat && event.physical === Keys.space)) return;
    state.sweep.restart(ui.elapsed());
    ui.notify();
  };

  return ui;
}

await runMain(makeUi, import.meta.url);

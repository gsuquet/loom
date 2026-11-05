export interface BenchResult {
  frames: number;
  idleMedianMs: number;
  deltaP50: number;
  deltaP95: number;
  deltaP99: number;
  deltaMax: number;
  under16_7: number;
  under17_7: number;
  droppedOverIdle: number;
  workP50: number;
  workP95: number;
  workMax: number;
  pxPerFrame: number;
  visibility: string;
}

const pct = (sorted: number[], p: number) =>
  sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * p))] ?? 0;
const round = (n: number) => Math.round(n * 100) / 100;

/** Work per scroll update, filled by the diff pane while a bench runs. */
export const workSamples: { on: boolean; values: number[] } = { on: false, values: [] };

function rafDeltas(frames: number, each?: () => void): Promise<number[]> {
  return new Promise((resolve) => {
    const deltas: number[] = [];
    let last = 0;
    const tick = (t: number) => {
      if (last) deltas.push(t - last);
      last = t;
      each?.();
      if (deltas.length >= frames) resolve(deltas);
      else requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
  });
}

/**
 * Scrolls by script and times requestAnimationFrame inside the page. It needs no permission.
 * It moves `scrollTop` on the main thread: it does not drive WebKit's scrolling thread, as a
 * trackpad does. The display sets the floor: at 60 Hz every delta is about 16.67 ms, so the
 * figures with a margin and the work per frame matter as much as the raw one.
 */
export async function scrollBench(
  el: HTMLElement,
  frames: number,
  pxPerFrame: number,
): Promise<BenchResult> {
  const idle = (await rafDeltas(60)).sort((a, b) => a - b);
  const idleMedian = pct(idle, 0.5);
  workSamples.values = [];
  workSamples.on = true;
  const deltas = await rafDeltas(frames, () => {
    const max = el.scrollHeight - el.clientHeight;
    el.scrollTop = el.scrollTop + pxPerFrame > max ? 0 : el.scrollTop + pxPerFrame;
  });
  workSamples.on = false;
  const usable = deltas.slice(5);
  const sorted = [...usable].sort((a, b) => a - b);
  const work = [...workSamples.values].sort((a, b) => a - b);
  const share = (limit: number) =>
    round((usable.filter((d) => d < limit).length / usable.length) * 100);
  return {
    frames: usable.length,
    idleMedianMs: round(idleMedian),
    deltaP50: round(pct(sorted, 0.5)),
    deltaP95: round(pct(sorted, 0.95)),
    deltaP99: round(pct(sorted, 0.99)),
    deltaMax: round(sorted.at(-1) ?? 0),
    under16_7: share(16.7),
    under17_7: share(17.7),
    droppedOverIdle: usable.filter((d) => d > idleMedian * 1.5).length,
    workP50: round(pct(work, 0.5)),
    workP95: round(pct(work, 0.95)),
    workMax: round(work.at(-1) ?? 0),
    pxPerFrame,
    visibility: document.visibilityState,
  };
}

/**
 * Render and layout cost per scroll step, with no paint and no requestAnimationFrame. It also runs
 * in a hidden page. It is a lower bound of the cost of a frame, not a frame time.
 */
export async function workBench(el: HTMLElement, steps: number, px: number) {
  workSamples.values = [];
  workSamples.on = true;
  for (let i = 0; i < steps; i += 1) {
    const max = el.scrollHeight - el.clientHeight;
    el.scrollTop = el.scrollTop + px > max ? 0 : el.scrollTop + px;
    el.dispatchEvent(new Event("scroll"));
    if (i % 16 === 0) await new Promise((r) => setTimeout(r, 0));
  }
  workSamples.on = false;
  const work = [...workSamples.values].sort((a, b) => a - b);
  return {
    steps: work.length,
    px,
    workP50: round(pct(work, 0.5)),
    workP95: round(pct(work, 0.95)),
    workP99: round(pct(work, 0.99)),
    workMax: round(work.at(-1) ?? 0),
    visibility: document.visibilityState,
  };
}

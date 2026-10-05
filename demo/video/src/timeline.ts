// Every shot is a real 2560x1600 frame of Tern 0.4.5 (1280x800 CSS px at 2x) running the
// mantern binary headless; see demo/scenarios.py for what was typed and clicked.

export const FPS = 30;

export type Seg = { dir: "c" | "m"; name: string; frames: number };

const seg = (dir: "c" | "m", name: string, frames: number): Seg => ({ dir, name, frames });
const pad = (n: number) => String(n).padStart(2, "0");
const run = (dir: "c" | "m", prefix: string, from: number, to: number, frames: number): Seg[] =>
  Array.from({ length: to - from + 1 }, (_, i) => seg(dir, `${prefix}${pad(from + i)}`, frames));

export const SCROLL_SHOTS = 90;

export type Scene = {
  id: string;
  /** Which step of the left-hand list is lit. */
  step: number;
  segs: Seg[];
  /** Pointer path in CSS px of the 1280x800 window: [frame, x, y, click?]. */
  pointer?: [number, number, number, boolean?][];
};

const sum = (segs: Seg[]) => segs.reduce((n, s) => n + s.frames, 0);

export const INTRO = 45;
export const OUTRO = 120;

export const scenes: Scene[] = [
  {
    id: "classic",
    step: 0,
    segs: [seg("c", "c01", 18), seg("c", "c03", 20), seg("c", "c04", 22)],
  },
  {
    id: "typing",
    step: 1,
    segs: [
      seg("m", "m00", 8),
      ...run("m", "t", 1, 11, 2),
      seg("m", "t11", 10),
      seg("m", "top", 32),
    ],
  },
  { id: "scroll", step: 2, segs: run("m", "s", 1, SCROLL_SHOTS, 1) },
  {
    id: "fold",
    step: 3,
    segs: [seg("m", "end", 22), seg("m", "see0", 4), seg("m", "see1", 24)],
    pointer: [
      [0, 980, 230],
      [20, 120, 658, true],
      [50, 130, 658],
    ],
  },
  {
    id: "chip",
    step: 4,
    segs: [seg("m", "see1", 8), seg("m", "go0", 4), seg("m", "go1", 8), seg("m", "go2", 54)],
    pointer: [
      [0, 130, 658],
      [7, 198, 571],
      [8, 198, 571, true],
      [74, 640, 300],
    ],
  },
];

export const sceneFrames = (s: Scene) => sum(s.segs);
export const STEPS = [
  { title: "Today", body: "man tar — a wall of monospace" },
  { title: "Same command", body: "mantern takes exactly man's arguments" },
  { title: "Real typography", body: "Title block, synopsis card, option cards, accent flags" },
  { title: "Folds", body: "Long pages open on what matters; sections click open" },
  { title: "Links", body: "SEE ALSO chips open the next page right below" },
];
export const TOTAL = INTRO + scenes.reduce((n, s) => n + sceneFrames(s), 0) + OUTRO;

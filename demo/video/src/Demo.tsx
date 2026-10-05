import {
  AbsoluteFill,
  Audio,
  Easing,
  OffthreadVideo,
  Sequence,
  interpolate,
  spring,
  staticFile,
  useCurrentFrame,
  useVideoConfig,
} from "remotion";
import { loadFont as loadInter } from "@remotion/google-fonts/Inter";
import { loadFont as loadMono } from "@remotion/google-fonts/JetBrainsMono";
import { FPS, INTRO, OUTRO, STEPS, Scene, sceneFrames, scenes } from "./timeline";

const ACCENT = "#2bb8ff";
const INK = "#e8ecf4";
const MUTED = "#8a93a6";
const SANS = loadInter("normal", { weights: ["400", "600", "700"], subsets: ["latin"] }).fontFamily;
const MONO = loadMono("normal", { weights: ["700", "400"], subsets: ["latin"] }).fontFamily;

// The window: Tern's own 1280x800 shot, scaled.
const WIN_W = 1360;
const WIN_H = (WIN_W * 800) / 1280;
const WIN_X = 500;
const WIN_Y = (1080 - WIN_H) / 2;
const K = WIN_W / 1280;

const ease = Easing.bezier(0.22, 1, 0.36, 1);

const Backdrop = () => (
  <AbsoluteFill
    style={{
      background:
        "radial-gradient(1200px 700px at 78% 18%, rgba(43,184,255,0.16), transparent 60%), radial-gradient(900px 600px at 8% 92%, rgba(139,92,246,0.14), transparent 60%), linear-gradient(160deg, #0a0e16, #0e1420 60%, #0a0d14)",
    }}
  />
);

const Wordmark = ({ size }: { size: number }) => (
  <div style={{ fontFamily: MONO, fontSize: size, fontWeight: 700, color: ACCENT, letterSpacing: -2 }}>
    mantern
  </div>
);

const Intro = () => {
  const f = useCurrentFrame();
  const { fps } = useVideoConfig();
  const pop = spring({ frame: f, fps, config: { damping: 200 } });
  const out = interpolate(f, [INTRO - 14, INTRO], [1, 0], { extrapolateLeft: "clamp" });
  return (
    <AbsoluteFill style={{ alignItems: "center", justifyContent: "center", opacity: out }}>
      <div style={{ transform: `translateY(${(1 - pop) * 24}px)`, opacity: pop, textAlign: "center" }}>
        <Wordmark size={150} />
        <div style={{ fontFamily: SANS, fontSize: 38, color: INK, marginTop: 6 }}>
          <span style={{ color: MUTED }}>man pages, </span>rendered natively in Tern
        </div>
      </div>
    </AbsoluteFill>
  );
};

const Steps = ({ active }: { active: number }) => {
  const f = useCurrentFrame();
  const enter = interpolate(f, [0, 18], [0, 1], { extrapolateRight: "clamp", easing: ease });
  return (
    <div
      style={{
        position: "absolute",
        left: 64,
        top: 0,
        bottom: 0,
        width: 400,
        display: "flex",
        flexDirection: "column",
        justifyContent: "center",
        gap: 30,
        opacity: enter,
        transform: `translateX(${(1 - enter) * -30}px)`,
      }}
    >
      <div style={{ marginBottom: 14 }}>
        <Wordmark size={64} />
      </div>
      {STEPS.map((s, i) => {
        const on = i === active;
        const done = i < active;
        return (
          <div key={s.title} style={{ display: "flex", gap: 16, opacity: on ? 1 : done ? 0.55 : 0.3, transition: "none" }}>
            <div
              style={{
                width: 4,
                borderRadius: 2,
                background: on ? ACCENT : "rgba(255,255,255,0.18)",
              }}
            />
            <div>
              <div style={{ fontFamily: SANS, fontSize: 30, fontWeight: 650, color: on ? INK : MUTED }}>{s.title}</div>
              <div style={{ fontFamily: SANS, fontSize: 21, color: MUTED, marginTop: 4, lineHeight: 1.35 }}>{s.body}</div>
            </div>
          </div>
        );
      })}
    </div>
  );
};

const Pointer = ({ path }: { path: NonNullable<Scene["pointer"]> }) => {
  const f = useCurrentFrame();
  const frames = path.map((p) => p[0]);
  const x = interpolate(f, frames, path.map((p) => p[1]), { easing: ease, extrapolateLeft: "clamp", extrapolateRight: "clamp" });
  const y = interpolate(f, frames, path.map((p) => p[2]), { easing: ease, extrapolateLeft: "clamp", extrapolateRight: "clamp" });
  const click = path.find((p) => p[3] && f >= p[0] && f < p[0] + 16);
  const t = click ? (f - click[0]) / 16 : 0;
  return (
    <>
      {click && (
        <div
          style={{
            position: "absolute",
            left: WIN_X + click[1] * K - 26,
            top: WIN_Y + click[2] * K - 26,
            width: 52,
            height: 52,
            borderRadius: 26,
            border: `3px solid ${ACCENT}`,
            opacity: 1 - t,
            transform: `scale(${0.4 + t * 1.1})`,
          }}
        />
      )}
      <svg
        width="30"
        height="30"
        viewBox="0 0 24 24"
        style={{
          position: "absolute",
          left: WIN_X + x * K - 4,
          top: WIN_Y + y * K - 3,
          transform: `scale(${click && t < 0.25 ? 0.88 : 1})`,
          filter: "drop-shadow(0 3px 5px rgba(0,0,0,0.5))",
        }}
      >
        <path d="M5 2 L5 19 L9.4 14.8 L12.3 21.5 L14.9 20.3 L12 13.7 L18 13.7 Z" fill="#fff" stroke="#10151f" strokeWidth="1.4" strokeLinejoin="round" />
      </svg>
    </>
  );
};

const SceneView = ({ scene, footageFrom }: { scene: Scene; footageFrom: number }) => (
  <AbsoluteFill>
    <Steps active={scene.step} />
    <div
      style={{
        position: "absolute",
        left: WIN_X,
        top: WIN_Y,
        width: WIN_W,
        height: WIN_H,
        borderRadius: 16,
        overflow: "hidden",
        boxShadow: "0 40px 90px rgba(0,0,0,0.55), 0 0 0 1px rgba(255,255,255,0.08)",
      }}
    >
      <OffthreadVideo muted src={staticFile("footage.mp4")} startFrom={footageFrom} style={{ width: WIN_W, height: WIN_H }} />
    </div>
    {scene.pointer && <Pointer path={scene.pointer} />}
  </AbsoluteFill>
);

const Outro = () => {
  const f = useCurrentFrame();
  const a = interpolate(f, [0, 16], [0, 1], { extrapolateRight: "clamp", easing: ease });
  const out = interpolate(f, [OUTRO - 18, OUTRO], [1, 0], { extrapolateLeft: "clamp" });
  const chips = ["Same arguments as man", "Falls back to your system man", "No plugin — just the Surface Protocol"];
  return (
    <AbsoluteFill style={{ alignItems: "center", justifyContent: "center", background: "rgba(8,11,17,0.82)", opacity: a * out }}>
      <div style={{ textAlign: "center", transform: `translateY(${(1 - a) * 20}px)` }}>
        <Wordmark size={140} />
        <div style={{ fontFamily: SANS, fontSize: 38, color: INK, marginTop: 8 }}>A better face for man pages.</div>
        <div style={{ display: "flex", gap: 16, marginTop: 44, justifyContent: "center" }}>
          {chips.map((c) => (
            <div
              key={c}
              style={{
                fontFamily: SANS,
                fontSize: 24,
                color: ACCENT,
                background: "rgba(43,184,255,0.12)",
                border: "1px solid rgba(43,184,255,0.35)",
                borderRadius: 999,
                padding: "10px 22px",
              }}
            >
              {c}
            </div>
          ))}
        </div>
        <div style={{ fontFamily: MONO, fontSize: 26, color: MUTED, marginTop: 48 }}>
          <span style={{ color: "#8b5cf6" }}>❯</span> mantern ls
        </div>
      </div>
    </AbsoluteFill>
  );
};

export const Demo = () => {
  let at = INTRO;
  let footage = 0;
  const offsets = scenes.map((s) => {
    const placed = { from: at, footageFrom: footage };
    at += sceneFrames(s);
    footage += sceneFrames(s);
    return placed;
  });
  return (
    <AbsoluteFill style={{ background: "#0a0e16" }}>
      <Backdrop />
      <Audio src={staticFile("music.wav")} volume={0.9} />
      <Sequence from={0} durationInFrames={INTRO}>
        <Intro />
      </Sequence>
      {scenes.map((s, i) => (
        <Sequence key={s.id} from={offsets[i].from} durationInFrames={sceneFrames(s)}>
          <SceneView scene={s} footageFrom={offsets[i].footageFrom} />
        </Sequence>
      ))}
      <Sequence from={at} durationInFrames={OUTRO}>
        <Outro />
      </Sequence>
    </AbsoluteFill>
  );
};

export { FPS };

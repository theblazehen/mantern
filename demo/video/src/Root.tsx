import { Composition } from "remotion";
import { Demo } from "./Demo";
import { FPS, TOTAL } from "./timeline";

export const Root = () => (
  <Composition id="Demo" component={Demo} durationInFrames={TOTAL} fps={FPS} width={1920} height={1080} />
);

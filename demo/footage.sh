#!/usr/bin/env bash
# Crabbox worker: cut every scene's shots into ONE video (public/footage.mp4), one input
# image per output frame so the length matches the timeline exactly. Remotion plays that
# video; swapping 100+ big JPEGs per frame left half-painted frames.
set -euo pipefail
V=/workspace/mtvideo
cd $V
rm -rf frames && mkdir frames
bun -e '
import { scenes } from "./src/timeline.ts";
let n = 0, out = "";
for (const s of scenes) for (const g of s.segs) for (let i = 0; i < g.frames; i++) {
  out += `ln -s ../public/shots/${g.dir}/${g.name}.jpg frames/${String(++n).padStart(5, "0")}.jpg\n`;
}
await Bun.write("frames.sh", out);
'
sh frames.sh
ffmpeg -loglevel error -y -framerate 30 -i frames/%05d.jpg -vf "scale=1360:850:flags=lanczos,format=yuv420p" \
  -c:v libx264 -crf 12 -preset medium -r 30 public/footage.mp4
ffprobe -v error -count_frames -show_entries stream=nb_read_frames -of csv=p=0 public/footage.mp4

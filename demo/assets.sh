#!/usr/bin/env bash
# Crabbox worker: stage the video project at /workspace/mtvideo with the shots as 1600x1000
# JPEGs and the music, so syncing the repo never prunes them.
set -euo pipefail
V=/workspace/mtvideo
mkdir -p $V/public/shots/m $V/public/shots/c
cp -r demo/video/src demo/video/package.json demo/video/tsconfig.json demo/video/remotion.config.ts $V/
for pair in "main m" "typing m" "classic c"; do
  set -- $pair
  ls /workspace/shots/out/$1/*-dark.png | xargs -P 12 -I{} sh -c \
    'ffmpeg -loglevel error -y -i "$1" -vf scale=1600:1000 -q:v 2 "'$V'/public/shots/'$2'/$(basename "$1" -dark.png).jpg"' _ {}
done
# numpy on this worker needs a libstdc++ from the Nix store.
LD_LIBRARY_PATH=/nix/store/0iv8glcslgfcgn371lbjr5jjw5a6cqir-gcc-15.3.0-lib/lib:/nix/store/gqlhr6gyj9py3ibr6qmdk0yv14bpdywz-crabbox-native-development-libraries/lib uv run --quiet --with numpy demo/music.py $V/public/music.wav 17
ls $V/public/shots/m | wc -l
(cd $V && [ -d node_modules ] || npm install --no-audit --no-fund 2>&1 | tail -3)

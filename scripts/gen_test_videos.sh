#!/usr/bin/env bash
# Generate small clips used by integration tests and local experiments.
set -euo pipefail

OUT_DIR="${1:-target/test-videos}"
mkdir -p "$OUT_DIR"

# Constant 30 fps, short GOP, 4 seconds (~120 frames).
ffmpeg -y -hide_banner -loglevel error \
  -f lavfi -i "testsrc=duration=4:size=160x120:rate=30" \
  -c:v libx264 -pix_fmt yuv420p -g 30 -keyint_min 30 -preset ultrafast \
  "$OUT_DIR/cfr.mp4"

# Variable presentation timing (do not assume 1/FPS).
ffmpeg -y -hide_banner -loglevel error \
  -f lavfi -i "testsrc=duration=4:size=160x120:rate=30" \
  -vf "setpts='if(eq(N,0),0,PREV_OUTPTS+N*0.01/TB)'" \
  -c:v libx264 -pix_fmt yuv420p -g 30 -preset ultrafast \
  "$OUT_DIR/vfr.mp4"

# Long GOP so backward stepping must decode from a distant keyframe on cache miss.
ffmpeg -y -hide_banner -loglevel error \
  -f lavfi -i "testsrc=duration=5:size=160x120:rate=30" \
  -c:v libx264 -pix_fmt yuv420p -g 250 -keyint_min 250 -preset ultrafast \
  "$OUT_DIR/long_gop.mp4"

echo "Wrote clips in $OUT_DIR"

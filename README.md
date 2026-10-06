# egg

Open-source media player built with Rust and GStreamer. Video opens in a GTK
window with an on-screen control bar. The same keys work when that window is
focused and when the terminal is focused.

egg’s standout feature: **frame-by-frame navigation in both directions**,
including a real backward step (seek to a previous keyframe, decode forward,
stop on the previous displayed PTS). Backward stepping is **not**
`current_pts - 1/fps`.

## System dependencies

Targeted at Ubuntu 22.04 and GStreamer 1.20 (`v1_20` features in `Cargo.toml`).
GStreamer 1.24 (Ubuntu 24.04) needs those features changed to `v1_24`.

Runtime:

- `gstreamer1.0-tools`
- `gstreamer1.0-plugins-base` / `good` / `bad` / `ugly`
- `gstreamer1.0-libav`
- `gstreamer1.0-x` (X11 video sinks)
- `gstreamer1.0-gtk3` and GTK 3 for the video window and control bar

Build:

- Rust stable (`rustc` 1.80+)
- `libgstreamer1.0-dev`
- `libgstreamer-plugins-base1.0-dev`
- `pkg-config`

```bash
sudo apt install \
  build-essential pkg-config \
  libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
  gstreamer1.0-tools gstreamer1.0-plugins-base \
  gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
  gstreamer1.0-plugins-ugly gstreamer1.0-libav \
  gstreamer1.0-x gstreamer1.0-gtk3 libgtk-3-0

curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
```

## Install

From a clone:

```bash
git clone https://github.com/haseenmatharyousuffansari/egg.git
cd egg
cargo install --path .
```

Then:

```bash
egg video.mp4
egg --info video.mp4
egg --start 00:05:20 --volume 70 --speed 0.5 video.mp4
egg --no-audio --loop video.mp4
egg --fullscreen video.mp4
RUST_LOG=info egg video.mp4
```

Or build without installing:

```bash
cargo build
./target/debug/egg video.mp4
cargo build --release
./target/release/egg video.mp4
```

`--help` and `--version` are supported. Quit with the **Quit** button, `q`, or
`Esc`.

## On-screen controls

| Button | Action |
| --- | --- |
| `Back 10` | Backward 10 frames (pauses) |
| `Back` | Backward one frame (pauses) |
| `Play` / `Pause` | Play or pause |
| `Fwd` | Forward one frame (pauses) |
| `Fwd 10` | Forward 10 frames (pauses) |
| `Vol-` / `Vol+` | Volume ±5 |
| `Spd-` / `Spd+` | Speed ±0.25× (0.25–4.0) |
| `1x` | Reset speed to 1.0× |
| `Loop` | Toggle loop |
| `Info` | Toggle media information in the terminal |
| `Quit` | Quit |

The bar shows play state, position, duration, speed, volume, and loop / info
flags. Closing the window quits.

## Keyboard controls

| Key | Action |
| --- | --- |
| `Space` | Play / pause |
| `Right` | Forward one frame (pauses) |
| `Left` | Backward one frame (pauses) |
| `]` or `Shift+Right` | Forward 10 frames |
| `[` or `Shift+Left` | Backward 10 frames |
| `Up` / `Down` | Volume ±5 |
| `+` / `-` | Speed ±0.25× (0.25–4.0) |
| `0` | Reset speed to 1.0× |
| `l` | Toggle loop |
| `i` | Toggle media information overlay |
| `q` / `Esc` | Quit |

Keys work with the video window focused or the terminal focused. Many terminals
do not report `Shift+Left` / `Shift+Right` reliably — use `[` and `]`.

Video is drawn by `xvimagesink` inside the GTK window (X11). Headless tests use
`fakesink`.

## DualStep / `.hm`

For long-GOP files, egg can write a DualStep companion (`.hm`) next to the
source so backward steps past the in-memory cache stay responsive without
re-decoding from distant keyframes. Writing `.hm` can take time and disk for
large sources; prefer short clips while developing. Generated `.hm` files are
gitignored — do not commit them.

## Architecture

```text
src/
├── main.rs                 CLI entry, raw-mode event loop
├── cli.rs                  clap + time parsing
├── error.rs                thiserror results
├── player/
│   ├── player.rs           playbin controller
│   ├── state.rs            PlayerState machine
│   ├── commands.rs         keyboard/CLI commands
│   ├── seeking.rs          Normal / Accurate / KeyUnitBefore
│   └── frame_stepper.rs    PTS identity, cache, probe actions
├── media/                  Discoverer --info, DualStep
├── input/                  crossterm keymap (shared with the window)
└── ui/
    ├── status.rs           terminal status line
    └── window.rs           GTK window, control bar, window keys
```

Playback uses `playbin` with a custom video-sink bin. A pad probe records each
presentation-timestamped video buffer. The player never treats
`query_position()` or `1/FPS` as the displayed frame.

## Frame stepping (summary)

**PTS is the displayed-frame identity.** Forward uses `GST_EVENT_STEP` (or a
short unsynced play burst). Backward on cache miss seeks to the previous
keyframe, decodes forward, and stops on the previous displayed PTS. A ring of
recent frames caches PTS targets for nearby LEFT/RIGHT steps.

## Tests

```bash
cargo test
cargo test --test frame_step
```

Integration tests generate tiny H.264 clips with `scripts/gen_test_videos.sh`
(requires `ffmpeg`) and drive a headless `fakesink` player.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for build steps, PR expectations, and
issue etiquette. Please follow the [Code of Conduct](CODE_OF_CONDUCT.md).

## Known limitations

- Backward stepping on a **cache miss** (without DualStep) decodes from the
  previous keyframe; long GOP files can take noticeable time.
- Linux / X11 is the supported target.
- Reverse playback (holding a negative rate) is not implemented.
- Network streams, playlists, subtitles, HDR, and hardware-decode selection are
  reserved for later.

## License

MIT — see [LICENSE](LICENSE).

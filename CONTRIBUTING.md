# Contributing to egg

Thanks for helping improve egg. This document covers building on Ubuntu 22.04,
running tests, and opening pull requests.

## Code of Conduct

By participating, you agree to follow our [Code of Conduct](CODE_OF_CONDUCT.md).

## Development environment (Ubuntu 22.04)

egg targets GStreamer 1.20 (the `v1_20` features in `Cargo.toml`). On Ubuntu
24.04 / GStreamer 1.24, change those features to `v1_24`.

```bash
sudo apt install \
  build-essential pkg-config \
  libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
  gstreamer1.0-tools gstreamer1.0-plugins-base \
  gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
  gstreamer1.0-plugins-ugly gstreamer1.0-libav \
  gstreamer1.0-x gstreamer1.0-gtk3 libgtk-3-0 \
  ffmpeg

curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
```

Rust stable 1.80+ is expected.

## Build and install locally

```bash
git clone https://github.com/haseenmatharyousuffansari/egg.git
cd egg
cargo build
cargo install --path .
egg --help
```

## Tests

Headless frame-step integration tests (no GTK window) live in
`tests/frame_step.rs`. They generate tiny H.264 clips via
`scripts/gen_test_videos.sh` (needs `ffmpeg`).

```bash
cargo test --test frame_step
cargo test
```

Please run the frame-step suite before opening a PR that touches seeking,
DualStep / `.hm`, or the player probe path.

## Pull requests

1. Open an issue first for larger design changes when practical.
2. Keep PRs focused: one concern per PR when you can.
3. Match existing style and module layout (`src/player`, `src/media`, `src/ui`).
4. Update README or comments when behavior user-facing behavior changes.
5. Describe *what* changed and *why* in the PR body; note how you tested.

## Issues

- Use the bug / feature templates when they fit.
- For bugs: include OS, GStreamer version (`gst-launch-1.0 --version`), egg
  version (`egg --version`), a short reproduction, and whether DualStep / `.hm`
  was involved.
- Search existing issues before filing a duplicate.

## DualStep / `.hm` notes

Writing a DualStep companion (`.hm`) can take a long time and a lot of disk for
large sources. Prefer short sample clips when debugging. Do not commit `.hm`,
`*.hm.partial`, or generated test video binaries.

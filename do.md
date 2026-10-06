Build a Rust CLI media player called `rmpv` (Rust Media Player) inspired by mpv.

## Goal

Create a terminal/CLI media player capable of playing virtually any media format supported by the underlying media framework, with one major feature that mpv does not provide conveniently:

**Frame-by-frame navigation in BOTH directions.**

The player must support:

* Play
* Pause
* Forward one frame
* Backward one frame
* Forward N frames
* Backward N frames
* Accurate seeking
* Normal seeking
* Playback speed control
* Volume control
* Fullscreen if practical
* Media information
* Keyboard controls
* CLI arguments
* Multiple common video formats/codecs

The first priority is reliable frame stepping, especially backward frame stepping.

---

# Technology

Use:

* Rust
* GStreamer
* GStreamer Rust bindings
* Cargo
* Tokio only if actually necessary; do not introduce async complexity unnecessarily.

Do NOT implement video codecs yourself.

Use GStreamer for:

* Container demuxing
* Codec decoding
* Audio decoding
* Video decoding
* Seeking
* Timestamp handling
* Hardware acceleration where available

The Rust application should be the player/controller layer.

---

# Important technical requirement

Do NOT implement frame stepping as:

```text
current_timestamp - frame_duration
```

because this is not reliable for compressed video, variable frame rate video, B-frames, and keyframe-based seeking.

Backward frame stepping must work by using an appropriate seek/decode strategy.

The implementation should conceptually support:

```text
Current displayed frame
        │
        ▼
Determine previous frame target
        │
        ▼
Seek to a safe earlier timestamp/keyframe
        │
        ▼
Decode forward
        │
        ▼
Stop exactly when previous frame is reached
        │
        ▼
Display that frame
```

Similarly:

```text
Current displayed frame
        │
        ▼
Determine next frame
        │
        ▼
Decode until next frame
        │
        ▼
Pause/display
```

Do not assume constant FPS.

Use actual buffer timestamps / duration information whenever possible.

---

# CLI

The application should work like:

```bash
rmpv video.mp4
```

Also support:

```bash
rmpv --help
rmpv --version
rmpv --info video.mp4
```

Potential options:

```text
--start <TIME>
--volume <0-100>
--speed <RATE>
--no-audio
--fullscreen
--loop
```

Keep CLI parsing simple using `clap`.

---

# Keyboard controls

Implement intuitive controls.

```text
SPACE       Play / Pause

RIGHT       Forward one frame
LEFT        Backward one frame

SHIFT+RIGHT Forward 10 frames
SHIFT+LEFT  Backward 10 frames

UP          Increase volume
DOWN        Decrease volume

+           Increase playback speed
-           Decrease playback speed

0           Reset playback speed

q           Quit
ESC         Quit / exit fullscreen

i           Show media information

l           Toggle loop
```

If terminal/input limitations prevent SHIFT+LEFT or SHIFT+RIGHT from being detected reliably, provide alternative commands such as:

```text
]   forward 10 frames
[   backward 10 frames
```

Document the actual supported controls.

---

# Architecture

Do NOT put everything in `main.rs`.

Use a modular architecture similar to:

```text
src/
├── main.rs
├── cli.rs
├── player/
│   ├── mod.rs
│   ├── player.rs
│   ├── state.rs
│   ├── commands.rs
│   ├── seeking.rs
│   └── frame_stepper.rs
├── media/
│   ├── mod.rs
│   ├── info.rs
│   └── streams.rs
├── input/
│   ├── mod.rs
│   └── keyboard.rs
└── ui/
    ├── mod.rs
    └── status.rs
```

Keep responsibilities separated.

---

# Player State

Create an explicit player state machine.

For example:

```rust
enum PlayerState {
    Loading,
    Playing,
    Paused,
    Seeking,
    FrameStepping,
    Ended,
    Error,
}
```

Avoid scattering state across unrelated variables.

---

# Frame Stepper

Create a dedicated component:

```rust
struct FrameStepper {
    // appropriate state
}
```

with operations conceptually like:

```rust
fn next_frame(...)
fn previous_frame(...)
fn step_forward(...)
fn step_backward(...)
```

The frame stepper must understand that video can be:

* Constant FPS
* Variable FPS
* B-frame based
* Long GOP
* Different keyframe intervals

Do not calculate frame timestamps using:

```text
1 / FPS
```

unless there is no better information available.

---

# Frame stepping behavior

When paused:

```text
LEFT
```

should display the immediately preceding decoded/displayed frame.

Example:

```text
Frame: 100
LEFT
Frame: 99
LEFT
Frame: 98
RIGHT
Frame: 99
```

When playing:

```text
RIGHT
```

should pause playback and move to the next frame.

Similarly:

```text
LEFT
```

should pause playback and move to the previous frame.

After frame stepping, the player should remain paused.

---

# Frame stepping algorithm

Design this carefully.

Maintain information about the currently displayed frame:

```text
current_frame_position
current_frame_pts
current_frame_duration
```

For forward stepping:

1. Pause.
2. Obtain the current frame timestamp.
3. Advance/decode until the next video buffer.
4. Display that buffer.
5. Remain paused.

For backward stepping:

1. Pause.
2. Determine the timestamp of the desired previous frame.
3. Seek backwards to a safe position before the target.
4. Flush the pipeline appropriately.
5. Decode forward.
6. Capture decoded frames.
7. Stop when the target frame is reached.
8. Display it.
9. Remain paused.

Do not rely on a simple negative seek operation alone.

Investigate GStreamer's:

```text
GST_SEEK_FLAG_FLUSH
GST_SEEK_FLAG_ACCURATE
GST_SEEK_FLAG_KEY_UNIT
GST_FORMAT_TIME
```

and choose the appropriate combination.

The implementation should favor correctness over performance initially.

---

# Important distinction

Do not confuse:

```text
presentation timestamp
decode timestamp
frame number
byte position
keyframe position
```

Treat PTS as the primary timing reference for displayed video frames.

Document the assumptions in the code.

---

# GStreamer pipeline

Start with an automatic pipeline where possible.

Conceptually:

```text
filesrc
   ↓
decodebin
   ├── video → videosink
   └── audio → audiosink
```

Use appropriate GStreamer elements rather than manually supporting every codec.

For the initial implementation, prefer:

```text
playbin
```

if it makes reliable frame stepping possible.

If `playbin` prevents the control needed for accurate frame stepping, implement the pipeline manually using:

```text
filesrc
decodebin
queue
videoconvert
videosink
```

and corresponding audio elements.

Make the pipeline architecture replaceable.

---

# Video rendering

This is a CLI application, but it still needs a video window.

Use a GStreamer video sink appropriate for the platform.

Do not attempt to render video frames as terminal ASCII art.

The terminal is only for:

```text
controls
status
metadata
commands
```

The actual video should be rendered in a native window.

---

# Terminal UI

Show a lightweight status line such as:

```text
▶  00:12:35.420 / 01:32:10.000
Frame: 18563
FPS: 29.97
Speed: 1.00x
Volume: 80%
State: PAUSED
```

When stepping:

```text
◀ FRAME
PTS: 00:12:35.386
Frame: 18562
```

Keep the UI minimal.

Do not build a full graphical UI.

---

# Media information

Implement:

```bash
rmpv --info video.mp4
```

Output something similar to:

```text
File:
  video.mp4

Container:
  Matroska

Duration:
  00:12:35.420

Video:
  Codec: H.264
  Resolution: 1920x1080
  FPS: 29.97
  Pixel Format: ...

Audio:
  Codec: AAC
  Sample Rate: 48000 Hz
  Channels: 2
```

Use GStreamer discovery APIs.

---

# Error handling

Do not use:

```rust
unwrap()
```

throughout the application.

Use proper Rust error handling.

Prefer:

```text
Result<T, Error>
```

with a meaningful error type.

Errors should explain things such as:

```text
Could not open file
Unsupported media
Could not initialize GStreamer
Could not create video sink
Seek failed
Frame stepping failed
```

---

# Logging

Use `tracing`.

Support:

```bash
RUST_LOG=info rmpv video.mp4
```

and:

```bash
RUST_LOG=debug rmpv video.mp4
```

Debug logging should be particularly useful for frame stepping.

For example:

```text
current PTS
target PTS
seek PTS
keyframe PTS
decoded PTS
displayed PTS
```

---

# Testing

Create tests for the frame-stepping logic.

At minimum test:

```text
forward 1 frame
backward 1 frame
forward 10 frames
backward 10 frames
forward then backward
backward then forward
beginning of video
end of video
variable FPS
long GOP
```

Use generated/test videos where necessary.

Important invariant:

```text
step_forward()
step_backward()
```

should return to approximately the same displayed frame, subject to documented timestamp precision.

Also test:

```text
100 → backward → 99
99 → forward → 100
```

Do not assume frame number can always be inferred from FPS.

---

# Performance

Version 1 should prioritize correctness.

Do not prematurely optimize backward frame stepping.

After correctness is established, investigate:

* Decoder reuse
* Keyframe caching
* Frame caching
* Reverse playback optimization
* Hardware decoding
* Zero-copy buffers
* GPU rendering

A useful future optimization would be caching recently decoded frames:

```text
Frame cache

98
99
100 ← current
101
102
```

Then:

```text
LEFT
```

can immediately show frame 99 without seeking.

---

# Future architecture

Design the code so these can later be added without rewriting the player:

```text
Reverse playback
Frame cache
A/B looping
Screenshot
Frame export
Exact timestamp seeking
Subtitle selection
Audio track selection
Video track selection
Playlist
Network streams
RTSP
HLS
DASH
Hardware decoding
HDR
10-bit video
```

Potential future commands:

```text
rmpv video.mp4 --start 00:05:20
rmpv video.mp4 --speed 0.25
rmpv video.mp4 --no-audio
```

---

# Development approach

Do NOT generate the entire complicated application at once.

Implement it incrementally.

## Phase 1

Create a minimal project that:

```text
cargo run -- video.mp4
```

and can:

* Open the file
* Play video
* Play audio
* Pause
* Quit

## Phase 2

Add:

* Keyboard input
* Seeking
* Status information

## Phase 3

Implement:

* Forward one frame
* Backward one frame

This is the most important phase.

## Phase 4

Add:

* N-frame stepping
* Playback speed
* Volume
* Media information

## Phase 5

Improve:

* Error handling
* Logging
* Tests
* Architecture
* Documentation

## Phase 6

Optimize:

* Frame cache
* Hardware decoding
* Reverse stepping performance

After each phase, ensure the project compiles and works before moving to the next phase.

---

# Critical requirement for Cursor

Before writing implementation code, inspect the currently installed versions of:

```text
rustc
cargo
gstreamer
gstreamer-video
gstreamer-audio
gstreamer-app
```

and determine the compatible Rust GStreamer crate versions.

Do not blindly use outdated examples from the internet.

Use current APIs.

If an API is uncertain, verify it against the installed GStreamer version and current Rust GStreamer documentation.

---

# Deliverables

Start by creating:

```text
Cargo.toml
src/main.rs
src/cli.rs
src/player/
src/media/
src/input/
src/ui/
README.md
```

README must explain:

1. Dependencies
2. Linux installation
3. Build instructions
4. Run instructions
5. Keyboard controls
6. Architecture
7. Frame stepping implementation
8. Known limitations

Target Linux first.

Make the project compile and run before adding advanced functionality.

Most importantly:

**Do not fake backward frame stepping with timestamp subtraction. Implement an actual decode/seek strategy capable of finding the previous displayed video frame.**

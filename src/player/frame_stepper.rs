//! Frame stepping built on **presentation timestamps**, not FPS arithmetic.
//!
//! Assumptions documented here and relied on by the rest of the player:
//!
//! - **PTS is the identity of a displayed frame.** Two buffers are the same
//!   displayed frame if their PTS values match (within a small tolerance used
//!   only for tests / equality checks).
//! - **DTS, byte offset, and keyframe byte position are not display identity.**
//!   They may be used only as seek helpers (for example KEY_UNIT snap).
//! - **Frame numbers** are a sequential count of *displayed* video buffers when
//!   the player has observed them in order. They are never computed as
//!   `pts * fps`.
//! - Video may be CFR, VFR, B-frame / reordered, or long-GOP. Previous/next
//!   frames are found by comparing actual buffer PTS values collected after a
//!   keyframe seek + decode-forward, never by `current_pts - 1/fps`.
//!
//! Backward stepping conceptually:
//!   current displayed PTS
//!     → seek to a safe earlier keyframe (FLUSH | KEY_UNIT | SNAP_BEFORE)
//!     → decode forward
//!     → stop at the last frame whose PTS is strictly less than current
//!     → display that frame and stay paused

use std::collections::VecDeque;

use gstreamer::{Buffer, Caps, ClockTime};

/// A displayed (or just-decoded) video frame identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameInfo {
    pub pts: ClockTime,
    pub duration: Option<ClockTime>,
    /// Sequential displayed-buffer count when known. `None` after a random seek.
    pub index: Option<u64>,
}

/// Decoded frame stored so LEFT/RIGHT inside the window can skip seeking.
#[derive(Debug, Clone)]
pub struct CachedFrame {
    pub pts: ClockTime,
    pub duration: Option<ClockTime>,
    pub buffer: Buffer,
    pub caps: Option<Caps>,
}

impl CachedFrame {
    pub fn info(&self) -> FrameInfo {
        FrameInfo {
            pts: self.pts,
            duration: self.duration,
            index: None,
        }
    }
}

const DEFAULT_CACHE_CAP: usize = 16;

/// Ring of recently decoded frames, keyed by PTS.
#[derive(Debug)]
pub struct FrameCache {
    frames: VecDeque<CachedFrame>,
    cap: usize,
}

impl Default for FrameCache {
    fn default() -> Self {
        Self::new(DEFAULT_CACHE_CAP)
    }
}

impl FrameCache {
    pub fn new(cap: usize) -> Self {
        Self {
            frames: VecDeque::new(),
            cap,
        }
    }

    pub fn insert(&mut self, frame: CachedFrame) {
        if let Some(existing) = self.frames.iter_mut().find(|f| f.pts == frame.pts) {
            *existing = frame;
            return;
        }
        self.frames.push_back(frame);
        while self.frames.len() > self.cap {
            self.frames.pop_front();
        }
    }

    pub fn previous(&self, current: ClockTime) -> Option<&CachedFrame> {
        self.frames
            .iter()
            .filter(|f| f.pts < current)
            .max_by_key(|f| f.pts)
    }

    pub fn next(&self, current: ClockTime) -> Option<&CachedFrame> {
        self.frames
            .iter()
            .filter(|f| f.pts > current)
            .min_by_key(|f| f.pts)
    }

    pub fn get(&self, pts: ClockTime) -> Option<&CachedFrame> {
        self.frames.iter().find(|f| f.pts == pts)
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    pub fn clear(&mut self) {
        self.frames.clear();
    }

    pub fn pts_list(&self) -> Vec<u64> {
        self.frames.iter().map(|f| f.pts.nseconds()).collect()
    }
}

/// What the pad probe should do with the next decoded video buffers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeAction {
    /// Normal playback: record PTS / cache, do not drop.
    Idle,
    /// After a flush seek: the next buffer is the preroll frame.
    WaitPreroll,
    /// Forward step: accept the next buffer whose PTS differs from `after`.
    WaitAfter { after: ClockTime },
    /// Backward decode: pass frames with PTS < `before`, drop the first
    /// buffer at or past `before`, and signal completion.
    CollectBefore { before: ClockTime },
    /// A step/seek already chose the displayed frame. Ignore further buffers
    /// until the player thread goes back to `Idle` (avoids in-flight overshoot).
    Hold,
}

/// Events emitted by the streaming-thread probe to the player thread.
#[derive(Debug, Clone, Copy)]
pub enum ProbeEvent {
    Frame(FrameInfo),
    Preroll(FrameInfo),
    ReachedTarget { last: Option<FrameInfo> },
}

/// Shared probe / cache state. The pad probe runs on a streaming thread.
pub struct SharedFrameState {
    pub current: Option<FrameInfo>,
    pub first_pts: Option<ClockTime>,
    pub caps: Option<Caps>,
    pub cache: FrameCache,
    pub action: ProbeAction,
    pub showing_cache: bool,
    /// Next buffer on the video thread is replaced with this frame.
    pub replay: Option<CachedFrame>,
    /// After a backward redisplay, drop later frames so they do not paint
    /// over the previous picture.
    pub freeze_picture: bool,
    /// Set by the player thread just before it pauses a step. The probe
    /// holds the stream lock until this is set, so decode cannot run to EOS.
    pub step_release: bool,
    pub event_tx: std::sync::mpsc::Sender<ProbeEvent>,
}

impl SharedFrameState {
    pub fn new(event_tx: std::sync::mpsc::Sender<ProbeEvent>) -> Self {
        Self {
            current: None,
            first_pts: None,
            caps: None,
            cache: FrameCache::default(),
            action: ProbeAction::Idle,
            showing_cache: false,
            replay: None,
            freeze_picture: false,
            step_release: false,
            event_tx,
        }
    }

    pub fn update_current(&mut self, mut frame: FrameInfo) {
        if self.first_pts.is_none() {
            self.first_pts = Some(frame.pts);
        }
        if let Some(prev) = self.current {
            if frame.pts > prev.pts {
                frame.index = prev.index.map(|i| i.saturating_add(1));
            } else if frame.pts == prev.pts {
                frame.index = prev.index;
            } else {
                // Backward display: decrement when we still have a sequential index.
                frame.index = prev.index.map(|i| i.saturating_sub(1));
            }
        } else {
            frame.index = Some(0);
        }
        self.current = Some(frame);
    }

    pub fn apply_index_delta(&mut self, delta: i64) {
        if let Some(cur) = self.current.as_mut() {
            if let Some(i) = cur.index {
                cur.index = Some(i.saturating_add_signed(delta));
            }
        }
    }

    pub fn forget_index(&mut self) {
        if let Some(cur) = self.current.as_mut() {
            cur.index = None;
        }
    }
}

/// Previous displayed PTS from a decode window of actual buffer timestamps.
///
/// `collected` is nanoseconds. Values greater than or equal to `current_pts`
/// are ignored. This is the only legal way to find "the previous frame".
pub fn previous_pts_from_window(collected: &[u64], current_pts: u64) -> Option<u64> {
    collected.iter().copied().filter(|&p| p < current_pts).max()
}

/// Next displayed PTS from a decode window of actual buffer timestamps.
pub fn next_pts_from_window(collected: &[u64], current_pts: u64) -> Option<u64> {
    collected.iter().copied().filter(|&p| p > current_pts).min()
}

/// True when two PTS values name the same displayed frame.
///
/// Tolerance is 1 ms — used only for tests and "are we still on this frame"
/// checks, never to *invent* a previous-frame timestamp.
pub fn pts_close_ns(a: u64, b: u64) -> bool {
    a.abs_diff(b) <= 1_000_000
}

pub fn pts_close(a: ClockTime, b: ClockTime) -> bool {
    pts_close_ns(a.nseconds(), b.nseconds())
}

/// 1 nanosecond earlier than `pts`, used only as a KEY_UNIT snap nudge when
/// the current frame *is* a keyframe. This is not a frame-duration guess.
pub fn nudge_before(pts: ClockTime) -> ClockTime {
    ClockTime::from_nseconds(pts.nseconds().saturating_sub(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfr(fps_num: u64, fps_den: u64, frames: u64) -> Vec<u64> {
        // PTS from the container/encoder, not something we should ever invert
        // via 1/fps in the player — tests still *generate* a CFR sequence.
        (0..frames)
            .map(|i| i * 1_000_000_000 * fps_den / fps_num)
            .collect()
    }

    #[test]
    fn forward_one_frame() {
        let frames = cfr(30, 1, 10);
        assert_eq!(next_pts_from_window(&frames, frames[3]), Some(frames[4]));
    }

    #[test]
    fn backward_one_frame() {
        let frames = cfr(30, 1, 10);
        assert_eq!(
            previous_pts_from_window(&frames, frames[3]),
            Some(frames[2])
        );
    }

    #[test]
    fn forward_ten_frames() {
        let frames = cfr(30, 1, 40);
        let mut pts = frames[5];
        for _ in 0..10 {
            pts = next_pts_from_window(&frames, pts).unwrap();
        }
        assert_eq!(pts, frames[15]);
    }

    #[test]
    fn backward_ten_frames() {
        let frames = cfr(30, 1, 40);
        let mut pts = frames[20];
        for _ in 0..10 {
            pts = previous_pts_from_window(&frames, pts).unwrap();
        }
        assert_eq!(pts, frames[10]);
    }

    #[test]
    fn forward_then_backward_same_frame() {
        let frames = cfr(30, 1, 200);
        let start = frames[100];
        let next = next_pts_from_window(&frames, start).unwrap();
        let back = previous_pts_from_window(&frames, next).unwrap();
        assert!(pts_close_ns(back, start), "{back} vs {start}");
    }

    #[test]
    fn backward_then_forward_same_frame() {
        let frames = cfr(30, 1, 200);
        let start = frames[100];
        let prev = previous_pts_from_window(&frames, start).unwrap();
        let fwd = next_pts_from_window(&frames, prev).unwrap();
        assert!(pts_close_ns(fwd, start), "{fwd} vs {start}");
        // 100 → backward → 99 → forward → 100
        assert_eq!(prev, frames[99]);
        assert_eq!(fwd, frames[100]);
    }

    #[test]
    fn beginning_of_video() {
        let frames = cfr(24, 1, 5);
        assert_eq!(previous_pts_from_window(&frames, frames[0]), None);
    }

    #[test]
    fn end_of_video() {
        let frames = cfr(24, 1, 5);
        assert_eq!(next_pts_from_window(&frames, *frames.last().unwrap()), None);
    }

    #[test]
    fn variable_fps_window() {
        // Irregular PTS: 0, 10ms, 80ms, 90ms, 200ms — must not use 1/FPS.
        let frames = vec![0, 10_000_000, 80_000_000, 90_000_000, 200_000_000];
        assert_eq!(
            previous_pts_from_window(&frames, 90_000_000),
            Some(80_000_000)
        );
        assert_eq!(next_pts_from_window(&frames, 10_000_000), Some(80_000_000));
    }

    #[test]
    fn long_gop_previous_is_not_keyframe_guess() {
        // 250-frame GOP: previous of frame 250 is 249, not "the last keyframe".
        let frames = cfr(30, 1, 300);
        assert_eq!(
            previous_pts_from_window(&frames, frames[250]),
            Some(frames[249])
        );
        assert_ne!(
            previous_pts_from_window(&frames, frames[250]),
            Some(frames[0])
        );
    }

    #[test]
    fn does_not_invent_pts_via_subtraction() {
        let frames = vec![0, 40_000_000, 80_000_000];
        let current: u64 = 80_000_000;
        let guessed = current.saturating_sub(33_366_666); // 1/29.97 style guess
        let actual = previous_pts_from_window(&frames, current).unwrap();
        assert_eq!(actual, 40_000_000);
        assert_ne!(actual, guessed);
    }
}

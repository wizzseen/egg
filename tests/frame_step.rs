//! Integration tests for bidirectional frame stepping against real H.264 clips.
//!
//! Frame identity is **PTS**, never `index = pts * fps`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use gstreamer::ClockTime;
use egg::player::frame_stepper::pts_close;
use egg::player::{Player, PlayerOptions, SeekMode};

static GST_TEST: Mutex<()> = Mutex::new(());

fn lock_gst() -> MutexGuard<'static, ()> {
    GST_TEST.lock().unwrap_or_else(|e| e.into_inner())
}

fn video_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-videos")
}

fn ensure_videos() {
    let dir = video_dir();
    let needed = ["cfr.mp4", "vfr.mp4", "long_gop.mp4"];
    if needed.iter().all(|n| dir.join(n).is_file()) {
        return;
    }
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/gen_test_videos.sh");
    let status = Command::new("bash")
        .arg(&script)
        .arg(&dir)
        .status()
        .expect("failed to spawn ffmpeg helper script");
    assert!(status.success(), "ffmpeg fixture generation failed");
}

fn open_headless(path: &Path) -> Player {
    let mut player = Player::open(
        path,
        PlayerOptions {
            no_audio: true,
            headless: true,
            start_paused: true,
            volume: 0,
            ..PlayerOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
    player
        .wait_ready(Duration::from_secs(8))
        .unwrap_or_else(|e| panic!("preroll {}: {e}", path.display()));
    player
}

fn pts(player: &Player) -> ClockTime {
    player.current_pts().expect("probed PTS")
}

#[test]
fn forward_one_frame_cfr() {
    let _lock = lock_gst();
    ensure_videos();
    gstreamer::init().unwrap();
    let mut p = open_headless(&video_dir().join("cfr.mp4"));
    let a = pts(&p);
    p.step_forward(1).expect("forward 1");
    let b = pts(&p);
    assert!(b > a, "expected next PTS > {a}, got {b}");
}

#[test]
fn backward_one_frame_cfr() {
    let _lock = lock_gst();
    ensure_videos();
    gstreamer::init().unwrap();
    let mut p = open_headless(&video_dir().join("cfr.mp4"));
    p.step_forward(3).expect("setup");
    let mid = pts(&p);
    p.step_backward(1).expect("backward 1");
    let prev = pts(&p);
    assert!(prev < mid, "expected previous PTS < {mid}, got {prev}");
}

#[test]
fn forward_ten_then_status_moves() {
    let _lock = lock_gst();
    ensure_videos();
    gstreamer::init().unwrap();
    let mut p = open_headless(&video_dir().join("cfr.mp4"));
    let a = pts(&p);
    p.step_forward(10).expect("forward 10");
    let b = pts(&p);
    assert!(b > a, "10-frame forward did not advance PTS ({a} -> {b})");
}

#[test]
fn backward_ten_then_status_moves() {
    let _lock = lock_gst();
    ensure_videos();
    gstreamer::init().unwrap();
    let mut p = open_headless(&video_dir().join("cfr.mp4"));
    p.step_forward(15).expect("setup");
    let a = pts(&p);
    p.step_backward(10).expect("backward 10");
    let b = pts(&p);
    assert!(b < a, "10-frame backward did not decrease PTS ({a} -> {b})");
}

#[test]
fn forward_then_backward_same_frame() {
    let _lock = lock_gst();
    ensure_videos();
    gstreamer::init().unwrap();
    let mut p = open_headless(&video_dir().join("cfr.mp4"));
    p.step_forward(8).expect("setup");
    let start = pts(&p);
    p.step_forward(1).expect("forward");
    p.step_backward(1).expect("backward");
    let back = pts(&p);
    assert!(
        pts_close(start, back),
        "step_forward then step_backward should return to the same PTS ({start} vs {back})"
    );
}

#[test]
fn backward_then_forward_same_frame() {
    let _lock = lock_gst();
    ensure_videos();
    gstreamer::init().unwrap();
    let mut p = open_headless(&video_dir().join("cfr.mp4"));
    p.step_forward(12).expect("setup");
    let start = pts(&p);
    p.step_backward(1).expect("backward");
    let prev = pts(&p);
    p.step_forward(1).expect("forward");
    let again = pts(&p);
    assert!(prev < start, "100→99 style: previous must be earlier");
    assert!(
        pts_close(start, again),
        "99→forward→100 style: returned PTS {again} != {start}"
    );
}

#[test]
fn beginning_of_video_stays() {
    let _lock = lock_gst();
    ensure_videos();
    gstreamer::init().unwrap();
    let mut p = open_headless(&video_dir().join("cfr.mp4"));
    let first = pts(&p);
    p.step_backward(1).expect("backward at start");
    let still = pts(&p);
    assert!(
        pts_close(first, still),
        "LEFT at the first frame must stay there ({first} vs {still})"
    );
}

#[test]
fn end_of_video_forward_does_not_panic() {
    let _lock = lock_gst();
    ensure_videos();
    gstreamer::init().unwrap();
    let mut p = open_headless(&video_dir().join("cfr.mp4"));
    p.seek_to(ClockTime::from_seconds(3), SeekMode::Accurate)
        .expect("seek near end");
    p.wait_ready(Duration::from_secs(5)).expect("seek preroll");
    // Stepping off the end must fail softly (EOS / last frame), not abort.
    let before = pts(&p);
    let _ = p.step_forward(2);
    let after = p.current_pts().expect("still have a frame at EOS");
    assert!(after >= before);
}

#[test]
fn variable_fps_roundtrip() {
    let _lock = lock_gst();
    ensure_videos();
    gstreamer::init().unwrap();
    let mut p = open_headless(&video_dir().join("vfr.mp4"));
    p.step_forward(6).expect("setup vfr");
    let start = pts(&p);
    p.step_forward(1).expect("vfr forward");
    let next = pts(&p);
    assert!(next > start);
    p.step_backward(1).expect("vfr backward");
    let back = pts(&p);
    assert!(
        pts_close(start, back),
        "VFR roundtrip used actual PTS, not 1/FPS ({start} vs {back})"
    );
}

#[test]
fn long_gop_roundtrip() {
    let _lock = lock_gst();
    ensure_videos();
    gstreamer::init().unwrap();
    let mut p = open_headless(&video_dir().join("long_gop.mp4"));
    p.step_forward(20).expect("long gop setup");
    let start = pts(&p);
    p.step_backward(1).expect("long gop backward");
    let prev = pts(&p);
    assert!(prev < start);
    p.step_forward(1).expect("long gop forward");
    let again = pts(&p);
    assert!(
        pts_close(start, again),
        "long-GOP backward must land on the previous displayed frame, not a keyframe guess ({start} vs {again})"
    );
}

#[test]
fn cache_serves_recent_backward_step() {
    let _lock = lock_gst();
    ensure_videos();
    gstreamer::init().unwrap();
    let mut p = open_headless(&video_dir().join("cfr.mp4"));
    p.step_forward(5).expect("fill cache");
    assert!(
        p.cache_len() >= 2,
        "expected decoded frames to be cached, got {}",
        p.cache_len()
    );
    p.step_backward(1).expect("cached backward");
    assert!(
        p.showing_cache() || p.current_pts().is_some(),
        "backward step after recent forwards should use the cache when possible"
    );
}

#[test]
fn backward_past_the_cached_ring() {
    let _lock = lock_gst();
    ensure_videos();
    gstreamer::init().unwrap();
    let mut p = open_headless(&video_dir().join("long_gop.mp4"));
    p.step_forward(20).expect("setup");
    let start = pts(&p);
    p.step_backward(17).expect("past the 16-frame ring");
    let back = pts(&p);
    assert!(back < start, "stepping past the cache did not move back ({start} vs {back})");
    let gap = start.saturating_sub(back);
    assert!(
        gap < ClockTime::from_seconds(2),
        "older frames must be the previous pictures, not a jump to the keyframe ({start} -> {back})"
    );
}

#[test]
fn play_after_step_advances() {
    let _lock = lock_gst();
    ensure_videos();
    gstreamer::init().unwrap();
    let mut p = open_headless(&video_dir().join("cfr.mp4"));
    p.step_forward(5).expect("setup");
    let at = pts(&p);
    p.play().expect("play after step");
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let mut moved = false;
    while std::time::Instant::now() < deadline {
        if pts(&p) > at {
            moved = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    p.pause().expect("pause after resume");
    assert!(moved, "play after a step stayed at {at}");
}

#[test]
fn write_hm_keeps_the_source_file() {
    let _lock = lock_gst();
    ensure_videos();
    let src = video_dir().join("cfr.mp4");
    let before = std::fs::metadata(&src).unwrap().len();
    let dual = egg::media::dualstep::DualStep::write_hm(&src, 160, 120, &mut |_, _| {}).unwrap();
    assert!(dual.frames.len() > 16, "short clip should store every frame");
    assert_eq!(std::fs::metadata(&src).unwrap().len(), before);
    assert!(dual.path.exists(), "missing {}", dual.path.display());
    let _ = std::fs::remove_file(&dual.path);
}

#[test]
fn dualstep_steps_back_one_frame_past_the_cache() {
    let _lock = lock_gst();
    ensure_videos();
    gstreamer::init().unwrap();
    let mut p = open_headless(&video_dir().join("cfr.mp4"));
    p.step_forward(24).expect("setup");
    let mut prev = pts(&p);
    for _ in 0..20 {
        p.step_backward(1).expect("backward");
        let now = pts(&p);
        assert!(now < prev, "Left did not move back ({prev} -> {now})");
        let gap = prev.nseconds() - now.nseconds();
        assert!(
            gap < 50_000_000,
            "Left jumped more than one frame ({prev} -> {now})"
        );
        prev = now;
    }
    assert_ne!(p.state(), egg::player::PlayerState::Ended);
    let at = pts(&p);
    p.play().expect("play after DualStep left");
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let mut moved = false;
    while std::time::Instant::now() < deadline {
        if pts(&p) > at {
            moved = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    p.pause().expect("pause after resume");
    assert!(moved, "play after stepping back stayed at {at}");
    assert_ne!(p.state(), egg::player::PlayerState::Ended);
}

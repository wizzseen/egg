use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use gstreamer::prelude::*;
use gstreamer::{
    ClockTime, Element, ElementFactory, GhostPad, Message, MessageView, PadProbeReturn,
    PadProbeType, State,
};
use gstreamer_video::prelude::*;

use crate::error::{Error, Result};
use crate::media::dualstep::{BackwardDecoder, DualStep};
use crate::media::{glib_filename_to_uri, MediaInfo};
use crate::player::commands::Command;
use crate::player::frame_stepper::{
    nudge_before, pts_close, CachedFrame, FrameInfo, ProbeAction, ProbeEvent, SharedFrameState,
};
use crate::player::seeking::SeekMode;
use crate::player::state::PlayerState;
use crate::ui::status::StatusSnapshot;
use crate::ui::window::VideoWindow;

enum HmUpdate {
    Progress { done: u32, total: u32 },
    Done(std::result::Result<DualStep, String>),
}

const VOLUME_STEP: u8 = 5;
const SPEED_STEP: f64 = 0.25;
const SPEED_MIN: f64 = 0.25;
const SPEED_MAX: f64 = 4.0;

#[derive(Debug, Clone)]
pub struct PlayerOptions {
    pub start: Option<ClockTime>,
    pub volume: u8,
    pub speed: f64,
    pub no_audio: bool,
    pub fullscreen: bool,
    pub looping: bool,
    /// Use `fakesink` so tests can run without a window / display.
    pub headless: bool,
    /// Stay paused after preroll (used by tests).
    pub start_paused: bool,
}

impl Default for PlayerOptions {
    fn default() -> Self {
        Self {
            start: None,
            volume: 80,
            speed: 1.0,
            no_audio: false,
            fullscreen: false,
            looping: false,
            headless: false,
            start_paused: false,
        }
    }
}

/// playbin-based player. The concrete pipeline is replaceable later
/// (`filesrc ! decodebin ! …`) if playbin ever blocks frame-accurate control.
pub struct Player {
    playbin: Element,
    bus: gstreamer::Bus,
    video_bin: gstreamer::Bin,
    display_sink: Element,
    present_pad: gstreamer::Pad,
    /// Set when the picture was stepped off the decoder's position. Play seeks
    /// back once; the step itself does not.
    stepped_pts: Option<ClockTime>,
    window: Option<VideoWindow>,
    shared: Arc<Mutex<SharedFrameState>>,
    event_rx: mpsc::Receiver<ProbeEvent>,
    state: PlayerState,
    want_playing: bool,
    speed: f64,
    volume: u8,
    looping: bool,
    duration: Option<ClockTime>,
    media: MediaInfo,
    path: PathBuf,
    dualstep: Option<DualStep>,
    backward: BackwardDecoder,
    show_info: bool,
    dirty: bool,
    ended: bool,
    hm_job: Option<mpsc::Receiver<HmUpdate>>,
}

impl Player {
    pub fn open(path: impl AsRef<Path>, options: PlayerOptions) -> Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            return Err(Error::OpenFile(format!(
                "{} does not exist",
                path.display()
            )));
        }

        gstreamer::init().map_err(|e| Error::GStreamerInit(e.to_string()))?;
        prefer_software_decode();

        let abs = path
            .canonicalize()
            .map_err(|e| Error::OpenFile(e.to_string()))?;
        let (playback, _opened_hm) = if abs.extension().and_then(|ext| ext.to_str()) == Some("hm") {
            (DualStep::source_for_hm(&abs)?, true)
        } else {
            (abs, false)
        };

        let media = MediaInfo::discover(&playback)?;
        let uri = glib_filename_to_uri(&playback)?;

        let playbin = ElementFactory::make("playbin")
            .name("egg")
            .property("uri", &uri)
            .build()
            .map_err(|e| Error::PlayerInit(format!("playbin: {e}")))?;

        let volume = options.volume.min(100);
        playbin.set_property("volume", volume as f64 / 100.0);

        if options.no_audio {
            let fake = ElementFactory::make("fakesink")
                .property("sync", false)
                .build()
                .map_err(|e| Error::PlayerInit(format!("audio fakesink: {e}")))?;
            playbin.set_property("audio-sink", &fake);
        }

        let (event_tx, event_rx) = mpsc::channel();
        let shared = Arc::new(Mutex::new(SharedFrameState::new(event_tx)));

        let title = path.file_name().and_then(|n| n.to_str()).unwrap_or("egg");
        let built = build_video_sink_bin(options.headless, Arc::clone(&shared), title)?;
        let video = media.video.clone();
        let dualstep = if let Some(video) = video {
            Some(DualStep::prepare(
                &playback,
                video.width,
                video.height,
                video.fps.unwrap_or(10.0),
            )?)
        } else {
            None
        };
        playbin.set_property("video-sink", &built.bin);

        let bus = playbin
            .bus()
            .ok_or_else(|| Error::PlayerInit("playbin has no bus".into()))?;

        playbin
            .set_state(State::Paused)
            .map_err(|e| Error::PlayerInit(format!("could not preroll: {e}")))?;

        wait_until_paused(&playbin, built.window.as_ref())?;

        let duration = playbin.query_duration::<ClockTime>();

        let mut player = Self {
            playbin,
            bus,
            video_bin: built.bin,
            display_sink: built.display_sink,
            present_pad: built.present_pad,
            stepped_pts: None,
            window: built.window,
            shared,
            event_rx,
            state: PlayerState::Paused,
            want_playing: false,
            speed: options.speed.clamp(SPEED_MIN, SPEED_MAX),
            volume,
            looping: options.looping,
            duration,
            media,
            path: playback,
            dualstep,
            backward: BackwardDecoder::new(),
            show_info: false,
            dirty: true,
            ended: false,
            hm_job: None,
        };

        if options.fullscreen {
            if let Some(window) = &player.window {
                window.fullscreen();
            }
        }

        if let Some(start) = options.start {
            player.seek_to(start, SeekMode::Accurate)?;
        }

        if (player.speed - 1.0).abs() > f64::EPSILON {
            if let Some(pos) = player.current_pts() {
                player.apply_rate(pos)?;
            }
        }

        if !options.start_paused {
            player.play()?;
        }
        Ok(player)
    }

    pub fn media_info(&self) -> &MediaInfo {
        &self.media
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn state(&self) -> PlayerState {
        self.state
    }

    pub fn current_pts(&self) -> Option<ClockTime> {
        self.lock_shared().current.map(|f| f.pts)
    }

    pub fn current_frame(&self) -> Option<FrameInfo> {
        self.lock_shared().current
    }

    pub fn cache_len(&self) -> usize {
        self.lock_shared().cache.len()
    }

    pub fn showing_cache(&self) -> bool {
        self.lock_shared().showing_cache
    }

    /// Block until the probe has observed a displayed frame, or timeout.
    pub fn wait_ready(&mut self, timeout: Duration) -> Result<FrameInfo> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(frame) = self.current_frame() {
                return Ok(frame);
            }
            if Instant::now() >= deadline {
                return Err(Error::PlayerInit(
                    "timeout waiting for the first decoded video frame".into(),
                ));
            }
            self.pump()?;
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    pub fn should_redraw(&self) -> bool {
        self.dirty
    }

    pub fn mark_clean(&mut self) {
        self.dirty = false;
    }

    pub fn handle(&mut self, cmd: Command) -> Result<bool> {
        match cmd {
            Command::Quit => return Ok(false),
            Command::PlayPause => self.toggle_play()?,
            Command::StepForward(n) => self.step_forward(n)?,
            Command::StepBackward(n) => self.step_backward(n)?,
            Command::VolumeUp => self.adjust_volume(VOLUME_STEP as i16)?,
            Command::VolumeDown => self.adjust_volume(-(VOLUME_STEP as i16))?,
            Command::SpeedUp => self.adjust_speed(SPEED_STEP)?,
            Command::SpeedDown => self.adjust_speed(-SPEED_STEP)?,
            Command::SpeedReset => self.set_speed(1.0)?,
            Command::ToggleLoop => {
                self.looping = !self.looping;
                tracing::info!(looping = self.looping, "loop toggled");
            }
            Command::ShowInfo => {
                self.show_info = !self.show_info;
            }
            Command::SaveHm => {
                if let Err(err) = self.save_as_hm() {
                    self.show_hm_message(&err.to_string());
                }
            }
            Command::SeekFraction(fraction) => self.seek_fraction(fraction)?,
        }
        self.dirty = true;
        Ok(true)
    }

    fn seek_fraction(&mut self, fraction: f64) -> Result<()> {
        let Some(duration) = self.duration.or(self.media.duration) else {
            return Ok(());
        };
        if duration.nseconds() == 0 {
            return Ok(());
        }
        let fraction = fraction.clamp(0.0, 1.0);
        let position = ClockTime::from_nseconds((duration.nseconds() as f64 * fraction) as u64);
        self.want_playing = self.state == PlayerState::Playing || self.want_playing;
        self.ended = false;
        self.seek_to(position, SeekMode::Accurate)?;
        Ok(())
    }

    /// Start a `.hm` save on a background thread. The source file is not changed.
    /// Progress is applied later by [`Self::poll_hm_job`] so the window stays live.
    fn save_as_hm(&mut self) -> Result<()> {
        if self.hm_job.is_some() {
            return Ok(());
        }
        let Some(video) = self.media.video.clone() else {
            return Err(Error::Other("this file has no video to convert".into()));
        };
        self.pause()?;
        self.show_hm_progress(0, 0);
        let source = self.path.clone();
        let width = video.width;
        let height = video.height;
        let (tx, rx) = mpsc::channel();
        self.hm_job = Some(rx);
        thread::spawn(move || {
            let result = DualStep::write_hm(&source, width, height, &mut |done, total| {
                let _ = tx.send(HmUpdate::Progress { done, total });
            });
            let _ = tx.send(HmUpdate::Done(result.map_err(|err| err.to_string())));
        });
        Ok(())
    }

    /// Apply background `.hm` progress. Called from the window loop, not the worker.
    pub fn poll_hm_job(&mut self) {
        let notes: Vec<HmUpdate> = {
            let Some(rx) = &self.hm_job else {
                return;
            };
            rx.try_iter().collect()
        };
        if notes.is_empty() {
            return;
        }
        let mut finished = false;
        for note in notes {
            match note {
                HmUpdate::Progress { done, total } => {
                    self.show_hm_progress(done, total);
                    self.dirty = true;
                }
                HmUpdate::Done(Ok(mut dual)) => {
                    let saved = dual.path.clone();
                    if let Some(video) = &self.media.video {
                        dual.fps = video.fps.unwrap_or(10.0);
                    }
                    if dual.fps <= 1.0 {
                        dual.fps = 10.0;
                    }
                    self.dualstep = Some(dual);
                    self.backward = BackwardDecoder::new();
                    self.ended = false;
                    self.show_hm_message(&format!("saved {}", saved.display()));
                    finished = true;
                    tracing::info!(path = %saved.display(), "wrote .hm backward track");
                }
                HmUpdate::Done(Err(err)) => {
                    self.show_hm_message(&err);
                    finished = true;
                    tracing::error!(error = %err, ".hm save failed");
                }
            }
        }
        if finished {
            self.hm_job = None;
            self.dirty = true;
        }
    }

    fn show_hm_progress(&self, done: u32, total: u32) {
        let text = if total == 0 {
            "saving .hm".to_string()
        } else {
            format!("saving .hm {done}/{total}")
        };
        if let Some(window) = &self.window {
            window.set_message(&text);
            window.set_save_progress(Some((done, total, text)));
        }
    }

    fn show_hm_message(&self, text: &str) {
        if let Some(window) = &self.window {
            window.set_message(text);
            window.set_save_progress(None);
        }
    }

    pub fn toggle_play(&mut self) -> Result<()> {
        match self.state {
            PlayerState::Playing => self.pause(),
            PlayerState::Paused | PlayerState::Ended | PlayerState::FrameStepping | PlayerState::Error => {
                self.play()
            }
            _ => Ok(()),
        }
    }

    pub fn play(&mut self) -> Result<()> {
        if self.state == PlayerState::Error {
            let _ = self.playbin.set_state(State::Ready);
            let _ = self.playbin.set_state(State::Paused);
            self.state = PlayerState::Paused;
        }
        {
            let mut st = self.lock_shared();
            st.freeze_picture = false;
            st.replay = None;
        }
        self.ended = false;
        self.want_playing = true;
        self.ensure_live_path()?;
        self.release_hold();
        self.playbin
            .set_state(State::Playing)
            .map_err(|e| Error::PlayerInit(e.to_string()))?;
        self.state = PlayerState::Playing;
        self.want_playing = true;
        self.dirty = true;
        Ok(())
    }

    pub fn pause(&mut self) -> Result<()> {
        let (_ret, state, pending) = self.playbin.state(ClockTime::ZERO);
        if state == State::Paused && pending == State::VoidPending {
            self.want_playing = false;
            if self.state != PlayerState::Ended && self.state != PlayerState::FrameStepping {
                self.state = PlayerState::Paused;
            }
            return Ok(());
        }
        self.playbin
            .set_state(State::Paused)
            .map_err(|e| Error::PlayerInit(e.to_string()))?;
        self.want_playing = false;
        if self.state != PlayerState::Ended {
            self.state = PlayerState::Paused;
        }
        // Pump until the clock has stopped. Returning early left the file
        // running, so the next Right arrow landed far ahead.
        self.settle_paused();
        self.dirty = true;
        Ok(())
    }

    pub fn seek_to(&mut self, position: ClockTime, mode: SeekMode) -> Result<()> {
        self.state = PlayerState::Seeking;
        {
            let mut st = self.lock_shared();
            st.action = ProbeAction::WaitPreroll;
            st.forget_index();
        }
        self.drain_events();

        let (rate, flags, start_ty, start, stop_ty, stop) =
            crate::player::seeking::seek_args(mode, self.speed, position);
        self.playbin
            .seek(rate, flags, start_ty, start, stop_ty, stop)
            .map_err(|e| Error::Seek(e.to_string()))?;

        self.wait_async_done(Duration::from_secs(8))?;
        let _ = self.wait_probe_event(Duration::from_secs(3));
        self.release_hold();
        if self.want_playing {
            self.playbin
                .set_state(State::Playing)
                .map_err(|e| Error::PlayerInit(e.to_string()))?;
            self.state = PlayerState::Playing;
        } else {
            let _ = self.playbin.set_state(State::Paused);
            self.state = PlayerState::Paused;
        }
        self.dirty = true;
        Ok(())
    }

    pub fn step_forward(&mut self, n: u64) -> Result<()> {
        if n == 0 {
            return Ok(());
        }
        self.pause()?;
        self.state = PlayerState::FrameStepping;
        self.dirty = true;

        for i in 0..n {
            if self.ended {
                break;
            }
            tracing::debug!(step = i + 1, of = n, "forward frame step");
            self.step_forward_one()?;
        }

        self.release_hold();
        self.state = PlayerState::Paused;
        self.dirty = true;
        Ok(())
    }

    pub fn step_backward(&mut self, n: u64) -> Result<()> {
        if n == 0 {
            return Ok(());
        }
        self.pause()?;
        self.state = PlayerState::FrameStepping;
        self.dirty = true;

        for i in 0..n {
            tracing::debug!(step = i + 1, of = n, "backward frame step");
            if !self.step_backward_one()? {
                break;
            }
        }

        self.release_hold();
        self.state = PlayerState::Paused;
        self.dirty = true;
        Ok(())
    }

    pub fn set_volume(&mut self, volume: u8) -> Result<()> {
        self.volume = volume.min(100);
        self.playbin
            .set_property("volume", self.volume as f64 / 100.0);
        self.dirty = true;
        Ok(())
    }

    pub fn set_speed(&mut self, speed: f64) -> Result<()> {
        self.speed = speed.clamp(SPEED_MIN, SPEED_MAX);
        if let Some(pos) = self.current_pts().or_else(|| self.query_position()) {
            self.apply_rate(pos)?;
        }
        self.dirty = true;
        Ok(())
    }

    /// Dispatch GTK events for the video window. No-op when headless.
    pub fn pump_window(&self) {
        if let Some(window) = &self.window {
            window.iterate();
        }
    }

    /// Next command from the video window bar or its keyboard, if one is waiting.
    pub fn poll_window_command(&self) -> Option<Command> {
        self.window.as_ref().and_then(VideoWindow::poll_command)
    }

    /// Refresh the on-screen status label and play/pause button.
    pub fn refresh_controls(&self) {
        if let Some(window) = &self.window {
            window.update(&self.status());
        }
    }

    pub fn poll_bus(&mut self, timeout: ClockTime) -> Result<()> {
        if let Some(msg) = self.bus.timed_pop(timeout) {
            self.handle_bus_message(&msg)?;
        }
        gstreamer::glib::MainContext::default().iteration(false);
        if self.state == PlayerState::Playing {
            self.dirty = true;
        }
        Ok(())
    }

    pub fn status(&self) -> StatusSnapshot {
        let (pts, index) = {
            let st = self.lock_shared();
            (st.current.map(|f| f.pts), st.current.and_then(|f| f.index))
        };
        StatusSnapshot {
            state: self.state,
            position: pts.or_else(|| self.query_position()),
            duration: self.duration,
            frame_index: index,
            pts,
            fps_label: self.media.fps_label(),
            speed: self.speed,
            volume: self.volume,
            looping: self.looping,
            stepping: self.state == PlayerState::FrameStepping,
            info_overlay: self.show_info.then(|| self.media.format()),
        }
    }

    pub fn shutdown(&mut self) -> Result<()> {
        let _ = self.playbin.set_state(State::Null);
        self.window.take();
        Ok(())
    }

    fn step_forward_one(&mut self) -> Result<()> {
        {
            let mut st = self.lock_shared();
            st.freeze_picture = false;
        }
        let current = self.lock_shared().current;
        let Some(cur) = current else {
            return Err(Error::FrameStep("no current frame PTS from probe".into()));
        };

        tracing::debug!(current_pts = %cur.pts, "forward step from");

        if let Some(next) = {
            let st = self.lock_shared();
            st.cache.next(cur.pts).cloned()
        } {
            if adjacent_frames(cur.pts, next.pts, next.duration.or(cur.duration)) {
                tracing::debug!(displayed_pts = %next.pts, "forward step cache hit");
                self.redisplay(&next)?;
                return Ok(());
            }
        }

        self.play_until_next_frame(cur.pts)?;
        return Ok(());
    }

    fn step_backward_one(&mut self) -> Result<bool> {
        {
            let mut st = self.lock_shared();
            st.freeze_picture = false;
        }
        let current = self.lock_shared().current;
        let Some(cur) = current else {
            return Err(Error::FrameStep("no current frame PTS from probe".into()));
        };

        if let Some(first) = self.lock_shared().first_pts {
            if cur.pts <= first || pts_close(cur.pts, first) {
                tracing::debug!(current_pts = %cur.pts, "already at first displayed frame");
                return Ok(false);
            }
        }

        if let Some(prev) = {
            let st = self.lock_shared();
            st.cache.previous(cur.pts).cloned()
        } {
            if adjacent_frames(prev.pts, cur.pts, prev.duration.or(cur.duration)) {
                tracing::debug!(
                    current_pts = %cur.pts,
                    displayed_pts = %prev.pts,
                    "backward step to stored frame"
                );
                self.redisplay(&prev)?;
                return Ok(true);
            }
        }

        if self.dualstep.is_some() {
            match self.step_dualstep(cur.pts) {
                Ok(true) => return Ok(true),
                Ok(false) => return Ok(false),
                Err(err) => {
                    tracing::debug!("DualStep backward step failed: {err}");
                }
            }
        }

        self.decode_previous_frame(cur.pts)?;
        Ok(true)
    }

    /// One picture from the backward track. Catch-up frames stay off the window.
    fn step_dualstep(&mut self, current: ClockTime) -> Result<bool> {
        let pts = current.nseconds();
        if let Some(window) = &self.window {
            window.set_message("DualStep");
            window.iterate();
        }
        if let Some(step) = self.dualstep.as_mut() {
            step.ensure_around(pts)?;
            if let Some(index) = step.nearest(pts) {
                if index == 0 && pts > step.frame_ns().saturating_mul(2) {
                    step.ensure_around(pts.saturating_sub(step.frame_ns()))?;
                } else if index > 0 {
                    let gap = step.frames[index].pts_ns.saturating_sub(step.frames[index - 1].pts_ns);
                    if gap > step.frame_ns().saturating_mul(2) {
                        step.ensure_around(step.frames[index].pts_ns.saturating_sub(step.frame_ns()))?;
                    }
                }
            }
        }
        if let Some(window) = &self.window {
            window.iterate();
        }
        let picture = {
            let (dualstep, backward) = (&self.dualstep, &mut self.backward);
            let Some(step) = dualstep.as_ref() else {
                return Err(Error::FrameStep("no DualStep track".into()));
            };
            backward.picture_before(step, pts)?
        };
        let Some((pts_ns, buffer)) = picture else {
            return Ok(false);
        };
        let frame = crate::player::frame_stepper::CachedFrame {
            pts: ClockTime::from_nseconds(pts_ns),
            duration: None,
            buffer,
            caps: None,
        };
        self.ended = false;
        self.redisplay(&frame)?;
        Ok(true)
    }

    fn decode_previous_frame(&mut self, current_pts: ClockTime) -> Result<()> {
        tracing::debug!(
            current_pts = %current_pts,
            "backward step: one keyframe seek, then decode forward"
        );

        let mut seek_pts = current_pts;
        for attempt in 1..=2 {
            self.state = PlayerState::Seeking;
            {
                let mut st = self.lock_shared();
                st.action = ProbeAction::WaitPreroll;
            }
            self.drain_events();

            tracing::debug!(
                attempt,
                seek_pts = %seek_pts,
                current_pts = %current_pts,
                "backward step: KEY_UNIT SNAP_BEFORE seek"
            );

            let (rate, flags, start_ty, start, stop_ty, stop) =
                crate::player::seeking::seek_args(SeekMode::KeyUnitBefore, 1.0, seek_pts);
            self.playbin
                .seek(rate, flags, start_ty, start, stop_ty, stop)
                .map_err(|e| Error::Seek(e.to_string()))?;

            self.wait_async_done(Duration::from_secs(2))?;

            let preroll = match self.wait_probe_event(Duration::from_secs(2)) {
                Ok(ProbeEvent::Preroll(f) | ProbeEvent::Frame(f)) => f,
                Ok(other) => {
                    tracing::debug!("unexpected probe event after keyframe seek: {other:?}");
                    seek_pts = nudge_before(seek_pts);
                    continue;
                }
                Err(err) => {
                    tracing::debug!("no preroll after keyframe seek: {err}");
                    seek_pts = nudge_before(seek_pts);
                    continue;
                }
            };

            if preroll.pts < current_pts && !pts_close(preroll.pts, current_pts) {
                self.decode_forward_until_before(current_pts)?;
                return Ok(());
            }
            seek_pts = ClockTime::from_nseconds(seek_pts.nseconds().saturating_sub(1_000_000_000));
        }
        Err(Error::FrameStep(
            "could not find a keyframe before the current frame".into(),
        ))
    }

    fn decode_forward_until_before(&mut self, before: ClockTime) -> Result<()> {
        {
            let mut st = self.lock_shared();
            st.action = ProbeAction::CollectBefore { before };
        }
        self.drain_events();
        {
            let mut st = self.lock_shared();
            st.step_release = false;
        }

        tracing::debug!(target_pts = %before, "backward step: decode forward to previous frame");

        self.playbin
            .set_state(State::Playing)
            .map_err(|e| Error::FrameStep(e.to_string()))?;

        let ev = self.wait_probe_event(Duration::from_secs(45));
        {
            let mut st = self.lock_shared();
            st.step_release = true;
        }

        self.playbin
            .set_state(State::Paused)
            .map_err(|e| Error::FrameStep(e.to_string()))?;
        self.settle_paused();
        self.release_hold();

        match ev {
            Ok(ProbeEvent::ReachedTarget { last }) => {
                if let Some(f) = last {
                    if let Some(stored) = {
                        let st = self.lock_shared();
                        st.cache.get(f.pts).cloned()
                    } {
                        self.redisplay(&stored)?;
                    }
                    tracing::debug!(displayed_pts = %f.pts, "backward step displayed");
                    Ok(())
                } else {
                    Err(Error::FrameStep(
                        "decode window contained no frame before the target PTS".into(),
                    ))
                }
            }
            Ok(ProbeEvent::Frame(f) | ProbeEvent::Preroll(f)) => {
                tracing::debug!(displayed_pts = %f.pts, "backward step displayed (frame event)");
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    fn play_until_next_frame(&mut self, after: ClockTime) -> Result<()> {
        {
            let mut st = self.lock_shared();
            st.action = ProbeAction::WaitAfter { after };
            st.step_release = false;
        }
        self.drain_events();
        self.playbin
            .set_state(State::Playing)
            .map_err(|e| Error::FrameStep(e.to_string()))?;
        let ev = self.wait_probe_event(Duration::from_secs(2));
        {
            let mut st = self.lock_shared();
            st.step_release = true;
        }
        self.playbin
            .set_state(State::Paused)
            .map_err(|e| Error::FrameStep(e.to_string()))?;
        self.settle_paused();
        {
            let mut st = self.lock_shared();
            st.freeze_picture = false;
        }
        self.release_hold();
        match ev {
            Ok(ProbeEvent::Frame(f) | ProbeEvent::Preroll(f)) => {
                tracing::debug!(displayed_pts = %f.pts, "forward step via play burst");
                Ok(())
            }
            Ok(_) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Show a frame the player already decoded, and stay paused.
    /// The next decoded buffer is replaced with that picture. Playback does
    /// not continue past it.
    fn redisplay(&mut self, frame: &CachedFrame) -> Result<()> {
        tracing::debug!(
            displayed_pts = %frame.pts,
            "redisplay stored frame"
        );
        self.pause()?;
        {
            let mut st = self.lock_shared();
            st.freeze_picture = false;
            st.replay = Some(frame.clone());
            st.step_release = false;
            st.action = ProbeAction::Idle;
        }
        self.drain_events();
        self.playbin
            .set_state(State::Playing)
            .map_err(|e| Error::FrameStep(e.to_string()))?;
        let ev = self.wait_probe_event(Duration::from_secs(2));
        {
            let mut st = self.lock_shared();
            st.step_release = true;
        }
        self.playbin
            .set_state(State::Paused)
            .map_err(|e| Error::FrameStep(e.to_string()))?;
        self.settle_paused();
        self.stepped_pts = Some(frame.pts);
        self.want_playing = false;
        self.state = PlayerState::Paused;
        self.dirty = true;
        match ev {
            Ok(_) => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn ensure_live_path(&mut self) -> Result<()> {
        self.release_hold();
        let shown = self.stepped_pts.take();
        {
            let mut st = self.lock_shared();
            st.showing_cache = false;
            st.freeze_picture = false;
        }
        if let Some(pts) = shown {
            let needs_seek = match self.query_position() {
                Some(pos) => !pts_close(pos, pts),
                None => true,
            };
            if needs_seek {
                tracing::debug!(displayed_pts = %pts, "play resumes at the stepped frame");
                self.seek_to(pts, SeekMode::Accurate)?;
            }
        }
        Ok(())
    }

    /// Wait until the pipeline reports paused, pumping the window so the bar
    /// stays live. Returns as soon as the clock stops.
    fn settle_paused(&mut self) {
        let deadline = Instant::now() + Duration::from_millis(400);
        loop {
            self.pump_window();
            let (_ret, state, pending) = self.playbin.state(ClockTime::from_mseconds(10));
            if state == State::Paused && pending == State::VoidPending {
                return;
            }
            if Instant::now() >= deadline {
                return;
            }
            if let Some(msg) = self.bus.timed_pop(ClockTime::from_mseconds(10)) {
                let _ = self.handle_bus_message(&msg);
            }
        }
    }

    fn apply_rate(&mut self, position: ClockTime) -> Result<()> {
        self.playbin
            .seek(
                self.speed,
                gstreamer::SeekFlags::FLUSH | gstreamer::SeekFlags::ACCURATE,
                gstreamer::SeekType::Set,
                position,
                gstreamer::SeekType::None,
                ClockTime::NONE,
            )
            .map_err(|e| Error::Seek(format!("rate change: {e}")))?;
        self.wait_async_done(Duration::from_secs(4))?;
        Ok(())
    }

    fn adjust_volume(&mut self, delta: i16) -> Result<()> {
        let next = (self.volume as i16 + delta).clamp(0, 100) as u8;
        self.set_volume(next)
    }

    fn adjust_speed(&mut self, delta: f64) -> Result<()> {
        self.set_speed(self.speed + delta)
    }

    fn query_position(&self) -> Option<ClockTime> {
        self.playbin.query_position::<ClockTime>()
    }

    fn release_hold(&self) {
        let (_ret, state, pending) = self.playbin.state(ClockTime::ZERO);
        if state == State::Playing || pending == State::Playing {
            return;
        }
        let mut st = self.lock_shared();
        if st.action == ProbeAction::Hold {
            st.action = ProbeAction::Idle;
        }
    }

    fn wait_async_done(&mut self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            if Instant::now() >= deadline {
                let (_ret, state, pending) = self.playbin.state(ClockTime::from_mseconds(0));
                if pending == State::VoidPending
                    && (state == State::Paused || state == State::Playing)
                {
                    return Ok(());
                }
                return Err(Error::Seek("timeout waiting for seek to finish".into()));
            }
            let remain = deadline.saturating_duration_since(Instant::now());
            let wait = ClockTime::from_mseconds(remain.as_millis().min(50) as u64);
            if let Some(msg) = self.bus.timed_pop(wait) {
                match msg.view() {
                    MessageView::AsyncDone(_) => return Ok(()),
                    MessageView::Error(err) => {
                        return Err(Error::Seek(format!(
                            "{} ({})",
                            err.error(),
                            err.debug().unwrap_or_default()
                        )));
                    }
                    _ => self.handle_bus_message(&msg)?,
                }
            }
            gstreamer::glib::MainContext::default().iteration(false);
        }
    }

    fn wait_probe_event(&mut self, timeout: Duration) -> Result<ProbeEvent> {
        let deadline = Instant::now() + timeout;
        loop {
            self.pump()?;
            self.pump_window();
            if Instant::now() >= deadline {
                return Err(Error::FrameStep(
                    "timeout waiting for a decoded video frame".into(),
                ));
            }
            let remain = deadline.saturating_duration_since(Instant::now());
            match self
                .event_rx
                .recv_timeout(Duration::from_millis(20).min(remain))
            {
                Ok(ev) => return Ok(ev),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(Error::FrameStep("frame probe channel closed".into()));
                }
            }
        }
    }

    fn drain_events(&self) {
        while self.event_rx.try_recv().is_ok() {}
    }

    fn pump(&mut self) -> Result<()> {
        gstreamer::glib::MainContext::default().iteration(false);
        while let Some(msg) = self.bus.timed_pop(ClockTime::ZERO) {
            self.handle_bus_message(&msg)?;
        }
        Ok(())
    }

    fn handle_bus_message(&mut self, msg: &Message) -> Result<()> {
        match msg.view() {
            MessageView::Error(err) => {
                let text = format!("{} ({})", err.error(), err.debug().unwrap_or_default());
                tracing::error!(error = %text, "gstreamer error");
                self.state = PlayerState::Error;
                self.dirty = true;
                Err(Error::Other(text))
            }
            MessageView::Eos(_) => {
                tracing::info!("end of stream");
                if self.looping {
                    self.ended = false;
                    self.seek_to(ClockTime::ZERO, SeekMode::Normal)?;
                    if self.want_playing || self.state == PlayerState::Playing {
                        self.play()?;
                    }
                } else {
                    self.ended = true;
                    self.state = PlayerState::Ended;
                    let _ = self.playbin.set_state(State::Paused);
                    self.want_playing = false;
                }
                self.dirty = true;
                Ok(())
            }
            MessageView::DurationChanged(_) => {
                self.duration = self.playbin.query_duration::<ClockTime>();
                self.dirty = true;
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn lock_shared(&self) -> std::sync::MutexGuard<'_, SharedFrameState> {
        self.shared.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.playbin.set_state(State::Null);
        let _ = (&self.video_bin, &self.display_sink, &self.present_pad);
    }
}

struct VideoSinkBin {
    bin: gstreamer::Bin,
    display_sink: Element,
    present_pad: gstreamer::Pad,
    window: Option<VideoWindow>,
}

/// NVIDIA's V4L2 decoder on this machine writes unusable NVMM frames (grey
/// pixels, or "Feature not supported on this GPU"). mpv leaves hardware decode
/// off unless asked; libav (`avdec_*`) is the same choice.
fn adjacent_frames(a: ClockTime, b: ClockTime, duration: Option<ClockTime>) -> bool {
    let gap = a.nseconds().abs_diff(b.nseconds());
    let limit = duration
        .map(|d| d.nseconds() + d.nseconds() / 2)
        .unwrap_or(50_000_000);
    gap > 0 && gap <= limit
}

fn wait_for_step_release(shared: &std::sync::Mutex<SharedFrameState>) {
    let deadline = Instant::now() + Duration::from_millis(200);
    loop {
        {
            let st = shared.lock().unwrap_or_else(|e| e.into_inner());
            if st.step_release {
                break;
            }
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    // Stay in the probe a little longer so the player thread can enter
    // set_state(Paused) and block on this stream lock. Returning as soon as
    // the flag is set let the decoder emit extra frames before the pause.
    std::thread::sleep(Duration::from_millis(15));
}

fn replace_with_stored(
    info: &mut gstreamer::PadProbeInfo,
    frame: &CachedFrame,
    live_pts: gstreamer::ClockTime,
    duration: Option<gstreamer::ClockTime>,
) {
    let Some(out) = info.buffer_mut() else {
        return;
    };
    let Some(mut shown) = copy_buffer(&frame.buffer) else {
        return;
    };
    {
        let buf = shown.make_mut();
        // Timestamp the copy as the buffer the sink is expecting, so it is
        // shown now. The picture is still the stored frame.
        buf.set_pts(live_pts);
        if let Some(duration) = duration.or(frame.duration) {
            buf.set_duration(duration);
        }
    }
    *out = shown;
}

fn copy_buffer(buffer: &gstreamer::Buffer) -> Option<gstreamer::Buffer> {
    unsafe {
        let copied = gstreamer::ffi::gst_mini_object_copy(
            buffer.as_ptr() as *const gstreamer::ffi::GstMiniObject,
        );
        if copied.is_null() {
            None
        } else {
            Some(gstreamer::Buffer::from_glib_full(
                copied as *mut gstreamer::ffi::GstBuffer,
            ))
        }
    }
}

fn prefer_software_decode() {
    let Some(feature) = gstreamer::Registry::get().lookup_feature("nvv4l2decoder") else {
        return;
    };
    feature.set_rank(gstreamer::Rank::NONE);
    tracing::info!("demoted nvv4l2decoder so libav software decode is used");
}

fn wait_until_paused(playbin: &Element, window: Option<&VideoWindow>) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(window) = window {
            window.iterate();
        }
        let state_ret = playbin.state(ClockTime::from_mseconds(50));
        if state_ret.0.is_err() {
            return Err(Error::UnsupportedMedia(format!(
                "pipeline did not preroll ({state_ret:?})"
            )));
        }
        if state_ret.1 == State::Paused {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Error::UnsupportedMedia(format!(
                "pipeline did not preroll ({state_ret:?})"
            )));
        }
    }
}

fn build_video_sink_bin(
    headless: bool,
    shared: Arc<Mutex<SharedFrameState>>,
    title: &str,
) -> Result<VideoSinkBin> {
    let bin = gstreamer::Bin::builder().name("egg-vsink").build();

    let queue = ElementFactory::make("queue")
        .name("in_queue")
        .property("max-size-buffers", 8u32)
        .property("max-size-time", 0u64)
        .property("max-size-bytes", 0u32)
        .build()
        .map_err(|e| Error::VideoSink(format!("queue: {e}")))?;

    let convert = ElementFactory::make("videoconvert")
        .name("vconvert")
        .build()
        .map_err(|e| Error::VideoSink(format!("videoconvert: {e}")))?;

    let (window, xid) = if headless {
        (None, 0)
    } else {
        crate::ui::window::ensure_gtk()?;
        let (window, xid) = VideoWindow::create(title)?;
        (Some(window), xid)
    };
    // Xv draws YUV directly. Forcing BGRA through gtksink/Cairo was the grey
    // blocks and the sub-1x playback.
    let sink_factory = if headless { "fakesink" } else { "xvimagesink" };
    let display_sink = ElementFactory::make(sink_factory)
        .name("display_sink")
        .build()
        .map_err(|e| Error::VideoSink(format!("{sink_factory}: {e}")))?;
    if display_sink.find_property("sync").is_some() {
        display_sink.set_property("sync", !headless);
    }
    if display_sink.find_property("async").is_some() && headless {
        display_sink.set_property("async", false);
    }
    if display_sink.find_property("force-aspect-ratio").is_some() {
        display_sink.set_property("force-aspect-ratio", true);
    }
    if display_sink.find_property("handle-events").is_some() {
        display_sink.set_property("handle-events", false);
    }
    if xid != 0 {
        if let Some(overlay) = display_sink.dynamic_cast_ref::<gstreamer_video::VideoOverlay>() {
            unsafe { overlay.set_window_handle(xid as usize) };
            overlay.handle_events(false);
            overlay.expose();
        } else {
            return Err(Error::VideoSink(
                "xvimagesink does not support a window handle".into(),
            ));
        }
    }

    bin.add_many([&queue, &convert, &display_sink])
        .map_err(|e| Error::VideoSink(e.to_string()))?;
    queue
        .link(&convert)
        .map_err(|e| Error::VideoSink(format!("queue→convert: {e}")))?;
    convert
        .link(&display_sink)
        .map_err(|e| Error::VideoSink(format!("convert→sink: {e}")))?;
    let present_pad = convert
        .static_pad("sink")
        .ok_or_else(|| Error::VideoSink("videoconvert has no sink pad".into()))?;

    let queue_src = queue
        .static_pad("src")
        .ok_or_else(|| Error::VideoSink("queue has no src pad".into()))?;
    let queue_sink = queue
        .static_pad("sink")
        .ok_or_else(|| Error::VideoSink("queue has no sink pad".into()))?;
    let ghost = GhostPad::builder_with_target(&queue_sink)
        .map_err(|e| Error::VideoSink(e.to_string()))?
        .name("sink")
        .build();
    ghost
        .set_active(true)
        .map_err(|e| Error::VideoSink(e.to_string()))?;
    bin.add_pad(&ghost)
        .map_err(|e| Error::VideoSink(e.to_string()))?;

    let probe_shared = Arc::clone(&shared);
    queue_src.add_probe(PadProbeType::BUFFER, move |pad, info| {
        let replay = {
            let mut st = probe_shared.lock().unwrap_or_else(|e| e.into_inner());
            st.replay.take()
        };
        if let Some(frame) = replay {
            let live_pts = info.buffer().and_then(|b| b.pts());
            let live_duration = info.buffer().and_then(|b| b.duration());
            let live_owned = info.buffer().and_then(|b| copy_buffer(b));
            if let Some(out) = info.buffer_mut() {
                // Own memory, not the decoder pool. Keep the live timestamp so
                // the sink does not drop the frame as late and then stall.
                let Some(mut shown) = copy_buffer(&frame.buffer) else {
                    return PadProbeReturn::Ok;
                };
                if let Some(pts) = live_pts {
                    shown.make_mut().set_pts(pts);
                }
                *out = shown;
            }
            let shown = FrameInfo {
                pts: frame.pts,
                duration: frame.duration,
                index: None,
            };
            let mut st = probe_shared.lock().unwrap_or_else(|e| e.into_inner());
            if let (Some(pts), Some(buffer)) = (live_pts, live_owned) {
                if st.cache.get(pts).is_none() {
                    let caps = st.caps.clone();
                    st.cache.insert(CachedFrame {
                        pts,
                        duration: live_duration,
                        buffer,
                        caps,
                    });
                }
            }
            st.showing_cache = true;
            st.action = ProbeAction::Hold;
            st.update_current(shown);
            let _ = st.event_tx.send(ProbeEvent::Frame(shown));
            return PadProbeReturn::Ok;
        }

        if probe_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .freeze_picture
        {
            return PadProbeReturn::Drop;
        }

        let Some(buffer) = info.buffer() else {
            return PadProbeReturn::Ok;
        };
        let Some(pts) = buffer.pts() else {
            return PadProbeReturn::Ok;
        };
        let duration = buffer.duration();
        let caps = pad.current_caps();

        let mut st = probe_shared.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(caps) = caps.clone() {
            st.caps = Some(caps);
        }

        let frame = FrameInfo {
            pts,
            duration,
            index: None,
        };
        let cached_caps = st.caps.clone();
        let store = |st: &mut SharedFrameState| {
            if let Some(owned) = copy_buffer(buffer) {
                st.cache.insert(CachedFrame {
                    pts,
                    duration,
                    buffer: owned,
                    caps: cached_caps.clone(),
                });
            }
        };

        match st.action {
            ProbeAction::Idle => {
                store(&mut st);
                st.update_current(frame);
                PadProbeReturn::Ok
            }
            ProbeAction::WaitPreroll => {
                store(&mut st);
                st.update_current(frame);
                st.action = ProbeAction::Hold;
                let _ = st.event_tx.send(ProbeEvent::Preroll(frame));
                PadProbeReturn::Ok
            }
            ProbeAction::WaitAfter { after } => {
                if pts == after {
                    return PadProbeReturn::Ok;
                }
                store(&mut st);
                st.update_current(frame);
                st.action = ProbeAction::Hold;
                let _ = st.event_tx.send(ProbeEvent::Frame(frame));
                PadProbeReturn::Ok
            }
            ProbeAction::CollectBefore { before } => {
                if pts >= before {
                    let last = st.current;
                    let held = last.and_then(|f| st.cache.get(f.pts).cloned());
                    st.action = ProbeAction::Hold;
                    let _ = st.event_tx.send(ProbeEvent::ReachedTarget { last });
                    tracing::debug!(
                        decoded_pts = %pts,
                        target_pts = %before,
                        "stopping on the previous frame"
                    );
                    // Pass the stored previous picture, not the keyframe or a
                    // later frame. Returning Ok lets the sink hold the decoder
                    // so the rest of the file is not decoded before we pause.
                    if let Some(held) = held {
                        replace_with_stored(info, &held, pts, duration);
                    }
                    return PadProbeReturn::Ok;
                }
                store(&mut st);
                st.update_current(frame);
                tracing::debug!(decoded_pts = %pts, target_pts = %before, "collecting frame");
                PadProbeReturn::Drop
            }
            ProbeAction::Hold => {
                // Another buffer arrived before the player thread paused.
                // Keep it in the ring so the next step is that frame, but
                // keep painting the frame already chosen.
                store(&mut st);
                let held = st.current.and_then(|f| st.cache.get(f.pts).cloned());
                drop(st);
                wait_for_step_release(&probe_shared);
                if let Some(held) = held {
                    replace_with_stored(info, &held, pts, duration);
                }
                PadProbeReturn::Ok
            }
        }
    });

    Ok(VideoSinkBin {
        bin,
        display_sink,
        present_pad,
        window,
    })
}

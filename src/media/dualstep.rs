//! DualStep: a dual-predictive container.
//!
//! The source file stays the forward H.264 track. This file stores a second
//! H.264 track encoded backward in chunks of [`CHUNK`] frames, plus an index
//! from each display timestamp to that frame's access unit. The player decodes
//! the backward track with `avdec_h264`.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use gstreamer::prelude::*;
use gstreamer::{ClockTime, ElementFactory, FlowSuccess, Pipeline, State};
use gstreamer_app::{AppSink, AppSrc};

use crate::error::{Error, Result};

pub const CHUNK: u32 = 16;
const MAGIC: &[u8; 8] = b"HMSTEP1\0";
const VERSION: u32 = 1;
const TIMESCALE: u32 = 1_000_000_000;
const HEADER_LEN: u64 = 32;
const ENTRY_LEN: u64 = 24;

#[derive(Debug, Clone)]
pub struct FrameEnt {
    pub pts_ns: u64,
    pub offset: u64,
    pub size: u32,
    pub key: bool,
    pub chunk_id: u32,
    pub file: PathBuf,
}

#[derive(Debug)]
pub struct DualStep {
    pub path: PathBuf,
    pub source: PathBuf,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub frames: Vec<FrameEnt>,
}

impl DualStep {
    pub fn path_for(source: &Path) -> PathBuf {
        source.with_extension("hm")
    }

    fn legacy_hm_path(source: &Path) -> PathBuf {
        let name = source
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("video");
        source.with_file_name(format!("{name}.hm"))
    }

    /// The video that an `.hm` file was built from, sitting in the same folder.
    pub fn source_for_hm(hm: &Path) -> Result<PathBuf> {
        let parent = hm.parent().unwrap_or_else(|| Path::new("."));
        let stem = hm
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| Error::OpenFile(format!("{} has no name", hm.display())))?;
        let direct = parent.join(stem);
        if direct.is_file() && direct != hm {
            return Ok(direct);
        }
        for ext in ["mp4", "mkv", "mov", "webm", "avi", "m4v"] {
            let candidate = parent.join(format!("{stem}.{ext}"));
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
        Err(Error::OpenFile(format!(
            "{} needs the original video next to it, for example {}.mp4",
            hm.display(),
            stem
        )))
    }

    /// Load an existing backward track, or an empty shell. The whole movie is
    /// not converted on open. A 16-frame chunk is written when Left needs it.
    pub fn prepare(source: &Path, width: u32, height: u32, fps: f64) -> Result<Self> {
        let dest = Self::path_for(source);
        let legacy = Self::legacy_hm_path(source);
        let fps = if fps.is_finite() && fps > 1.0 { fps } else { 10.0 };
        let loaded = Self::reuse(source, &dest)?
            .map(|step| (step, dest.clone()))
            .or(Self::reuse(source, &legacy)?.map(|step| (step, legacy)));
        if let Some((mut existing, file)) = loaded {
            existing.source = source.to_path_buf();
            existing.path = file.clone();
            existing.fps = fps;
            for (index, frame) in existing.frames.iter_mut().enumerate() {
                frame.file = file.clone();
                frame.chunk_id = index as u32 / CHUNK;
            }
            return Ok(existing);
        }
        Ok(Self {
            path: dest,
            source: source.to_path_buf(),
            width,
            height,
            fps,
            frames: Vec::new(),
        })
    }

    pub fn frame_ns(&self) -> u64 {
        (1_000_000_000.0 / self.fps) as u64
    }

    pub fn covers(&self, pts_ns: u64) -> bool {
        match self.nearest(pts_ns) {
            Some(index) => {
                self.frames[index].pts_ns.abs_diff(pts_ns) <= self.frame_ns() + self.frame_ns() / 2
            }
            None => false,
        }
    }

    /// Build the 16-frame backward chunk that contains `pts_ns`, if it is not loaded.
    pub fn ensure_around(&mut self, pts_ns: u64) -> Result<()> {
        let span = self.frame_ns().saturating_mul(CHUNK as u64).max(1);
        let chunk_id = (pts_ns / span) as u32;
        if self.frames.iter().any(|frame| frame.chunk_id == chunk_id) {
            return Ok(());
        }
        let start_sec = chunk_id as f64 * (CHUNK as f64) / self.fps;
        let (times, pixels) = extract_chunk(
            &self.source,
            self.width,
            self.height,
            start_sec,
            CHUNK as usize,
        )?;
        if pixels.is_empty() {
            return Err(Error::FrameStep("could not read frames for DualStep".into()));
        }
        let pairs: Vec<(u64, Vec<u8>)> = times.into_iter().zip(pixels).collect();
        let annex = encode_reversed(&pairs, self.width, self.height)?;
        let units = access_units(&annex);
        let vcl: Vec<_> = units.into_iter().filter(|unit| unit.has_vcl).collect();
        if vcl.len() != pairs.len() {
            return Err(Error::FrameStep(format!(
                "backward chunk produced {} frames, expected {}",
                vcl.len(),
                pairs.len()
            )));
        }
        let dir = chunk_dir(&self.source);
        std::fs::create_dir_all(&dir)?;
        let file_path = dir.join(format!("{chunk_id}.h264"));
        let mut stored = Vec::new();
        {
            let mut out = File::create(&file_path)?;
            for (rev, unit) in vcl.iter().enumerate() {
                let display = pairs.len() - 1 - rev;
                let pos = out.stream_position()?;
                out.write_all(&annex[unit.start..unit.end])?;
                stored.push(FrameEnt {
                    pts_ns: pairs[display].0,
                    offset: pos,
                    size: (unit.end - unit.start) as u32,
                    key: rev == 0,
                    chunk_id,
                    file: file_path.clone(),
                });
            }
        }
        self.frames.retain(|frame| frame.chunk_id != chunk_id);
        self.frames.extend(stored);
        self.frames.sort_by_key(|frame| frame.pts_ns);
        Ok(())
    }

    pub fn nearest(&self, pts_ns: u64) -> Option<usize> {
        self.frames
            .iter()
            .enumerate()
            .min_by_key(|(_, frame)| frame.pts_ns.abs_diff(pts_ns))
            .map(|(index, _)| index)
    }

    fn reuse(source: &Path, dest: &Path) -> Result<Option<Self>> {
        let Ok(src_meta) = source.metadata() else {
            return Ok(None);
        };
        let Ok(dst_meta) = dest.metadata() else {
            return Ok(None);
        };
        if dst_meta.modified().ok() < src_meta.modified().ok() {
            return Ok(None);
        }
        match Self::load(dest) {
            Ok(step) => Ok(Some(step)),
            Err(_) => Ok(None),
        }
    }

    fn load(path: &Path) -> Result<Self> {
        let mut file = File::open(path)?;
        let mut magic = [0u8; 8];
        file.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(Error::Other("not a DualStep file".into()));
        }
        let version = read_u32(&mut file)?;
        let width = read_u32(&mut file)?;
        let height = read_u32(&mut file)?;
        let _timescale = read_u32(&mut file)?;
        let _chunk = read_u32(&mut file)?;
        let count = read_u32(&mut file)?;
        if version != VERSION || count == 0 || width == 0 || height == 0 {
            return Err(Error::Other("DualStep header is empty".into()));
        }
        let mut frames = Vec::with_capacity(count as usize);
        for _ in 0..count {
            frames.push(FrameEnt {
                pts_ns: read_u64(&mut file)?,
                offset: read_u64(&mut file)?,
                size: read_u32(&mut file)?,
                key: read_u32(&mut file)? & 1 == 1,
                chunk_id: 0,
                file: path.to_path_buf(),
            });
        }
        Ok(Self {
            path: path.to_path_buf(),
            source: PathBuf::new(),
            width,
            height,
            fps: 30.0,
            frames,
        })
    }

    /// Write `<video>.hm` beside `source`. The original file is not changed.
    pub fn write_hm(
        source: &Path,
        width: u32,
        height: u32,
        progress: &mut dyn FnMut(u32, u32),
    ) -> Result<Self> {
        let dest = Self::path_for(source);
        Self::build(source, &dest, width, height, progress)?;
        let mut loaded = Self::load(&dest)?;
        loaded.source = source.to_path_buf();
        for (index, frame) in loaded.frames.iter_mut().enumerate() {
            frame.file = dest.clone();
            frame.chunk_id = index as u32 / CHUNK;
        }
        Ok(loaded)
    }

    fn build(
        source: &Path,
        dest: &Path,
        width: u32,
        height: u32,
        progress: &mut dyn FnMut(u32, u32),
    ) -> Result<()> {
        if width == 0 || height == 0 {
            return Err(Error::Other("DualStep needs a video size".into()));
        }
        let pts = source_pts(source)?;
        let total = pts.len() as u32;
        if total == 0 {
            return Err(Error::Other("no video frames to convert".into()));
        }
        progress(0, total);
        let partial = dest.with_extension("hm.partial");
        let mut out = File::create(&partial)?;
        out.write_all(MAGIC)?;
        write_u32(&mut out, VERSION)?;
        write_u32(&mut out, width)?;
        write_u32(&mut out, height)?;
        write_u32(&mut out, TIMESCALE)?;
        write_u32(&mut out, CHUNK)?;
        write_u32(&mut out, total)?;
        out.write_all(&vec![0u8; (total as usize) * ENTRY_LEN as usize])?;

        let frame_bytes = (width as usize)
            .saturating_mul(height as usize)
            .saturating_mul(3)
            / 2;
        if frame_bytes == 0 {
            return Err(Error::Other("frame size is zero".into()));
        }

        let mut decoder = gentle_ffmpeg()
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-threads",
                "1",
                "-i",
            ])
            .arg(source)
            .args(["-map", "0:v:0", "-f", "rawvideo", "-pix_fmt", "yuv420p", "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Error::Other(format!("ffmpeg decode: {e}")))?;
        let mut stdout = decoder.stdout.take().ok_or_else(|| Error::Other("ffmpeg has no stdout".into()))?;
        let stderr = decoder.stderr.take();
        let err_task = thread::spawn(move || {
            let mut buf = String::new();
            if let Some(mut stderr) = stderr {
                let _ = stderr.read_to_string(&mut buf);
            }
            buf
        });

        let mut entries = Vec::with_capacity(pts.len());
        let mut raw = vec![0u8; frame_bytes];
        let mut index = 0usize;
        while index < pts.len() {
            let mut chunk = Vec::new();
            while chunk.len() < CHUNK as usize && index < pts.len() {
                if !read_exact_or_eof(&mut stdout, &mut raw)? {
                    break;
                }
                chunk.push((pts[index], raw.clone()));
                index += 1;
            }
            if chunk.is_empty() {
                break;
            }
            let annex = encode_reversed(&chunk, width, height)?;
            let units = access_units(&annex);
            let vcl: Vec<_> = units
                .into_iter()
                .filter(|unit| unit.has_vcl)
                .collect();
            if vcl.len() != chunk.len() {
                let _ = decoder.kill();
                return Err(Error::Other(format!(
                    "backward chunk produced {} frames, expected {}",
                    vcl.len(),
                    chunk.len()
                )));
            }
            // Encoded newest-first: the first access unit is the last display frame.
            for (rev, unit) in vcl.iter().enumerate() {
                let display = chunk.len() - 1 - rev;
                let pos = out.stream_position()?;
                out.write_all(&annex[unit.start..unit.end])?;
                entries.push(FrameEnt {
                    pts_ns: chunk[display].0,
                    offset: pos,
                    size: (unit.end - unit.start) as u32,
                    key: rev == 0,
                    chunk_id: 0,
                    file: dest.to_path_buf(),
                });
            }
            progress(index as u32, total);
            // Leave the rest of the machine a timeslice. An uncapped encode
            // takes every core and the desktop stops responding.
            thread::sleep(Duration::from_millis(50));
        }
        let status = decoder.wait().map_err(|e| Error::Other(format!("ffmpeg decode: {e}")))?;
        let err_log = err_task.join().unwrap_or_default();
        if !status.success() {
            return Err(Error::Other(format!("ffmpeg decode failed: {err_log}")));
        }
        if entries.len() != pts.len() {
            return Err(Error::Other(format!(
                "decoded {} frames, probe listed {}",
                entries.len(),
                pts.len()
            )));
        }
        // Entries were appended chunk by chunk, and each chunk is newest-first.
        // Rewrite them into display order for the index.
        entries.sort_by_key(|frame| frame.pts_ns);
        if entries.len() != pts.len() {
            return Err(Error::Other("DualStep index length mismatch".into()));
        }
        out.seek(SeekFrom::Start(HEADER_LEN))?;
        for frame in &entries {
            write_u64(&mut out, frame.pts_ns)?;
            write_u64(&mut out, frame.offset)?;
            write_u32(&mut out, frame.size)?;
            write_u32(&mut out, u32::from(frame.key))?;
        }
        out.flush()?;
        drop(out);
        std::fs::rename(&partial, dest)?;
        Ok(())
    }

    pub fn read_au(&self, index: usize) -> Result<Vec<u8>> {
        let frame = self
            .frames
            .get(index)
            .ok_or_else(|| Error::FrameStep("DualStep frame is out of range".into()))?;
        let mut file = File::open(&frame.file)?;
        file.seek(SeekFrom::Start(frame.offset))?;
        let mut buf = vec![0u8; frame.size as usize];
        file.read_exact(&mut buf)?;
        Ok(buf)
    }
}

/// Headless decoder for the backward track. At most one chunk is decoded, and
/// only the requested picture is returned.
pub struct BackwardDecoder {
    pipeline: Option<Pipeline>,
    appsrc: Option<AppSrc>,
    appsink: Option<AppSink>,
    primed_chunk: Option<u32>,
    /// Display index the next pull will produce, when the decoder is already
    /// sitting on the following frame in this chunk.
    primed_next: Option<u32>,
    /// Pictures already decoded from the backward track, keyed by PTS.
    /// Two chunks are enough to step back without decoding the same GOP again.
    pictures: Vec<(u32, u64, gstreamer::Buffer)>,
}

impl BackwardDecoder {
    pub fn new() -> Self {
        Self {
            pipeline: None,
            appsrc: None,
            appsink: None,
            primed_chunk: None,
            primed_next: None,
            pictures: Vec::new(),
        }
    }

    pub fn picture_before(&mut self, step: &DualStep, current_ns: u64) -> Result<Option<(u64, gstreamer::Buffer)>> {
        let Some(index) = step.nearest(current_ns) else {
            return Ok(None);
        };
        if step.frames[index].pts_ns.abs_diff(current_ns) > step.frame_ns().saturating_mul(2) {
            return Err(Error::FrameStep(
                "backward track is not positioned on this frame".into(),
            ));
        }
        if index == 0 {
            return Ok(None);
        }
        let target = index - 1;
        let pts = step.frames[target].pts_ns;
        let current_pts = step.frames[index].pts_ns;
        if current_pts.saturating_sub(pts) > step.frame_ns().saturating_mul(2) {
            return Err(Error::FrameStep(
                "backward track has a gap before this frame".into(),
            ));
        }
        if let Some((_, _, buffer)) = self.pictures.iter().find(|(_, stored, _)| *stored == pts) {
            return Ok(Some((pts, buffer.clone())));
        }
        self.decode_chunk(step, step.frames[target].chunk_id)?;
        if let Some((_, _, buffer)) = self.pictures.iter().find(|(_, stored, _)| *stored == pts) {
            return Ok(Some((pts, buffer.clone())));
        }
        let buffer = self.obtain(step, target)?;
        self.remember(step.frames[target].chunk_id, pts, buffer.clone());
        Ok(Some((pts, buffer)))
    }

    fn remember(&mut self, chunk: u32, pts: u64, buffer: gstreamer::Buffer) {
        let chunks: Vec<u32> = self.pictures.iter().map(|(id, _, _)| *id).collect();
        let distinct: Vec<u32> = {
            let mut ids = chunks;
            ids.sort_unstable();
            ids.dedup();
            ids
        };
        if distinct.len() >= 2 && !distinct.contains(&chunk) {
            let drop_id = distinct
                .iter()
                .copied()
                .max_by_key(|id| id.abs_diff(chunk))
                .unwrap_or(distinct[0]);
            self.pictures.retain(|(id, _, _)| *id != drop_id);
        }
        if let Some(slot) = self.pictures.iter_mut().find(|(_, stored, _)| *stored == pts) {
            *slot = (chunk, pts, buffer);
        } else {
            self.pictures.push((chunk, pts, buffer));
        }
    }

    fn decode_chunk(&mut self, step: &DualStep, chunk: u32) -> Result<()> {
        let mut members: Vec<usize> = step
            .frames
            .iter()
            .enumerate()
            .filter(|(_, frame)| frame.chunk_id == chunk)
            .map(|(index, _)| index)
            .collect();
        if members.is_empty() {
            return Ok(());
        }
        let have = self
            .pictures
            .iter()
            .filter(|(id, _, _)| *id == chunk)
            .count();
        if have >= members.len() {
            return Ok(());
        }
        self.pictures.retain(|(id, _, _)| *id != chunk);
        members.sort_by_key(|index| step.frames[*index].pts_ns);
        self.reset();
        self.ensure_pipeline()?;
        let mut got = Vec::new();
        for display in members.iter().rev() {
            let au = step.read_au(*display)?;
            self.push_au(&au)?;
            if let Some(buffer) = self.try_pull()? {
                got.push(buffer);
            }
        }
        self.finish_stream()?;
        while got.len() < members.len() {
            match self.try_pull()? {
                Some(buffer) => got.push(buffer),
                None => break,
            }
        }
        self.reset();
        if got.len() < members.len() {
            return Ok(());
        }
        for (index, buffer) in members.iter().rev().zip(got) {
            let pts = step.frames[*index].pts_ns;
            self.pictures.push((chunk, pts, buffer));
        }
        Ok(())
    }

    fn obtain(&mut self, step: &DualStep, target: usize) -> Result<gstreamer::Buffer> {
        let chunk = step.frames[target].chunk_id;
        let mut members: Vec<usize> = step
            .frames
            .iter()
            .enumerate()
            .filter(|(_, frame)| frame.chunk_id == chunk)
            .map(|(index, _)| index)
            .collect();
        members.sort_by_key(|index| step.frames[*index].pts_ns);
        let Some(pos) = members.iter().position(|index| *index == target) else {
            return Err(Error::FrameStep("DualStep chunk is missing the frame".into()));
        };
        let high = members.len() - 1;
        if self.primed_chunk == Some(chunk) && self.primed_next == Some(target as u32) {
            if let Some(buffer) = self.pull_one(&step.read_au(target)?)? {
                self.primed_next = (pos > 0).then_some(members[pos - 1] as u32);
                return Ok(buffer);
            }
        }
        self.reset();
        self.ensure_pipeline()?;
        let expected = high - pos + 1;
        let mut got = Vec::new();
        let mut flushed = false;
        for display in members[pos..=high].iter().rev() {
            let au = step.read_au(*display)?;
            self.push_au(&au)?;
            if let Some(buffer) = self.try_pull()? {
                got.push(buffer);
            }
        }
        if got.len() < expected {
            self.finish_stream()?;
            flushed = true;
            while got.len() < expected {
                match self.try_pull()? {
                    Some(buffer) => got.push(buffer),
                    None => break,
                }
            }
        }
        if got.len() < expected {
            self.reset();
            return Err(Error::FrameStep(
                "timeout waiting for a decoded video frame".into(),
            ));
        }
        let buffer = got.swap_remove(expected - 1);
        if flushed {
            self.reset();
        } else {
            self.primed_chunk = Some(chunk);
            self.primed_next = (pos > 0).then_some(members[pos - 1] as u32);
        }
        Ok(buffer)
    }

    fn ensure_pipeline(&mut self) -> Result<()> {
        if self.pipeline.is_some() {
            return Ok(());
        }
        let pipeline = Pipeline::with_name("dstep-back");
        let appsrc = ElementFactory::make("appsrc")
            .name("dstep_src")
            .build()
            .map_err(|e| Error::FrameStep(format!("appsrc: {e}")))?;
        let parse = ElementFactory::make("h264parse")
            .name("dstep_parse")
            .build()
            .map_err(|e| Error::FrameStep(format!("h264parse: {e}")))?;
        let dec = ElementFactory::make("avdec_h264")
            .name("dstep_dec")
            .build()
            .map_err(|e| Error::FrameStep(format!("avdec_h264: {e}")))?;
        let sink = ElementFactory::make("appsink")
            .name("dstep_sink")
            .property("sync", false)
            .property("max-buffers", 8u32)
            .property("drop", false)
            .build()
            .map_err(|e| Error::FrameStep(format!("appsink: {e}")))?;
        pipeline
            .add_many([&appsrc, &parse, &dec, &sink])
            .map_err(|e| Error::FrameStep(e.to_string()))?;
        appsrc
            .link(&parse)
            .map_err(|e| Error::FrameStep(format!("appsrc link: {e}")))?;
        parse
            .link(&dec)
            .map_err(|e| Error::FrameStep(format!("parse link: {e}")))?;
        dec.link(&sink)
            .map_err(|e| Error::FrameStep(format!("dec link: {e}")))?;
        let src = appsrc
            .dynamic_cast::<AppSrc>()
            .map_err(|_| Error::FrameStep("appsrc cast failed".into()))?;
        let caps = gstreamer::Caps::builder("video/x-h264")
            .field("stream-format", "byte-stream")
            .field("alignment", "au")
            .build();
        src.set_caps(Some(&caps));
        src.set_format(gstreamer::Format::Time);
        let sink = sink
            .dynamic_cast::<AppSink>()
            .map_err(|_| Error::FrameStep("appsink cast failed".into()))?;
        pipeline
            .set_state(State::Playing)
            .map_err(|e| Error::FrameStep(e.to_string()))?;
        self.pipeline = Some(pipeline);
        self.appsrc = Some(src);
        self.appsink = Some(sink);
        Ok(())
    }

    fn push_au(&self, au: &[u8]) -> Result<()> {
        let src = self
            .appsrc
            .as_ref()
            .ok_or_else(|| Error::FrameStep("backward decoder is not ready".into()))?;
        let mut buffer = gstreamer::Buffer::from_slice(au.to_vec());
        {
            let buf = buffer.get_mut().ok_or_else(|| Error::FrameStep("buffer not writable".into()))?;
            buf.set_pts(ClockTime::NONE);
            buf.set_dts(ClockTime::NONE);
        }
        match src.push_buffer(buffer) {
            Ok(FlowSuccess::Ok) => Ok(()),
            Ok(_) => Ok(()),
            Err(err) => Err(Error::FrameStep(format!("backward push: {err}"))),
        }
    }

    fn try_pull(&self) -> Result<Option<gstreamer::Buffer>> {
        let sink = self
            .appsink
            .as_ref()
            .ok_or_else(|| Error::FrameStep("backward decoder is not ready".into()))?;
        match sink.try_pull_sample(ClockTime::from_mseconds(80)) {
            Some(sample) => {
                let buffer = sample
                    .buffer()
                    .ok_or_else(|| Error::FrameStep("decoded sample has no buffer".into()))?
                    .copy();
                Ok(Some(buffer))
            }
            None => Ok(None),
        }
    }

    fn pull_one(&self, au: &[u8]) -> Result<Option<gstreamer::Buffer>> {
        self.push_au(au)?;
        if let Some(buffer) = self.try_pull()? {
            return Ok(Some(buffer));
        }
        self.finish_stream()?;
        self.try_pull()
    }

    fn finish_stream(&self) -> Result<()> {
        if let Some(src) = &self.appsrc {
            let _ = src.end_of_stream();
        }
        Ok(())
    }

    fn reset(&mut self) {
        if let Some(pipeline) = &self.pipeline {
            let _ = pipeline.set_state(State::Null);
        }
        self.pipeline = None;
        self.appsrc = None;
        self.appsink = None;
        self.primed_chunk = None;
        self.primed_next = None;
    }
}

impl Drop for BackwardDecoder {
    fn drop(&mut self) {
        self.reset();
    }
}

struct Au {
    start: usize,
    end: usize,
    has_vcl: bool,
}

fn access_units(data: &[u8]) -> Vec<Au> {
    let nals = nal_ranges(data);
    let mut units = Vec::new();
    let mut start: Option<usize> = None;
    let mut has_vcl = false;
    for (off, _end, nal_type) in &nals {
        let vcl = *nal_type == 1 || *nal_type == 5;
        if *nal_type == 9 {
            if let Some(start) = start {
                units.push(Au {
                    start,
                    end: *off,
                    has_vcl,
                });
            }
            start = Some(*off);
            has_vcl = false;
        } else if start.is_none() {
            start = Some(*off);
            has_vcl = vcl;
        } else if vcl {
            has_vcl = true;
        }
    }
    if let Some(start) = start {
        units.push(Au {
            start,
            end: data.len(),
            has_vcl,
        });
    }
    units
}

fn nal_ranges(data: &[u8]) -> Vec<(usize, usize, u8)> {
    let mut marks = Vec::new();
    let mut i = 0;
    while i + 3 < data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            let header = i + 3;
            if header < data.len() {
                marks.push((i, data[header] & 0x1f));
            }
            i += 3;
        } else if i + 4 < data.len()
            && data[i] == 0
            && data[i + 1] == 0
            && data[i + 2] == 0
            && data[i + 3] == 1
        {
            let header = i + 4;
            if header < data.len() {
                marks.push((i, data[header] & 0x1f));
            }
            i += 4;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::new();
    for (n, (off, nal_type)) in marks.iter().enumerate() {
        let end = marks.get(n + 1).map(|(next, _)| *next).unwrap_or(data.len());
        out.push((*off, end, *nal_type));
    }
    out
}

fn chunk_dir(source: &Path) -> PathBuf {
    let name = source
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("video");
    source.with_file_name(format!("{name}.dstep.chunks"))
}

fn extract_chunk(
    source: &Path,
    width: u32,
    height: u32,
    start_sec: f64,
    count: usize,
) -> Result<(Vec<u64>, Vec<Vec<u8>>)> {
    let frame_bytes = (width as usize)
        .saturating_mul(height as usize)
        .saturating_mul(3)
        / 2;
    let output = gentle_ffmpeg()
        .args([
            "-hide_banner",
            "-loglevel",
            "info",
            "-ss",
            &format!("{start_sec:.3}"),
            "-i",
        ])
        .arg(source)
        .args([
            "-frames:v",
            &count.to_string(),
            "-vf",
            "showinfo",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| Error::Other(format!("ffmpeg extract: {e}")))?;
    if !output.status.success() && output.stdout.len() < frame_bytes {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Other(format!("ffmpeg extract failed: {err}")));
    }
    let mut times = Vec::new();
    let err = String::from_utf8_lossy(&output.stderr);
    for line in err.lines() {
        if let Some(pos) = line.find("pts_time:") {
            let rest = line[pos + "pts_time:".len()..].trim();
            let token = rest.split_whitespace().next().unwrap_or("");
            if let Ok(ns) = parse_seconds_ns(token) {
                times.push(ns);
            }
        }
    }
    let mut pixels = Vec::new();
    let mut offset = 0;
    while offset + frame_bytes <= output.stdout.len() && pixels.len() < count {
        pixels.push(output.stdout[offset..offset + frame_bytes].to_vec());
        offset += frame_bytes;
    }
    if times.len() != pixels.len() {
        let frame_ns = if times.len() >= 2 {
            times[1].saturating_sub(times[0]).max(1)
        } else {
            100_000_000
        };
        let start_ns = (start_sec * 1_000_000_000.0) as u64;
        times = (0..pixels.len())
            .map(|index| start_ns.saturating_add(frame_ns.saturating_mul(index as u64)))
            .collect();
    }
    Ok((times, pixels))
}

fn gentle_ffmpeg() -> Command {
    let mut cmd = Command::new("nice");
    cmd.args(["-n", "19", "ffmpeg", "-threads", "1", "-filter_threads", "1"]);
    cmd
}

fn encode_reversed(chunk: &[(u64, Vec<u8>)], width: u32, height: u32) -> Result<Vec<u8>> {
    let dir = std::env::temp_dir().join(format!("dstep-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let raw_path = dir.join("chunk.yuv");
    let h264_path = dir.join("chunk.h264");
    {
        let mut raw = File::create(&raw_path)?;
        for (_pts, frame) in chunk.iter().rev() {
            raw.write_all(frame)?;
        }
    }
    let output = gentle_ffmpeg()
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "-s",
            &format!("{width}x{height}"),
            "-r",
            "30",
            "-i",
        ])
        .arg(&raw_path)
        .args([
            "-an",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-crf",
            "18",
            "-pix_fmt",
            "yuv420p",
            "-bf",
            "0",
            "-g",
            "16",
            "-keyint_min",
            "16",
            "-x264-params",
            "scenecut=0:open-gop=0:aud=1:bframes=0:threads=1:lookahead_threads=1",
            "-f",
            "h264",
        ])
        .arg(&h264_path)
        .output()
        .map_err(|e| Error::Other(format!("ffmpeg encode: {e}")))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Other(format!("ffmpeg encode failed: {err}")));
    }
    let bytes = std::fs::read(&h264_path)?;
    let _ = std::fs::remove_file(&raw_path);
    let _ = std::fs::remove_file(&h264_path);
    Ok(bytes)
}

fn source_pts(source: &Path) -> Result<Vec<u64>> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "packet=pts_time",
            "-of",
            "csv=p=0",
        ])
        .arg(source)
        .output()
        .map_err(|e| Error::Other(format!("ffprobe: {e}")))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Other(format!("ffprobe failed: {err}")));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut pts = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line == "N/A" {
            continue;
        }
        pts.push(parse_seconds_ns(line)?);
    }
    Ok(pts)
}

fn parse_seconds_ns(text: &str) -> Result<u64> {
    let (whole, frac) = text.split_once('.').unwrap_or((text, ""));
    let secs: u64 = whole
        .parse()
        .map_err(|_| Error::Other(format!("bad timestamp {text}")))?;
    let mut frac = frac.to_string();
    frac.truncate(9);
    while frac.len() < 9 {
        frac.push('0');
    }
    let nanos: u64 = frac
        .parse()
        .map_err(|_| Error::Other(format!("bad timestamp {text}")))?;
    Ok(secs.saturating_mul(1_000_000_000).saturating_add(nanos))
}

fn read_exact_or_eof(reader: &mut impl Read, buf: &mut [u8]) -> Result<bool> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) if filled == 0 => return Ok(false),
            Ok(0) => {
                return Err(Error::Other(
                    "decode ended in the middle of a frame".into(),
                ))
            }
            Ok(n) => filled += n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err.into()),
        }
    }
    Ok(true)
}

fn read_u32(file: &mut File) -> Result<u32> {
    let mut buf = [0u8; 4];
    file.read_exact(&mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

fn read_u64(file: &mut File) -> Result<u64> {
    let mut buf = [0u8; 8];
    file.read_exact(&mut buf)?;
    Ok(u64::from_le_bytes(buf))
}

fn write_u32(file: &mut File, value: u32) -> Result<()> {
    file.write_all(&value.to_le_bytes())?;
    Ok(())
}

fn write_u64(file: &mut File, value: u64) -> Result<()> {
    file.write_all(&value.to_le_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::access_units;

    #[test]
    fn splits_on_access_unit_delimiters() {
        // AUD, IDR slice, AUD, P slice. Each picture keeps its delimiter.
        let data = [
            0, 0, 0, 1, 9, 0x10, // AUD
            0, 0, 0, 1, 5, 0x88, // IDR
            0, 0, 0, 1, 9, 0x30, // AUD
            0, 0, 0, 1, 1, 0x9a, // non-IDR
        ];
        let units = access_units(&data);
        let vcl: Vec<_> = units.into_iter().filter(|unit| unit.has_vcl).collect();
        assert_eq!(vcl.len(), 2);
        assert!(vcl[0].has_vcl && vcl[1].has_vcl);
    }
}

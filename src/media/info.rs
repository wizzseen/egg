use std::path::Path;

use gstreamer::ClockTime;
use gstreamer_pbutils::prelude::*;
use gstreamer_pbutils::{Discoverer, DiscovererInfo};

use crate::error::{Error, Result};
use crate::media::streams::{caps_field_str, caps_label, friendly_caps_name};
use crate::ui::status::format_clock;

#[derive(Debug, Clone)]
pub struct VideoInfo {
    pub codec: String,
    pub width: u32,
    pub height: u32,
    pub fps: Option<f64>,
    pub pixel_format: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AudioInfo {
    pub codec: String,
    pub sample_rate: u32,
    pub channels: u32,
}

#[derive(Debug, Clone)]
pub struct MediaInfo {
    pub file: String,
    pub container: String,
    pub duration: Option<ClockTime>,
    pub video: Option<VideoInfo>,
    pub audio: Option<AudioInfo>,
}

impl MediaInfo {
    pub fn discover(path: &Path) -> Result<Self> {
        let abs = path
            .canonicalize()
            .map_err(|e| Error::OpenFile(format!("{}: {e}", path.display())))?;
        if !abs.is_file() {
            return Err(Error::OpenFile(format!("{} is not a file", abs.display())));
        }

        let uri = glib_filename_to_uri(&abs)?;
        let discoverer = Discoverer::new(ClockTime::from_seconds(15))
            .map_err(|e| Error::UnsupportedMedia(e.to_string()))?;
        let info = discoverer
            .discover_uri(&uri)
            .map_err(|e| Error::UnsupportedMedia(e.to_string()))?;

        Ok(from_discoverer(&abs, &info))
    }

    pub fn format(&self) -> String {
        let mut out = String::new();
        out.push_str("File:\n");
        out.push_str(&format!("  {}\n\n", self.file));
        out.push_str("Container:\n");
        out.push_str(&format!("  {}\n\n", self.container));
        out.push_str("Duration:\n");
        match self.duration {
            Some(d) => out.push_str(&format!("  {}\n\n", format_clock(d))),
            None => out.push_str("  unknown\n\n"),
        }

        match &self.video {
            Some(v) => {
                out.push_str("Video:\n");
                out.push_str(&format!("  Codec: {}\n", v.codec));
                out.push_str(&format!("  Resolution: {}x{}\n", v.width, v.height));
                match v.fps {
                    Some(fps) => out.push_str(&format!("  FPS: {fps:.2}\n")),
                    None => out.push_str("  FPS: unknown (do not infer frame times from this)\n"),
                }
                out.push_str(&format!(
                    "  Pixel Format: {}\n",
                    v.pixel_format.as_deref().unwrap_or("unknown")
                ));
            }
            None => out.push_str("Video:\n  (none)\n"),
        }

        out.push('\n');

        match &self.audio {
            Some(a) => {
                out.push_str("Audio:\n");
                out.push_str(&format!("  Codec: {}\n", a.codec));
                out.push_str(&format!("  Sample Rate: {} Hz\n", a.sample_rate));
                out.push_str(&format!("  Channels: {}\n", a.channels));
            }
            None => out.push_str("Audio:\n  (none)\n"),
        }

        out
    }

    pub fn fps_label(&self) -> String {
        match self.video.as_ref().and_then(|v| v.fps) {
            Some(fps) => format!("{fps:.2}"),
            None => "n/a".into(),
        }
    }
}

fn from_discoverer(path: &Path, info: &DiscovererInfo) -> MediaInfo {
    let container = info
        .stream_info()
        .and_then(|s| s.caps())
        .map(|c| caps_label(&c))
        .or_else(|| {
            info.stream_info()
                .map(|s| friendly_caps_name(&s.stream_type_nick()))
        })
        .unwrap_or_else(|| "unknown".into());

    let video = info.video_streams().into_iter().next().map(|v| {
        let caps = DiscovererStreamInfoExt::caps(&v);
        let codec = caps
            .as_ref()
            .map(caps_label)
            .unwrap_or_else(|| "unknown".into());
        let pixel_format = caps.as_ref().and_then(|c| caps_field_str(c, "format"));
        let fr = v.framerate();
        let fps = if fr.denom() != 0 {
            Some(fr.numer() as f64 / fr.denom() as f64)
        } else {
            None
        };
        VideoInfo {
            codec,
            width: v.width(),
            height: v.height(),
            fps,
            pixel_format,
        }
    });

    let audio = info.audio_streams().into_iter().next().map(|a| {
        let caps = DiscovererStreamInfoExt::caps(&a);
        let codec = caps
            .as_ref()
            .map(caps_label)
            .unwrap_or_else(|| "unknown".into());
        AudioInfo {
            codec,
            sample_rate: a.sample_rate(),
            channels: a.channels(),
        }
    });

    MediaInfo {
        file: path.display().to_string(),
        container,
        duration: info.duration(),
        video,
        audio,
    }
}

pub fn glib_filename_to_uri(path: &Path) -> Result<String> {
    gstreamer::glib::filename_to_uri(path, None)
        .map(|s| s.to_string())
        .map_err(|e| Error::OpenFile(e.to_string()))
}

use std::path::PathBuf;

use clap::Parser;
use gstreamer::ClockTime;

use crate::error::{Error, Result};

/// Rust Media Player — play almost any media file with bidirectional frame stepping.
#[derive(Parser, Debug)]
#[command(name = "egg", version, about, long_about = None)]
pub struct Args {
    /// Media file to play (or inspect with --info)
    pub file: PathBuf,

    /// Print media information and exit
    #[arg(long)]
    pub info: bool,

    /// Start position: seconds, MM:SS, or HH:MM:SS[.mmm]
    #[arg(long)]
    pub start: Option<String>,

    /// Volume (0-100)
    #[arg(long, default_value_t = 80, value_parser = clap::value_parser!(u8).range(0..=100))]
    pub volume: u8,

    /// Playback rate (for example 0.5, 1.0, 2.0)
    #[arg(long, default_value_t = 1.0)]
    pub speed: f64,

    /// Disable audio output
    #[arg(long)]
    pub no_audio: bool,

    /// Request a fullscreen video window if the sink supports it
    #[arg(long)]
    pub fullscreen: bool,

    /// Loop the file when it ends
    #[arg(long)]
    pub r#loop: bool,
}

impl Args {
    pub fn start_position(&self) -> Result<Option<ClockTime>> {
        match &self.start {
            Some(s) => Ok(Some(parse_time(s)?)),
            None => Ok(None),
        }
    }
}

/// Parse a CLI time into a GStreamer clock time.
pub fn parse_time(input: &str) -> Result<ClockTime> {
    let s = input.trim();
    if s.is_empty() {
        return Err(Error::other("empty time"));
    }

    if !s.contains(':') {
        let seconds: f64 = s
            .parse()
            .map_err(|_| Error::other(format!("invalid time: {s}")))?;
        if seconds < 0.0 {
            return Err(Error::other("time must be non-negative"));
        }
        return Ok(ClockTime::from_nseconds((seconds * 1_000_000_000.0) as u64));
    }

    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 2 && parts.len() != 3 {
        return Err(Error::other(format!("invalid time: {s}")));
    }

    let parse_f = |p: &str| -> Result<f64> {
        p.parse()
            .map_err(|_| Error::other(format!("invalid time: {s}")))
    };

    let (hours, minutes, seconds) = if parts.len() == 3 {
        (parse_f(parts[0])?, parse_f(parts[1])?, parse_f(parts[2])?)
    } else {
        (0.0, parse_f(parts[0])?, parse_f(parts[1])?)
    };

    if minutes >= 60.0 || seconds >= 60.0 || hours < 0.0 || minutes < 0.0 || seconds < 0.0 {
        return Err(Error::other(format!("invalid time: {s}")));
    }

    let total = hours * 3600.0 + minutes * 60.0 + seconds;
    Ok(ClockTime::from_nseconds((total * 1_000_000_000.0) as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_seconds() {
        let t = parse_time("1.5").unwrap();
        assert_eq!(t.nseconds(), 1_500_000_000);
    }

    #[test]
    fn parse_mm_ss() {
        let t = parse_time("01:02").unwrap();
        assert_eq!(t.nseconds(), 62_000_000_000);
    }

    #[test]
    fn parse_hh_mm_ss() {
        let t = parse_time("00:05:20.250").unwrap();
        assert_eq!(t.nseconds(), 320_250_000_000);
    }
}

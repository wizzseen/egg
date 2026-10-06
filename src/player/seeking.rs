use gstreamer::{ClockTime, SeekFlags, SeekType};

/// How a seek should snap.
///
/// `Normal` prefers a nearby keyframe (fast).
/// `Accurate` asks the pipeline to land on the requested timestamp.
/// `KeyUnitBefore` is the frame-stepper seek: previous keyframe at or before PTS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeekMode {
    Normal,
    Accurate,
    KeyUnitBefore,
}

impl SeekMode {
    pub fn flags(self) -> SeekFlags {
        match self {
            Self::Normal => SeekFlags::FLUSH | SeekFlags::KEY_UNIT,
            Self::Accurate => SeekFlags::FLUSH | SeekFlags::ACCURATE,
            Self::KeyUnitBefore => {
                SeekFlags::FLUSH | SeekFlags::KEY_UNIT | SeekFlags::SNAP_BEFORE
            }
        }
    }
}

/// Build the common (rate, flags, start=pos, stop=none) seek tuple pieces.
pub fn seek_args(
    mode: SeekMode,
    rate: f64,
    position: ClockTime,
) -> (f64, SeekFlags, SeekType, ClockTime, SeekType, Option<ClockTime>) {
    (
        rate,
        mode.flags(),
        SeekType::Set,
        position,
        SeekType::None,
        ClockTime::NONE,
    )
}

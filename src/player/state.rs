/// Explicit player state machine. Playback, seeking, and frame stepping
/// must go through this rather than scattering booleans.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerState {
    Loading,
    Playing,
    Paused,
    Seeking,
    FrameStepping,
    Ended,
    Error,
}

impl PlayerState {
    pub fn as_label(self) -> &'static str {
        match self {
            Self::Loading => "LOADING",
            Self::Playing => "PLAYING",
            Self::Paused => "PAUSED",
            Self::Seeking => "SEEKING",
            Self::FrameStepping => "STEPPING",
            Self::Ended => "ENDED",
            Self::Error => "ERROR",
        }
    }
}

use std::fmt::Display;

use gstreamer::glib;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Could not open file: {0}")]
    OpenFile(String),
    #[error("Unsupported media: {0}")]
    UnsupportedMedia(String),
    #[error("Could not initialize GStreamer: {0}")]
    GStreamerInit(String),
    #[error("Could not create video sink: {0}")]
    VideoSink(String),
    #[error("Seek failed: {0}")]
    Seek(String),
    #[error("Frame stepping failed: {0}")]
    FrameStep(String),
    #[error("Could not initialize player: {0}")]
    PlayerInit(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

impl Error {
    pub fn other(msg: impl Display) -> Self {
        Self::Other(msg.to_string())
    }
}

impl From<glib::BoolError> for Error {
    fn from(value: glib::BoolError) -> Self {
        Self::Other(value.to_string())
    }
}

impl From<glib::Error> for Error {
    fn from(value: glib::Error) -> Self {
        Self::Other(value.to_string())
    }
}

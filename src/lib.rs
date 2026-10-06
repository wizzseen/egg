//! egg media player library surface.
//!
//! The binary is a thin CLI around [`player::Player`]. Tests use the same
//! types with `PlayerOptions::headless`.

pub mod cli;
pub mod error;
pub mod input;
pub mod media;
pub mod player;
pub mod ui;

pub use error::{Error, Result};
pub use player::{Player, PlayerOptions};

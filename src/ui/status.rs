use std::io::{stdout, Write};

use crossterm::cursor;
use crossterm::execute;
use crossterm::style::{Color, Print, ResetColor, SetForegroundColor};
use crossterm::terminal::{self, Clear, ClearType};
use gstreamer::ClockTime;

use crate::error::Result;
use crate::player::state::PlayerState;

#[derive(Debug, Clone)]
pub struct StatusSnapshot {
    pub state: PlayerState,
    pub position: Option<ClockTime>,
    pub duration: Option<ClockTime>,
    pub frame_index: Option<u64>,
    pub pts: Option<ClockTime>,
    pub fps_label: String,
    pub speed: f64,
    pub volume: u8,
    pub looping: bool,
    pub stepping: bool,
    pub info_overlay: Option<String>,
}

pub fn format_clock(t: ClockTime) -> String {
    let ns = t.nseconds();
    let total_ms = ns / 1_000_000;
    let ms = total_ms % 1000;
    let total_s = total_ms / 1000;
    let s = total_s % 60;
    let total_m = total_s / 60;
    let m = total_m % 60;
    let h = total_m / 60;
    format!("{h:02}:{m:02}:{s:02}.{ms:03}")
}

pub fn format_clock_opt(t: Option<ClockTime>) -> String {
    t.map(format_clock).unwrap_or_else(|| "--:--:--.---".into())
}

/// One line for the on-screen control bar.
pub fn bar_text(status: &StatusSnapshot) -> String {
    let loop_flag = if status.looping { "  loop" } else { "" };
    let info = if status.info_overlay.is_some() {
        "  info"
    } else {
        ""
    };
    format!(
        "{}  {} / {}  {:.2}x  vol {}%{loop_flag}{info}",
        status.state.as_label(),
        format_clock_opt(status.position),
        format_clock_opt(status.duration),
        status.speed,
        status.volume,
    )
}

/// Draw a compact status block at the bottom of the terminal.
pub fn draw(status: &StatusSnapshot) -> Result<()> {
    let mut out = stdout();
    let (_cols, rows) = terminal::size()?;
    let height = if status.info_overlay.is_some() { 16 } else { 6 };
    let row = rows.saturating_sub(height);

    execute!(
        out,
        cursor::SavePosition,
        cursor::MoveTo(0, row),
        Clear(ClearType::FromCursorDown),
    )?;

    let icon = match status.state {
        PlayerState::Playing => "▶",
        PlayerState::FrameStepping => {
            if status.stepping {
                "◀"
            } else {
                "▶"
            }
        }
        PlayerState::Paused | PlayerState::Seeking => "⏸",
        PlayerState::Ended => "■",
        PlayerState::Error => "!",
        PlayerState::Loading => "…",
    };

    let pos = format_clock_opt(status.position);
    let dur = format_clock_opt(status.duration);
    let frame = status
        .frame_index
        .map(|i| i.to_string())
        .unwrap_or_else(|| "?".into());
    let pts = format_clock_opt(status.pts);
    let loop_flag = if status.looping { " LOOP" } else { "" };

    execute!(
        out,
        SetForegroundColor(Color::Cyan),
        Print(format!("{icon}  {pos} / {dur}{loop_flag}\r\n")),
        ResetColor,
        Print(format!(
            "Frame: {frame}    PTS: {pts}    FPS: {}\r\n",
            status.fps_label
        )),
        Print(format!(
            "Speed: {:.2}x    Volume: {}%    State: {}\r\n",
            status.speed,
            status.volume,
            status.state.as_label()
        )),
    )?;

    if status.stepping {
        execute!(
            out,
            SetForegroundColor(Color::Yellow),
            Print(format!("FRAME STEP    PTS: {pts}    Frame: {frame}\r\n")),
            ResetColor,
        )?;
    }

    if let Some(info) = &status.info_overlay {
        execute!(out, Print("\r\n"), Print(info.replace('\n', "\r\n")))?;
    }

    execute!(out, cursor::RestorePosition)?;
    out.flush()?;
    Ok(())
}

pub fn restore_terminal() {
    let mut out = stdout();
    let _ = execute!(out, cursor::Show, Clear(ClearType::CurrentLine), ResetColor);
}

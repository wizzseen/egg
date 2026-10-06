use std::io::{stdout, Write};
use std::time::Duration;

use clap::Parser;
use crossterm::cursor;
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, Clear, ClearType};
use gstreamer::ClockTime;

use egg::cli::Args;
use egg::error::{Error, Result};
use egg::input::poll_command;
use egg::media::MediaInfo;
use egg::player::{Player, PlayerOptions};
use egg::ui::{self, restore_terminal};

fn main() {
    if let Err(err) = run() {
        let _ = disable_raw_mode();
        restore_terminal();
        eprintln!("{err}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    gstreamer::init().map_err(|e| Error::GStreamerInit(e.to_string()))?;

    let args = Args::parse();

    if args.info {
        let info = MediaInfo::discover(&args.file)?;
        print!("{}", info.format());
        return Ok(());
    }

    let options = PlayerOptions {
        start: args.start_position()?,
        volume: args.volume,
        speed: args.speed,
        no_audio: args.no_audio,
        fullscreen: args.fullscreen,
        looping: args.r#loop,
        headless: false,
        start_paused: false,
    };

    let mut player = Player::open(&args.file, options)?;

    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, cursor::Hide)?;
    stdout.flush()?;

    let result = event_loop(&mut player);
    let _ = player.shutdown();

    let _ = disable_raw_mode();
    let _ = execute!(stdout, cursor::Show, Clear(ClearType::FromCursorDown));
    restore_terminal();
    result
}

fn event_loop(player: &mut Player) -> Result<()> {
    ui::draw(&player.status())?;
    player.refresh_controls();
    player.mark_clean();

    loop {
        player.poll_bus(ClockTime::from_mseconds(20))?;
        player.pump_window();
        player.poll_hm_job();

        let mut quit = false;
        while let Some(cmd) = player.poll_window_command() {
            if !player.handle(cmd)? {
                quit = true;
                break;
            }
        }
        if quit {
            break;
        }

        if let Some(cmd) = poll_command(Duration::from_millis(20))? {
            if !player.handle(cmd)? {
                break;
            }
        }

        if player.should_redraw() {
            ui::draw(&player.status())?;
            player.refresh_controls();
            player.mark_clean();
        }
    }
    Ok(())
}

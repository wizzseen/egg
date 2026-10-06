/// Commands issued by the CLI startup path or the keyboard loop.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Command {
    PlayPause,
    StepForward(u64),
    StepBackward(u64),
    VolumeUp,
    VolumeDown,
    SpeedUp,
    SpeedDown,
    SpeedReset,
    ToggleLoop,
    ShowInfo,
    /// Write a `.hm` backward track beside the current file. The source is kept.
    SaveHm,
    /// Jump to a fraction of the file, 0 at the start and 1 at the end.
    SeekFraction(f64),
    Quit,
}

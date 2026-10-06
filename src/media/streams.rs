use gstreamer::Caps;

/// Friendly codec / container label from a caps name such as `video/x-h264`.
pub fn caps_label(caps: &Caps) -> String {
    let Some(s) = caps.structure(0) else {
        return "unknown".to_string();
    };
    friendly_caps_name(s.name().as_str())
}

pub fn friendly_caps_name(name: &str) -> String {
    match name {
        "video/x-h264" => "H.264".into(),
        "video/x-h265" | "video/x-hevc" => "H.265 / HEVC".into(),
        "video/x-vp8" => "VP8".into(),
        "video/x-vp9" => "VP9".into(),
        "video/x-av1" => "AV1".into(),
        "video/x-theora" => "Theora".into(),
        "video/mpeg" => "MPEG video".into(),
        "video/x-raw" => "Raw video".into(),
        "audio/mpeg" => "MPEG audio".into(),
        "audio/x-aac" => "AAC".into(),
        "audio/x-opus" => "Opus".into(),
        "audio/x-vorbis" => "Vorbis".into(),
        "audio/x-flac" => "FLAC".into(),
        "audio/x-wav" | "audio/x-raw" => "PCM / raw audio".into(),
        "video/quicktime" => "QuickTime / MP4".into(),
        "video/x-matroska" => "Matroska".into(),
        "application/x-matroska" => "Matroska".into(),
        "video/webm" => "WebM".into(),
        "video/x-msvideo" => "AVI".into(),
        "application/ogg" => "Ogg".into(),
        other => other.to_string(),
    }
}

pub fn caps_field_str(caps: &Caps, field: &str) -> Option<String> {
    let s = caps.structure(0)?;
    s.get::<String>(field)
        .ok()
        .or_else(|| s.get::<&str>(field).ok().map(|v| v.to_string()))
}

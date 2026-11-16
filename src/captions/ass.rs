//! ASS (Advanced SubStation Alpha) caption generation: karaoke word highlighting (`\k`),
//! font/outline styling, and bottom-centre placement with a safe area for 16:9 and 9:16 canvases.
//! Generation is pure and unit-tested. Burn-in uses ffmpeg's `subtitles` filter, which needs
//! ffmpeg built with libass (checked by `burn_subtitle_file`, which reports ffmpeg's own error).

use std::path::Path;

use crate::error::{Error, ErrorCode, Result};
use crate::util::proc::{self, ProcOpts};

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct AssWord {
    pub text: String,
    pub start: f64,
    pub end: f64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct AssLine {
    pub start: f64,
    pub end: f64,
    pub text: String,
    /// Word timings for karaoke; empty when the engine gave only segment times.
    pub words: Vec<AssWord>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// 16:9 and other landscape canvases: bottom centre, small bottom margin.
    Landscape,
    /// 9:16 canvases: bottom centre, raised clear of platform UI (safe area).
    Vertical,
}

impl Layout {
    pub fn for_canvas(width: u32, height: u32) -> Layout {
        if height > width {
            Layout::Vertical
        } else {
            Layout::Landscape
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AssStyle {
    pub font: String,
    /// Font size in pixels; 0 picks a size from the canvas height.
    pub size: u32,
    /// Colour of already-spoken text (karaoke sweep target), e.g. `#FFD400`.
    pub highlight: String,
    /// Colour of text not yet spoken, and of all text when `word_highlight` is off.
    pub base: String,
    pub outline: String,
    pub outline_width: u32,
    pub bold: bool,
    pub word_highlight: bool,
}

impl Default for AssStyle {
    fn default() -> Self {
        AssStyle {
            font: "Arial".into(),
            size: 0,
            highlight: "#FFD400".into(),
            base: "#FFFFFF".into(),
            outline: "#000000".into(),
            outline_width: 3,
            bold: true,
            word_highlight: true,
        }
    }
}

/// ASS colour `&H00BBGGRR` from `#RRGGBB` or a basic colour name.
pub fn ass_colour(spec: &str) -> Result<String> {
    let s = spec.trim();
    let hex = match s.to_ascii_lowercase().as_str() {
        "white" => "FFFFFF",
        "black" => "000000",
        "yellow" => "FFFF00",
        "red" => "FF0000",
        "green" => "00FF00",
        "blue" => "0000FF",
        "cyan" => "00FFFF",
        _ => s.strip_prefix('#').unwrap_or(s),
    };
    if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(Error::invalid(format!(
            "colour '{spec}' must be #RRGGBB or one of white, black, yellow, red, green, blue, cyan"
        )));
    }
    let (r, g, b) = (&hex[0..2], &hex[2..4], &hex[4..6]);
    Ok(format!("&H00{b}{g}{r}").to_ascii_uppercase())
}

/// ASS timestamp `H:MM:SS.cc` (centiseconds).
pub fn ass_time(t: f64) -> String {
    let cs = (t.max(0.0) * 100.0).round() as u64;
    format!(
        "{}:{:02}:{:02}.{:02}",
        cs / 360_000,
        cs / 6000 % 60,
        cs / 100 % 60,
        cs % 100
    )
}

struct Geometry {
    font_size: u32,
    margin_v: u32,
    margin_lr: u32,
}

fn geometry(width: u32, height: u32, layout: Layout, style: &AssStyle) -> Geometry {
    let (w, h) = (width as f64, height as f64);
    let (size_factor, margin_v, margin_lr) = match layout {
        Layout::Landscape => (0.05, (h * 0.06).round(), (w * 0.04).round()),
        // 18% of the height above the bottom edge keeps captions clear of platform overlays.
        Layout::Vertical => (0.04, (h * 0.18).round(), (w * 0.08).round()),
    };
    let font_size = if style.size > 0 {
        style.size
    } else {
        (h * size_factor).round() as u32
    };
    Geometry {
        font_size,
        margin_v: margin_v as u32,
        margin_lr: margin_lr as u32,
    }
}

/// Make text safe inside an ASS event: braces and backslashes start override tags, so they are
/// replaced; whitespace is collapsed.
fn escape(text: &str) -> String {
    let replaced: String = text
        .chars()
        .map(|c| match c {
            '{' => '(',
            '}' => ')',
            '\\' => '/',
            _ => c,
        })
        .collect();
    replaced.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Event body. With word highlighting, each word carries `\k` (centiseconds) until the next
/// word starts, so gaps between words are covered and the line's total equals its duration.
fn line_body(line: &AssLine, style: &AssStyle, base: &str) -> String {
    let usable: Vec<&AssWord> = line
        .words
        .iter()
        .filter(|w| !escape(&w.text).is_empty() && w.end >= w.start)
        .collect();
    if style.word_highlight && !usable.is_empty() {
        let parts: Vec<String> = usable
            .iter()
            .enumerate()
            .map(|(i, w)| {
                let next = usable
                    .get(i + 1)
                    .map(|n| n.start)
                    .unwrap_or(line.end)
                    .max(w.start);
                let cs = (((next - w.start) * 100.0).round() as i64).max(1);
                format!("{{\\k{cs}}}{}", escape(&w.text))
            })
            .collect();
        parts.join(" ")
    } else {
        format!("{{\\1c{base}&}}{}", escape(&line.text))
    }
}

/// Serialise caption lines as an ASS script for a `width` x `height` canvas.
pub fn generate_ass(
    lines: &[AssLine],
    width: u32,
    height: u32,
    layout: Layout,
    style: &AssStyle,
) -> Result<String> {
    if width == 0 || height == 0 {
        return Err(Error::invalid("the caption canvas must have a non-zero size"));
    }
    let g = geometry(width, height, layout, style);
    let highlight = ass_colour(&style.highlight)?;
    let base = ass_colour(&style.base)?;
    let outline = ass_colour(&style.outline)?;
    let bold = if style.bold { -1 } else { 0 };
    let mut out = String::new();
    out.push_str("[Script Info]\nScriptType: v4.00+\n");
    out.push_str(&format!("PlayResX: {width}\nPlayResY: {height}\n"));
    out.push_str("WrapStyle: 0\nScaledBorderAndShadow: yes\n\n");
    out.push_str("[V4+ Styles]\n");
    out.push_str("Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n");
    out.push_str(&format!(
        "Style: Default,{},{},{},{},{},&H64000000,{},0,0,0,100,100,0,0,1,{},0,2,{},{},{},1\n\n",
        style.font,
        g.font_size,
        highlight,
        base,
        outline,
        bold,
        style.outline_width,
        g.margin_lr,
        g.margin_lr,
        g.margin_v
    ));
    out.push_str("[Events]\n");
    out.push_str("Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n");
    for line in lines {
        if line.end <= line.start || escape(&line.text).is_empty() && line.words.is_empty() {
            continue;
        }
        out.push_str(&format!(
            "Dialogue: 0,{},{},Default,,0,0,0,,{}\n",
            ass_time(line.start),
            ass_time(line.end),
            line_body(line, style, &base)
        ));
    }
    Ok(out)
}

/// Escape a path for use inside a single-quoted ffmpeg filter argument.
fn escape_filter_path(p: &Path) -> String {
    p.to_string_lossy()
        .replace('\\', "\\\\")
        .replace(':', "\\:")
        .replace('\'', "\\'")
}

/// The `subtitles` filter argument for a subtitle file.
///
/// An apostrophe cannot be escaped into the filtergraph's `'...'` quoting: ffmpeg 6.1.1 drops it
/// and then fails to open `.../its/sub.srt`, so burn-in used to fail outright on such a project.
/// A path the filtergraph cannot carry is therefore staged to a quote-free name first.
fn subtitle_filter_value(subtitle: &Path) -> Result<String> {
    let path = if crate::util::paths::filtergraph_inexpressible(subtitle) {
        crate::util::paths::stage_for_filtergraph(subtitle)?
    } else {
        subtitle.to_path_buf()
    };
    Ok(format!("subtitles=filename='{}'", escape_filter_path(&path)))
}

/// Burn a subtitle file (ASS or SRT) into `input`, writing `output`. Uses the `subtitles`
/// filter (libass); video is re-encoded, audio is copied.
pub fn burn_subtitle_file(input: &Path, subtitle: &Path, output: &Path) -> Result<()> {
    let ffmpeg = proc::which("ffmpeg")
        .ok_or_else(|| Error::new(ErrorCode::DependencyMissing, "ffmpeg is not on PATH".to_string()))?;
    let args: Vec<String> = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-y".into(),
        "-i".into(),
        input.to_string_lossy().to_string(),
        "-vf".into(),
        subtitle_filter_value(subtitle)?,
        "-c:v".into(),
        "libx264".into(),
        "-preset".into(),
        "medium".into(),
        "-crf".into(),
        "18".into(),
        "-pix_fmt".into(),
        "yuv420p".into(),
        "-c:a".into(),
        "copy".into(),
        output.to_string_lossy().to_string(),
    ];
    let out = proc::run(&ffmpeg, &args, &ProcOpts::default().with_timeout(3600))?;
    if !out.success {
        return Err(Error::new(
            ErrorCode::Render,
            format!(
                "subtitle burn-in failed (ffmpeg needs the libass subtitles filter): {}",
                crate::secrets::redact(&out.tail_stderr(4))
            ),
        ));
    }
    Ok(())
}

/// Write an ASS track for `lines` next to `video` and burn it in place (via a temporary file).
pub fn burn_track(
    lines: &[AssLine],
    width: u32,
    height: u32,
    style: &AssStyle,
    video: &Path,
) -> Result<()> {
    if lines.is_empty() {
        return Ok(());
    }
    let ass_path = video.with_extension("captions.ass");
    let script = generate_ass(lines, width, height, Layout::for_canvas(width, height), style)?;
    std::fs::write(&ass_path, script)?;
    let tmp = video.with_extension("captioned.tmp.mp4");
    burn_subtitle_file(video, &ass_path, &tmp)?;
    std::fs::rename(&tmp, video)
        .map_err(|e| Error::io(format!("cannot replace {}: {e}", video.display())))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(text: &str, start: f64, end: f64) -> AssWord {
        AssWord {
            text: text.into(),
            start,
            end,
        }
    }

    fn karaoke_line() -> AssLine {
        AssLine {
            start: 0.0,
            end: 1.2,
            text: "Hello world".into(),
            words: vec![word("Hello", 0.0, 0.4), word("world", 0.6, 1.0)],
        }
    }

    #[test]
    fn karaoke_tags_cover_each_word_and_the_gaps() {
        let ass = generate_ass(
            &[karaoke_line()],
            1920,
            1080,
            Layout::Landscape,
            &AssStyle::default(),
        )
        .unwrap();
        // Hello runs until world starts (0.6 s); world runs to the line end (1.2 s).
        assert!(ass.contains("{\\k60}Hello {\\k60}world"), "{ass}");
        let dialogue = ass.lines().find(|l| l.starts_with("Dialogue:")).unwrap();
        assert!(dialogue.starts_with("Dialogue: 0,0:00:00.00,0:00:01.20,Default,"), "{dialogue}");
        let total: i64 = dialogue
            .split("{\\k")
            .skip(1)
            .map(|s| s.split('}').next().unwrap().parse::<i64>().unwrap())
            .sum();
        assert_eq!(total, 120, "karaoke durations must sum to the line length");
    }

    #[test]
    fn plain_text_uses_base_colour_when_highlight_is_off() {
        let style = AssStyle {
            word_highlight: false,
            ..AssStyle::default()
        };
        let ass = generate_ass(&[karaoke_line()], 1920, 1080, Layout::Landscape, &style).unwrap();
        assert!(ass.contains("{\\1c&H00FFFFFF&}Hello world"), "{ass}");
        assert!(!ass.contains("\\k"));
    }

    #[test]
    fn vertical_canvas_gets_a_safe_area_and_landscape_does_not() {
        let line = karaoke_line();
        let style = AssStyle::default();
        let vertical = generate_ass(&[line.clone()], 1080, 1920, Layout::for_canvas(1080, 1920), &style).unwrap();
        let landscape = generate_ass(&[line], 1920, 1080, Layout::for_canvas(1920, 1080), &style).unwrap();
        assert!(vertical.contains("PlayResX: 1080\nPlayResY: 1920"));
        // Vertical: MarginL/R = 8% of width (86), MarginV = 18% of height (346); bottom centre.
        assert!(vertical.contains(",86,86,346,1\n"), "{vertical}");
        assert!(landscape.contains(",77,77,65,1\n") || landscape.contains(",77,77,65,1"), "{landscape}");
        assert_eq!(Layout::for_canvas(1920, 1080), Layout::Landscape);
        assert_eq!(Layout::for_canvas(1080, 1920), Layout::Vertical);
        // Alignment 2 = bottom centre in both layouts.
        assert!(vertical.contains(",2,86,86,346,1"));
        assert!(landscape.contains(",2,77,77,65,1"));
    }

    #[test]
    fn styling_is_applied_and_colours_are_bgr() {
        let style = AssStyle {
            font: "DejaVu Sans".into(),
            size: 72,
            highlight: "#FFD400".into(),
            outline_width: 5,
            bold: false,
            ..AssStyle::default()
        };
        let ass = generate_ass(&[karaoke_line()], 1280, 720, Layout::Landscape, &style).unwrap();
        // #FFD400 is RGB; ASS stores BGR: &H0000D4FF.
        assert!(ass.contains("Style: Default,DejaVu Sans,72,&H0000D4FF,&H00FFFFFF,&H00000000,&H64000000,0,"), "{ass}");
        assert!(ass.contains(",0,0,0,100,100,0,0,1,5,0,2,"), "{ass}");
        assert_eq!(ass_colour("yellow").unwrap(), "&H0000FFFF");
        assert!(ass_colour("#12345").is_err());
        assert!(ass_colour("nope").is_err());
    }

    #[test]
    fn timestamps_use_centiseconds() {
        assert_eq!(ass_time(3723.456), "1:02:03.46");
        assert_eq!(ass_time(0.0), "0:00:00.00");
        assert_eq!(ass_time(-1.0), "0:00:00.00");
    }

    #[test]
    fn override_characters_in_speech_cannot_break_the_script() {
        let line = AssLine {
            start: 0.0,
            end: 1.0,
            text: "{\\an8}evil\\N text}".into(),
            words: vec![],
        };
        let ass = generate_ass(&[line], 1280, 720, Layout::Landscape, &AssStyle::default()).unwrap();
        let dialogue = ass.lines().find(|l| l.starts_with("Dialogue:")).unwrap();
        let body = dialogue.splitn(10, ',').last().unwrap();
        assert_eq!(body.matches('{').count(), 1, "only our own tag: {body}");
        assert!(body.contains("(\\an8)") || body.contains("(/an8)"), "{body}");
    }

    #[test]
    fn empty_and_backwards_lines_are_skipped() {
        let lines = vec![
            AssLine { start: 2.0, end: 1.0, text: "backwards".into(), words: vec![] },
            AssLine { start: 0.0, end: 1.0, text: "   ".into(), words: vec![] },
            karaoke_line(),
        ];
        let ass = generate_ass(&lines, 1280, 720, Layout::Landscape, &AssStyle::default()).unwrap();
        assert_eq!(ass.lines().filter(|l| l.starts_with("Dialogue:")).count(), 1);
    }

    #[test]
    fn an_apostrophe_subtitle_path_is_staged_for_the_filtergraph() {
        let dir = tempfile::tempdir().unwrap();
        let subdir = dir.path().join("owner's cut");
        std::fs::create_dir_all(&subdir).unwrap();
        let srt = subdir.join("captions.srt");
        std::fs::write(&srt, "1\n00:00:00,000 --> 00:00:01,000\nhello\n").unwrap();

        let value = subtitle_filter_value(&srt).unwrap();
        assert!(value.starts_with("subtitles=filename='"), "{value}");
        assert_eq!(
            value.matches('\'').count(),
            2,
            "only the wrapping quotes remain: {value}"
        );
        assert!(value.contains("filterpaths"), "{value}");
        // The staged copy holds the script that was asked for.
        let staged = value
            .trim_start_matches("subtitles=filename='")
            .trim_end_matches('\'');
        assert_eq!(std::fs::read_to_string(staged).unwrap(), std::fs::read_to_string(&srt).unwrap());
    }

    #[test]
    fn expressible_subtitle_paths_are_named_directly() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["With Spaces", "a,b", "hellö", "视频"] {
            let subdir = dir.path().join(name);
            std::fs::create_dir_all(&subdir).unwrap();
            let srt = subdir.join("captions.srt");
            std::fs::write(&srt, "1\n").unwrap();
            let value = subtitle_filter_value(&srt).unwrap();
            assert!(!value.contains("filterpaths"), "{name} keeps its path: {value}");
            assert_eq!(value.matches('\'').count(), 2, "{value}");
        }
    }

    #[test]
    fn escape_filter_path_quotes_special_characters() {
        assert_eq!(escape_filter_path(Path::new("/tmp/a:b'c.ass")), "/tmp/a\\:b\\'c.ass");
    }
}

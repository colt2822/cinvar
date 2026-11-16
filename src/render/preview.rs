//! `--pro-preview`: a Pro-only operation run for real, shortened and visibly watermarked.
//!
//! The Free edition refuses what it does not include. `--pro-preview` does not lift that limit
//! silently: it runs the *real* capability at the *real* settings, but only for the first
//! [`crate::edition::PRO_PREVIEW_SECS`] seconds and with the notice burn-in below, so the output
//! is a demonstration, never a substitute for a license. An operation that produces an asset
//! rather than a video (a timeline export, a LUT) gets a sidecar marker file instead
//! ([`write_marker`]), which travels with the file and says the same thing.

use std::path::{Path, PathBuf};

use crate::error::Result;

use super::fonts;

/// The text burned into a previewed video (also the storefront price).
pub const OVERLAY: &str = crate::edition::PRO_PREVIEW_OVERLAY;

/// Escapes a constant for filtergraph single-quote context. The notice is fixed text, but it is
/// escaped anyway: an unescaped quote or colon in a future wording would silently break renders.
fn escape_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str(r"\\"),
            '\'' => out.push_str(r"\'"),
            ':' => out.push_str(r"\:"),
            '%' => out.push_str(r"\%"),
            _ => out.push(ch),
        }
    }
    out
}

/// drawtext spec burning the preview notice across the lower third. Deliberately subtle:
/// small, semi-transparent, outlined so it survives a bright frame, and never covering a face.
pub(super) fn overlay_filter(width: u32, height: u32) -> String {
    let sf = (width.min(height) as f64 / 1080.0).clamp(0.35, 4.0);
    let size = ((height as f64 * 0.026).round() as u32).clamp(14, 120);
    let pad = (height as f64 * 0.035).round().max(8.0) as u32;
    let border = ((2.0 * sf).round() as u32).max(1);
    let mut spec = format!(
        "drawtext=text='{}':fontsize={size}:fontcolor=white@0.62:borderw={border}:bordercolor=black@0.35",
        escape_text(OVERLAY)
    );
    // An explicit font file keeps drawtext away from fontconfig (which can abort on Windows).
    if let Some(fonts::FontChoice::File(path)) = fonts::drawtext_font(None) {
        if let Some(quoted) = fonts::fontfile_option(&path) {
            spec.push_str(&format!(":fontfile={quoted}"));
        }
    }
    spec.push_str(&format!(":x=(w-text_w)/2:y=h-text_h-{pad}"));
    spec
}

/// The bare drawtext spec, for callers outside `render` that reassemble with `-vf` (the
/// upscaler) instead of building a filter_complex graph.
pub fn drawtext_spec(width: u32, height: u32) -> String {
    overlay_filter(width, height)
}

/// Appends the overlay to a finished video graph: the last chain re-labels the video stream, so
/// the render encodes the watermarked frames directly (no second pass, no loss).
pub(super) fn overlay_graph(
    filtergraph: &str,
    video_label: &str,
    width: u32,
    height: u32,
) -> (String, String) {
    let out = "propreview".to_string();
    let graph = format!(
        "{filtergraph};[{video_label}]{}[{out}]",
        overlay_filter(width, height)
    );
    (graph, out)
}

/// Sidecar marker for a previewed *asset* (something that is not a video). Written next to the
/// asset so a copied folder carries the notice with it.
pub fn write_marker(asset: &Path) -> Result<PathBuf> {
    let mut name = asset
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "asset".to_string());
    name.push_str(".pro-preview.txt");
    let path = asset.with_file_name(name);
    let body = format!(
        "{}\n\nThis file was produced by a CINVAR Pro preview (--pro-preview).\n\
         It is the real output of the Pro capability, marked as a preview.\n\
         Buy CINVAR Pro to drop the marker: {}\n",
        OVERLAY,
        crate::edition::UPGRADE_URL
    );
    crate::util::paths::write_atomic(&path, body.as_bytes())?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_is_quoted_and_positioned() {
        let spec = overlay_filter(3840, 2160);
        assert!(spec.starts_with("drawtext=text='CINVAR PRO PREVIEW - Unlock for $29.99'"));
        assert!(spec.contains(":x=(w-text_w)/2"), "{spec}");
        assert!(spec.contains(":y=h-text_h-"), "{spec}");
        assert!(!spec.contains(":bordercolor= "), "{spec}");
        assert!(spec.contains(":fontcolor=white@0.62"), "{spec}");
    }

    #[test]
    fn overlay_relabels_the_video_stream() {
        let (graph, label) = overlay_graph("color=black[vout]", "vout", 1920, 1080);
        assert!(graph.ends_with("[propreview]"), "{graph}");
        assert!(graph.contains("[vout]drawtext="), "{graph}");
        assert_eq!(label, "propreview");
    }

    #[test]
    fn marker_sits_next_to_the_asset() {
        let dir = tempfile::tempdir().unwrap();
        let asset = dir.path().join("cut.otio");
        std::fs::write(&asset, "{}").unwrap();
        let marker = write_marker(&asset).unwrap();
        assert_eq!(marker.file_name().unwrap(), "cut.otio.pro-preview.txt");
        let body = std::fs::read_to_string(&marker).unwrap();
        assert!(body.contains("CINVAR PRO PREVIEW"), "{body}");
        assert!(body.contains(crate::edition::UPGRADE_URL), "{body}");
    }
}

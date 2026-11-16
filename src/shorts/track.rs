//! Final delivery of a Short through the existing render pipeline (crate::render).
//!
//! The vertical master (already 1080x1920, captions burned in) is imported into a scratch
//! project as a single clip on a 1080x1920 timeline and rendered by crate::render, so the
//! delivered file gets the same encoder selection, loudness normalisation, capability
//! preflight, output validation and atomic rename as every other CINVAR export.

use std::path::Path;

use crate::error::{Error, Result};
use crate::project::ops;
use crate::project::store::ProjectStore;
use crate::render::{self, RenderRequest};
use crate::shorts::vertical::{OUT_HEIGHT, OUT_WIDTH};
use crate::util::time::{fps_from_f64, Fps};

/// Frame rate used when the master's own rate cannot be read.
const FALLBACK_FPS: f64 = 30.0;

fn source_fps(master: &Path) -> Fps {
    crate::qa::probe::probe(master)
        .ok()
        .and_then(|probe| probe.fps)
        .filter(|fps| *fps >= 1.0 && *fps <= 240.0)
        .map(fps_from_f64)
        .unwrap_or_else(|| fps_from_f64(FALLBACK_FPS))
}

/// Render master into output at 1080x1920. Returns the measured frame size.
pub fn render_vertical(master: &Path, output: &Path, work_dir: &Path) -> Result<(u32, u32)> {
    if !master.is_file() {
        return Err(Error::invalid(format!(
            "nothing to render: {} does not exist",
            master.display()
        )));
    }
    let scratch = work_dir.join("render-project");
    std::fs::create_dir_all(&scratch)
        .map_err(|e| Error::io(format!("cannot create {}: {e}", scratch.display())))?;
    let (store, mut project) = ProjectStore::create(&scratch, "short")?;

    let asset = store.import_asset(&mut project, master, "shorts")?;
    if asset.info.as_ref().map(|i| i.duration.nanos <= 0).unwrap_or(true) {
        return Err(Error::invalid(format!(
            "the vertical master {} has no measurable duration",
            master.display()
        )));
    }
    let fps = source_fps(master);
    let timeline = ops::create_timeline(&mut project, "Short", OUT_WIDTH, OUT_HEIGHT, fps)?;
    ops::insert_clip(
        &mut project,
        Some(&timeline),
        None,
        ops::ClipRequest {
            asset_id: &asset.id,
            source_in: None,
            source_out: None,
            at: None,
            name: Some("short"),
        },
    )?;

    let mut request = RenderRequest::for_output(output);
    request.timeline_id = Some(timeline);
    request.width = Some(OUT_WIDTH);
    request.height = Some(OUT_HEIGHT);
    request.fps = Some(fps);
    request.crf = Some(18);
    // Captions are burned into the master before this pass (word-timed karaoke ASS), so the
    // render pipeline must not draw a second, plain caption track over them.
    request.burn_captions = false;
    request.entitlements = None;

    let outcome = render::render(&project, &store, &request, &|_, _| {}, &|| false)?;
    Ok((outcome.width, outcome.height))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_master_is_refused() {
        let err = render_vertical(Path::new("/nonexistent/m.mp4"), Path::new("/tmp/o.mp4"), Path::new("/tmp"))
            .unwrap_err();
        assert_eq!(err.code, crate::error::ErrorCode::InvalidInput);
    }
}

//! Audio source separation through Demucs (github.com/facebookresearch/demucs), run in an
//! external Python environment named by `CINVAR_DEMUCS_PYTHON` (a venv with
//! `demucs==4.1.0` + torch), or by the path `cinvar setup --pro-ai` recorded in the config.
//! Weights live in the Hugging Face cache named by `CINVAR_DEMUCS_CACHE` (default
//! `<cache-root>/demucs`). A run whose checkpoint is already cached proceeds with the child
//! forced offline; only a run that would have to download needs
//! `CINVAR_DEMUCS_ALLOW_DOWNLOAD=1`, and nothing is ever downloaded silently.
//!
//! Resource notes: `htdemucs` needs ~3 GB VRAM on GPU (`CINVAR_DEMUCS_DEVICE=cuda`,
//! pick the GPU with CUDA_VISIBLE_DEVICES — never the display GPU) or ~4 GB RAM on CPU
//! (~13 s for a 10 s clip measured).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::secrets;
use crate::util::paths::HelperOrigin;
use crate::util::proc::{self, ProcOpts};

const HELPER: &str = include_str!("demucs_helper.py");
pub const ENV_PYTHON: &str = "CINVAR_DEMUCS_PYTHON";
pub const ENV_DEVICE: &str = "CINVAR_DEMUCS_DEVICE";
pub const ENV_ALLOW_DOWNLOAD: &str = "CINVAR_DEMUCS_ALLOW_DOWNLOAD";
pub const ENV_CACHE: &str = "CINVAR_DEMUCS_CACHE";
const TIMEOUT_SECS: u64 = 3600;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DemucsReport {
    pub stems: Vec<String>,
    pub seconds: f64,
}

/// The Demucs interpreter: the exported variable (user authority), else the path
/// `cinvar setup --pro-ai` recorded in the config.
pub fn python_from_env() -> Result<PathBuf> {
    let Some((path, origin)) = crate::util::paths::helper_path(ENV_PYTHON) else {
        return Err(Error::invalid("Demucs is not configured").with_hint(format!(
            "set {ENV_PYTHON} to the python inside a venv with demucs==4.1.0, or run cinvar setup --pro-ai"
        )));
    };
    if path.is_file() {
        return Ok(path);
    }
    match origin {
        HelperOrigin::Environment => Err(Error::invalid(format!(
            "{ENV_PYTHON} does not point to a python binary: {}",
            path.display()
        ))),
        HelperOrigin::Config => Err(Error::invalid(format!(
            "{ENV_PYTHON} recorded by cinvar setup --pro-ai is not a file: {}",
            path.display()
        ))
        .with_hint("re-run: cinvar setup --pro-ai")),
    }
}

/// Parses the helper's JSON (its stdout is a single line).
pub fn parse_report(text: &str) -> Result<DemucsReport> {
    let line = text
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'))
        .ok_or_else(|| Error::internal("demucs helper printed no JSON result"))?;
    let r: DemucsReport = serde_json::from_str(line.trim())
        .map_err(|e| Error::internal(format!("demucs helper returned invalid JSON: {e}")))?;
    if r.stems.len() < 2 {
        return Err(Error::internal("demucs helper produced fewer than 2 stems"));
    }
    Ok(r)
}

/// Smallest file that can plausibly be a demucs checkpoint; htdemucs's own is 84 MB.
const MIN_CHECKPOINT_BYTES: u64 = 1 << 20;

/// Letters and digits only, lower case: `models--adefossez--HTDemucs` and `htdemucs` both reduce
/// to something comparable.
fn normalised(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase()
}

fn is_checkpoint(path: &Path) -> bool {
    let weights = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            matches!(
                e.to_ascii_lowercase().as_str(),
                "safetensors" | "th" | "pt" | "pth" | "bin"
            )
        })
        .unwrap_or(false);
    weights
        && std::fs::metadata(path)
            .map(|m| m.is_file() && m.len() >= MIN_CHECKPOINT_BYTES)
            .unwrap_or(false)
}

/// Every cache root a demucs checkpoint can live under: the adapter's own cache (exported to the
/// child as `HF_HOME`), `TORCH_HOME`, and torch's default location. Demucs 4.1 fetches weights
/// through `huggingface_hub`, which caches under `<root>/hub/models--<owner>--<repo>/snapshots`,
/// while the torch.hub path uses `<root>/hub/checkpoints`.
fn cache_roots(cache: &Path) -> Vec<PathBuf> {
    let mut roots = vec![cache.to_path_buf()];
    if let Ok(torch_home) = std::env::var("TORCH_HOME") {
        if !torch_home.trim().is_empty() {
            roots.push(PathBuf::from(torch_home));
        }
    }
    roots.push(crate::util::paths::home_dir().join(".cache").join("torch"));
    roots
}

/// The checkpoint file name prefix demucs gives each model on the torch.hub path (`955717e8` is
/// htdemucs). An unknown model has no prefix, so its torch.hub cache is never assumed to match.
fn checkpoint_prefix(model: &str) -> Option<&'static str> {
    match model {
        "htdemucs" => Some("955717e8"),
        _ => None,
    }
}

fn first_checkpoint(dir: &Path, prefix: Option<&str>) -> Option<PathBuf> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .collect();
    entries.sort();
    entries.into_iter().find(|path| {
        is_checkpoint(path)
            && prefix.map_or(true, |p| {
                path.file_name()
                    .map(|n| n.to_string_lossy().starts_with(p))
                    .unwrap_or(false)
            })
    })
}

/// The checkpoint this run needs when it is already on disk, and `None` when it is not.
///
/// `None` is the only case that requires the download opt-in: a cached checkpoint is what the
/// opt-in would produce, so refusing it protects nothing.
pub fn cached_checkpoint(cache: &Path, model: &str) -> Option<PathBuf> {
    let want = normalised(model);
    for root in cache_roots(cache) {
        let hub = root.join("hub");
        let mut repos: Vec<PathBuf> = std::fs::read_dir(&hub)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .filter(|path| {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                normalised(name.rsplit("--").next().unwrap_or(name.as_str())) == want
            })
            .collect();
        repos.sort();
        for repo in repos {
            let mut revisions: Vec<PathBuf> = std::fs::read_dir(repo.join("snapshots"))
                .into_iter()
                .flatten()
                .flatten()
                .map(|entry| entry.path())
                .collect();
            revisions.sort();
            for revision in revisions {
                if let Some(found) = first_checkpoint(&revision, None) {
                    return Some(found);
                }
            }
        }
        // The torch.hub path stores <signature-prefix>-<hash>.th, so an unknown model cannot be
        // matched there and is never *assumed* cached.
        if let Some(prefix) = checkpoint_prefix(model) {
            if let Some(found) = first_checkpoint(&hub.join("checkpoints"), Some(prefix)) {
                return Some(found);
            }
        }
    }
    None
}

/// Runs demucs separation; returns stem paths.
///
/// A checkpoint already in the cache runs with no opt-in and with the child forced offline.
/// Only a run that would download needs `CINVAR_DEMUCS_ALLOW_DOWNLOAD=1`, so a cached install
/// never has to re-authorise, and nothing downloads without the opt-in.
pub fn separate(input: &Path, work_dir: &Path, model: &str) -> Result<DemucsReport> {
    let python = python_from_env()?;
    std::fs::create_dir_all(work_dir)?;
    let script = work_dir.join("demucs_helper.py");
    std::fs::write(&script, HELPER)?;
    let mut o = ProcOpts::default().with_timeout(TIMEOUT_SECS);
    let cache = std::env::var(ENV_CACHE)
        .unwrap_or_else(|_| "/mnt/elyreon-data/cinvar-models/demucs".to_string());
    o.env.push(("HF_HOME".to_string(), cache.clone()));
    if std::env::var(ENV_DEVICE).is_ok() {
        o.env
            .push((ENV_DEVICE.to_string(), std::env::var(ENV_DEVICE).unwrap()));
    }
    let cache_dir = PathBuf::from(&cache);
    let cached = cached_checkpoint(&cache_dir, model);
    let opted_in = std::env::var(ENV_ALLOW_DOWNLOAD).as_deref() == Ok("1");
    if cached.is_none() && !opted_in {
        // Only a run that would fetch needs the opt-in. Refusing a cached checkpoint would
        // break the common case: the weights are installed once, then reused forever.
        return Err(Error::invalid(format!(
            "the {model} weights are not in the cache, so this run would download them"
        ))
        .with_hint(format!(
            "set {ENV_ALLOW_DOWNLOAD}=1 for the one download, or point {ENV_CACHE} at a cache that already holds them (looked in {})",
            cache_dir.display()
        )));
    }
    if cached.is_some() && !opted_in {
        // Nothing needs fetching, so the child is kept off the network: demucs' Hugging Face
        // loader queries the Hub even when the checkpoint is already on disk.
        o.env.push(("HF_HUB_OFFLINE".to_string(), "1".to_string()));
        o.env.push(("TRANSFORMERS_OFFLINE".to_string(), "1".to_string()));
    }
    let args = vec![
        "-I".to_string(),
        script.to_string_lossy().to_string(),
        input.to_string_lossy().to_string(),
        work_dir.to_string_lossy().to_string(),
        model.to_string(),
    ];
    let out = proc::run(&python, &args, &o)?;
    if !out.success {
        return Err(Error::internal(format!(
            "demucs helper failed: {}",
            secrets::redact(&out.tail_stderr(3))
        )));
    }
    parse_report(&out.stdout)
}

/// Attack of the ducking compressor, in milliseconds. 20 ms is short enough that the
/// music is already down when the first syllable lands.
pub const DUCK_ATTACK_MS: u32 = 20;
/// Release of the ducking compressor, in milliseconds. 300 ms lets the music bed recover
/// smoothly inside a pause instead of pumping up and down between words.
pub const DUCK_RELEASE_MS: u32 = 300;
/// Mix sample rate and channel layout, so a 44.1 kHz bed and a 48 kHz speech stem still sum.
const MIX_RATE: u32 = 48_000;

/// Sidechain ducker settings for the music bed.
#[derive(Debug, Clone, PartialEq)]
pub struct DuckOptions {
    /// Level (linear, 0..1) at which the sidechain starts compressing.
    pub threshold: f64,
    /// Compression ratio while the sidechain is above the threshold. 1.0 is transparent.
    pub ratio: f64,
    pub attack_ms: u32,
    pub release_ms: u32,
    /// Fixed gain applied by the compressor (1.0 = none).
    pub makeup: f64,
    /// Gain applied to the music bed before compression (1.0 = unchanged).
    pub music_gain: f64,
    /// Peak ceiling of the finished mix (linear). Below 1.0 keeps the sum from clipping.
    pub limit: f64,
}

impl Default for DuckOptions {
    fn default() -> Self {
        DuckOptions {
            threshold: 0.05,
            ratio: 8.0,
            attack_ms: DUCK_ATTACK_MS,
            release_ms: DUCK_RELEASE_MS,
            makeup: 1.0,
            music_gain: 1.0,
            limit: 0.95,
        }
    }
}

impl DuckOptions {
    /// These settings with the compressor bypassed: the same mix without ducking, which is
    /// what a measurement compares against. Ratio 1 leaves the bed at its own level while
    /// the amplitude is above the threshold.
    pub fn without_ducking(&self) -> DuckOptions {
        DuckOptions {
            ratio: 1.0,
            threshold: 1.0,
            ..self.clone()
        }
    }
}

/// The ffmpeg filter_complex for a ducked mix: input 0 is speech, input 1 is the music bed.
///
/// The bed goes through sidechaincompress with the speech as its sidechain, and the speech
/// itself continues into the sum untouched, so the words are never attenuated. alimiter caps
/// the sum below full scale, because normalize=0 (needed to keep the speech at its own level)
/// can otherwise push the peak past 0 dBFS.
///
/// target_secs is the length the mix must reach (the longer of the two inputs). Both inputs are
/// padded past it before the compressor, because sidechaincompress stops as soon as the
/// sidechain ends - without the padding the music bed would be cut off the moment the speech
/// stops, and the mix would be as short as the speech stem. The sum is trimmed back to the
/// target afterwards.
pub fn duck_filter(opts: &DuckOptions, target_secs: Option<f64>) -> String {
    let (pad, trim) = match target_secs {
        Some(secs) if secs.is_finite() && secs > 0.0 => (
            format!(",apad=whole_dur={:.3}", secs + DUCK_TAIL_MARGIN),
            format!(",atrim=end={:.3}", secs),
        ),
        _ => (String::new(), String::new()),
    };
    format!(
        "[0:a]aformat=sample_rates={MIX_RATE}:channel_layouts=stereo{pad},asplit=2[sp_mix][sp_sc];\
[1:a]aformat=sample_rates={MIX_RATE}:channel_layouts=stereo,volume={gain:.4}{pad}[music];\
[music][sp_sc]sidechaincompress=threshold={threshold:.4}:ratio={ratio:.3}:attack={attack}:release={release}:makeup={makeup:.4}[bed];\
[sp_mix][bed]amix=inputs=2:normalize=0:duration=longest{trim},alimiter=limit={limit:.4}:level=0[mix]",
        gain = opts.music_gain,
        threshold = opts.threshold,
        ratio = opts.ratio,
        attack = opts.attack_ms,
        release = opts.release_ms,
        makeup = opts.makeup,
        limit = opts.limit,
        pad = pad,
        trim = trim,
    )
}

/// How far past the target length the inputs are padded before the compressor, in seconds.
pub const DUCK_TAIL_MARGIN: f64 = 0.5;

/// Measured duration of an audio file, in seconds.
fn media_seconds(path: &Path) -> Result<f64> {
    let info = crate::media::probe(path)?;
    let seconds = info.duration.as_secs_f64();
    if !(seconds > 0.0) {
        return Err(Error::invalid(format!(
            "{} has no measurable duration",
            path.display()
        )));
    }
    Ok(seconds)
}

fn encode_args(output: &Path) -> Vec<String> {
    let ext = output
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("m4a")
        .to_ascii_lowercase();
    match ext.as_str() {
        "wav" => vec!["-c:a".into(), "pcm_s16le".into()],
        _ => vec!["-c:a".into(), "aac".into(), "-b:a".into(), "192k".into()],
    }
}

/// Mix a speech stem with a music bed ducked under it.
///
/// Returns the output path. The mix runs for the longer of the two inputs
/// (amix duration=longest) so a short bed never truncates the speech.
pub fn duck_mix(
    vocals: &Path,
    music: &Path,
    output: &Path,
    opts: &DuckOptions,
) -> Result<PathBuf> {
    for (label, path) in [("speech", vocals), ("music", music)] {
        if !path.is_file() {
            return Err(Error::invalid(format!(
                "the {label} stem does not exist: {}",
                path.display()
            )));
        }
    }
    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let ffmpeg = proc::which("ffmpeg").ok_or_else(|| Error::invalid("ffmpeg not found on PATH"))?;
    // The mix runs for the longer input; both stems are padded past it so the compressor
    // cannot end the bed early.
    let target = media_seconds(vocals)?.max(media_seconds(music)?);
    let mut args: Vec<String> = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-y".into(),
        "-i".into(),
        vocals.to_string_lossy().to_string(),
        "-i".into(),
        music.to_string_lossy().to_string(),
        "-filter_complex".into(),
        duck_filter(opts, Some(target)),
        "-map".into(),
        "[mix]".into(),
    ];
    args.extend(encode_args(output));
    args.push(output.to_string_lossy().to_string());
    let mix = proc::run(&ffmpeg, &args, &ProcOpts::default().with_timeout(1800))?;
    if !mix.success {
        return Err(Error::internal(format!(
            "the ducked mix failed: {}",
            secrets::redact(&mix.tail_stderr(4))
        )));
    }
    if !output.is_file() {
        return Err(Error::internal(format!(
            "the ducked mix wrote no file at {}",
            output.display()
        )));
    }
    Ok(output.to_path_buf())
}

/// Produces a cleaned mix from separated stems: speech at its own level, music bed ducked
/// under it with a sidechain compressor (attack 20 ms, release 300 ms by default).
pub fn clean_mix_ducked(
    input: &Path,
    output: &Path,
    work_dir: &Path,
    opts: &DuckOptions,
) -> Result<DemucsReport> {
    let report = separate(input, work_dir, "htdemucs")?;
    let stem = |suffix: &str| -> Result<PathBuf> {
        report
            .stems
            .iter()
            .find(|s| s.ends_with(suffix))
            .map(PathBuf::from)
            .ok_or_else(|| Error::internal(format!("no {suffix} in the demucs output")))
    };
    let vocals = stem("vocals.wav")?;
    let music = stem("other.wav")?;
    duck_mix(&vocals, &music, output, opts)?;
    Ok(report)
}

/// Produces a cleaned mix: vocals + a music bed ducked under the speech (sidechain
/// compressor), rendered with ffmpeg. Returns the separation report.
pub fn clean_mix(input: &Path, output: &Path, work_dir: &Path) -> Result<DemucsReport> {
    clean_mix_ducked(input, output, work_dir, &DuckOptions::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_report() {
        let raw = "noise\n{\"ok\": true, \"stems\": [\"/a/vocals.wav\", \"/a/other.wav\", \"/a/drums.wav\", \"/a/bass.wav\"], \"seconds\": 13.11}";
        let r = parse_report(raw).unwrap();
        assert_eq!(r.stems.len(), 4);
        assert!((r.seconds - 13.11).abs() < 1e-9);
    }

    #[test]
    fn rejects_degenerate_report() {
        assert!(parse_report("{\"stems\":[\"/a/vocals.wav\"],\"seconds\":1.0}").is_err());
        assert!(parse_report("no json").is_err());
    }

    #[test]
    fn refuses_without_download_opt_in() {
        let _guard = env_lock();
        std::env::set_var(ENV_ALLOW_DOWNLOAD, "");
        let r = separate(Path::new("/tmp/x.mp3"), Path::new("/tmp/dw"), "htdemucs");
        assert!(r.is_err());
        std::env::remove_var(ENV_ALLOW_DOWNLOAD);
    }


    /// The download gate reads the cache, so its decision is deterministic.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn write_checkpoint(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, vec![0u8; (MIN_CHECKPOINT_BYTES + 16) as usize]).unwrap();
    }

    /// A cache populated by `huggingface_hub` (what demucs 4.1 actually writes) and one populated
    /// by `torch.hub` are both recognised, and nothing else is.
    #[test]
    fn a_cached_checkpoint_is_found_and_an_absent_one_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let empty = dir.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert_eq!(cached_checkpoint(&empty, "htdemucs"), None);

        let hf = dir.path().join("hf");
        let weights = hf
            .join("hub")
            .join("models--adefossez--HTDemucs")
            .join("snapshots")
            .join("cbc8a9b1a87023b7fd74e7b3412e6321c0eab003")
            .join("955717e8.safetensors");
        write_checkpoint(&weights);
        assert_eq!(cached_checkpoint(&hf, "htdemucs"), Some(weights));
        assert_eq!(
            cached_checkpoint(&hf, "htdemucs_ft"),
            None,
            "a different model is not satisfied by this checkpoint"
        );

        let torch_cache = dir.path().join("torch");
        let th = torch_cache
            .join("hub")
            .join("checkpoints")
            .join("955717e8-8726e21a.th");
        write_checkpoint(&th);
        assert_eq!(cached_checkpoint(&torch_cache, "htdemucs"), Some(th));
        assert_eq!(
            cached_checkpoint(&torch_cache, "some-other-model"),
            None,
            "an unknown model is never assumed cached"
        );

        // A truncated download is not a usable checkpoint.
        let partial = dir.path().join("partial");
        let truncated = partial
            .join("hub")
            .join("models--adefossez--HTDemucs")
            .join("snapshots")
            .join("rev")
            .join("955717e8.safetensors");
        std::fs::create_dir_all(truncated.parent().unwrap()).unwrap();
        std::fs::write(&truncated, b"short").unwrap();
        assert_eq!(cached_checkpoint(&partial, "htdemucs"), None);
    }

    /// A stand-in interpreter: the helper is never run, only spawned, so it just answers in the
    /// JSON shape `parse_report` expects.
    #[cfg(unix)]
    fn fake_python(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("python");
        std::fs::write(
            &path,
            "#!/bin/sh\necho '{\"stems\":[\"/tmp/demucs/vocals.wav\",\"/tmp/demucs/other.wav\"],\"seconds\":0.1}'\n",
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// The defect this fixes: a cached install had to keep `CINVAR_DEMUCS_ALLOW_DOWNLOAD=1` set
    /// forever, even though nothing was left to download.
    #[cfg(unix)]
    #[test]
    fn separates_with_cached_weights_and_no_download_opt_in() {
        let _guard = env_lock();
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("cache");
        write_checkpoint(
            &cache
                .join("hub")
                .join("models--acme--htdemucs-test")
                .join("snapshots")
                .join("rev")
                .join("weights.safetensors"),
        );
        std::env::set_var(ENV_PYTHON, fake_python(dir.path()));
        std::env::set_var(ENV_CACHE, &cache);
        std::env::remove_var(ENV_ALLOW_DOWNLOAD);
        let report = separate(
            &dir.path().join("input.mp3"),
            &dir.path().join("work"),
            "htdemucs-test",
        )
        .expect("a cached checkpoint runs without the download opt-in");
        assert_eq!(report.stems.len(), 2);
        std::env::remove_var(ENV_PYTHON);
        std::env::remove_var(ENV_CACHE);
    }

    /// ...and it still refuses honestly when the cache is empty, rather than downloading.
    #[cfg(unix)]
    #[test]
    fn refuses_when_the_checkpoint_is_missing_and_downloads_are_not_authorised() {
        let _guard = env_lock();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var(ENV_PYTHON, fake_python(dir.path()));
        std::env::set_var(ENV_CACHE, dir.path().join("empty-cache"));
        std::env::remove_var(ENV_ALLOW_DOWNLOAD);
        let err = separate(
            &dir.path().join("input.mp3"),
            &dir.path().join("work"),
            "htdemucs-test",
        )
        .expect_err("an empty cache must not download silently");
        let hint = err.hint.clone().unwrap_or_default();
        assert!(
            err.message.contains("not in the cache"),
            "the refusal names the real reason: {}",
            err.message
        );
        assert!(hint.contains(ENV_ALLOW_DOWNLOAD), "{hint}");

        // The opt-in is what a download needs, and it is enough on its own.
        std::env::set_var(ENV_ALLOW_DOWNLOAD, "1");
        assert!(separate(
            &dir.path().join("input.mp3"),
            &dir.path().join("work"),
            "htdemucs-test"
        )
        .is_ok());
        std::env::remove_var(ENV_ALLOW_DOWNLOAD);
        std::env::remove_var(ENV_PYTHON);
        std::env::remove_var(ENV_CACHE);
    }

    /// Real run against an installed demucs, with no download opt-in: proves a cached checkpoint
    /// really separates (and that the forced-offline child still finds its weights).
    /// Needs CINVAR_DEMUCS_PYTHON, CINVAR_DEMUCS_CACHE, CINVAR_E2E_AUDIO and CINVAR_E2E_OUT.
    /// Run with: cargo test --release -- --ignored e2e_demucs_cached_weights
    #[test]
    #[ignore = "needs an installed demucs venv and CINVAR_E2E_AUDIO"]
    fn e2e_demucs_cached_weights_run_without_allow_download() {
        // The two real-run tests share the process environment, so they take turns.
        let _guard = env_lock();
        let env = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("{k} is not set"));
        let input = PathBuf::from(env("CINVAR_E2E_AUDIO"));
        let out = PathBuf::from(env("CINVAR_E2E_OUT"));
        std::fs::create_dir_all(&out).unwrap();
        std::env::remove_var(ENV_ALLOW_DOWNLOAD);
        let cache = PathBuf::from(env("CINVAR_DEMUCS_CACHE"));
        std::env::set_var(ENV_CACHE, &cache);
        assert!(
            cached_checkpoint(&cache, "htdemucs").is_some(),
            "the run needs a cached checkpoint in {}",
            cache.display()
        );
        let report = separate(&input, &out.join("work"), "htdemucs").expect("separation runs");
        assert!(report.stems.len() >= 4, "four stems: {:?}", report.stems);
        for stem in &report.stems {
            assert!(Path::new(stem).is_file(), "{stem} exists");
        }
        println!("DEMUCS_STEMS {}", serde_json::to_string(&report.stems).unwrap());
        println!("DEMUCS_SECONDS {}", report.seconds);
    }

    /// The other half of the rule, against the real interpreter and a genuinely empty cache:
    /// the run refuses and names the opt-in instead of downloading anything.
    /// Run with: cargo test --release -- --ignored e2e_demucs_refuses_without_cached_weights
    #[test]
    #[ignore = "needs an installed demucs venv and CINVAR_E2E_AUDIO"]
    fn e2e_demucs_refuses_without_cached_weights() {
        let _guard = env_lock();
        let env = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("{k} is not set"));
        let input = PathBuf::from(env("CINVAR_E2E_AUDIO"));
        let out = PathBuf::from(env("CINVAR_E2E_OUT")).join("refusal");
        std::fs::create_dir_all(&out).unwrap();
        let empty = out.join("empty-cache");
        std::fs::create_dir_all(&empty).unwrap();
        std::env::remove_var(ENV_ALLOW_DOWNLOAD);
        std::env::set_var(ENV_CACHE, &empty);
        let err = separate(&input, &out.join("work"), "htdemucs")
            .expect_err("an empty cache must not download without the opt-in");
        println!("DEMUCS_REFUSAL {}", err.message);
        println!("DEMUCS_REFUSAL_HINT {}", err.hint.unwrap_or_default());
        assert!(Path::new(&empty).read_dir().unwrap().next().is_none(), "nothing was fetched");
    }
}

//! Free/Pro edition model: one binary, feature-based entitlements.
//!
//! There is a single application. What differs between editions is an [`Entitlements`] value
//! resolved once from configuration and an (optional) signed license file. Gated code asks
//! the entitlements ("may this export at 3840x2160?", "may this use cloud render?") instead of
//! checking an edition name, so adding a tier or moving a feature is a table change.
//!
//! Phase 1 scope (see `CINVAR_FREE_PRO_PLAN.md`):
//! * enforcement of the Free export-resolution cap at the render, Blender and tool boundaries;
//! * a tool-level feature table (currently cloud render only);
//! * the signed-license format and verifier, with **no production key provisioned yet**;
//! * release builds enforce by default; the `edition-observe` feature (tests only) makes the
//!   default observe-only. The already-published 0.1.0 package is unaffected (separate binary).
//!
//! Honest limits: CINVAR's source is Apache-2.0. A gate in open source code stops accidents and
//! honest users, not someone who rebuilds the binary. See the plan's "what the gate is not".

pub mod license;

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::{Error, ErrorCode, Result};

/// Output size tiers. A size belongs to the smallest tier that contains it:
/// * `export.2k` (Free): long edge <= 2048 and at most 2048x1080 pixels, either orientation
///   (so 1920x1080, 2048x1080, 1080x2048, 1600x1200 pass; 2560x1440 does not);
/// * `export.4k`: long edge <= 4096 and at most 4096x2160 pixels (UHD and DCI 4K);
/// * `export.8k`: anything else up to the engine limit (`render::capability::MAX_DIMENSION`).
pub const TIER_2K_LONG_EDGE: u32 = 2048;
pub const TIER_2K_PIXELS: u64 = 2048 * 1080;
pub const TIER_4K_LONG_EDGE: u32 = 4096;
pub const TIER_4K_PIXELS: u64 = 4096 * 2160;

/// The capability an output of this size needs.
pub fn export_tier(width: u32, height: u32) -> Feature {
    let long = width.max(height);
    let px = width as u64 * height as u64;
    if long <= TIER_2K_LONG_EDGE && px <= TIER_2K_PIXELS {
        Feature::Export2k
    } else if long <= TIER_4K_LONG_EDGE && px <= TIER_4K_PIXELS {
        Feature::Export4k
    } else {
        Feature::Export8k
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Edition {
    Free,
    Pro,
}

impl Edition {
    pub fn as_str(self) -> &'static str {
        match self {
            Edition::Free => "free",
            Edition::Pro => "pro",
        }
    }
}

/// A gated capability. Only capabilities that are implemented and enforced belong here; planned
/// Pro features are listed in the plan document, not in this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Feature {
    /// Output up to 2048x1080 / 1080x2048. Included in Free.
    #[serde(rename = "export.2k")]
    Export2k,
    /// Output up to 3840x2160 (long edge 4096).
    #[serde(rename = "export.4k")]
    Export4k,
    /// Output above 4K up to the engine limit.
    #[serde(rename = "export.8k")]
    Export8k,
    /// Sending a render to a paid cloud GPU (experimental).
    #[serde(rename = "render.cloud")]
    CloudRender,
    /// Queue several renders with different outputs (Pro module).
    #[serde(rename = "render.batch")]
    RenderBatch,
    /// Save and reuse export presets (Pro module).
    #[serde(rename = "export.presets")]
    ExportPresets,
    /// Advanced color grading: exposure, white balance, curves, grade presets (Pro module).
    #[serde(rename = "color.advanced")]
    ColorAdvanced,
    /// ProRes, DNxHR and AV1 export codecs.
    #[serde(rename = "export.pro_codecs")]
    ProCodecs,
    /// Word-level caption timing from WhisperX.
    #[serde(rename = "captions.word_level")]
    CaptionsWordLevel,
    /// Animated ASS karaoke captions (per-word highlight animation).
    #[serde(rename = "captions.karaoke")]
    CaptionsKaraoke,
    /// Custom branded caption styles (fonts, palettes, layouts saved as presets).
    #[serde(rename = "captions.branded")]
    CaptionsBranded,
    /// Full Real-ESRGAN video upscaling.
    #[serde(rename = "enhance.upscale")]
    EnhanceUpscale,
    /// Demucs 4-stem audio separation and cleaned mixes.
    #[serde(rename = "audio.stems")]
    AudioStems,
    /// FastERRCNN subject-tracking reframing (stable 9:16 crops).
    #[serde(rename = "reframe.tracking")]
    ReframeTracking,
    /// Semantic media indexing without the Free per-project clip cap.
    #[serde(rename = "semantic.full")]
    SemanticFull,
    /// Cross-project semantic and transcript search.
    #[serde(rename = "search.cross_project")]
    SearchCrossProject,
    /// Autonomous multi-step AI editing workflows.
    #[serde(rename = "ai.autonomous")]
    AiAutonomous,
    /// Multi-folder batch processing.
    #[serde(rename = "batch.multi_folder")]
    BatchMultiFolder,
    /// Custom pacing curves for automated edits.
    #[serde(rename = "pacing.custom")]
    PacingCustom,
    /// Multi-Short generation from one long source.
    #[serde(rename = "shorts.multi")]
    ShortsMulti,
    /// Advanced MCP production tools (long-running jobs, batch orchestration).
    #[serde(rename = "mcp.production")]
    McpProduction,
    /// OpenTimelineIO export for Resolve/Premiere interoperability.
    #[serde(rename = "export.otio")]
    OtioExport,
    /// Full Blender animation production and rendering.
    #[serde(rename = "blender.production")]
    BlenderProduction,
    /// Blender screen/device mockup compositions from timeline video.
    #[serde(rename = "blender.mockup")]
    BlenderMockup,
    /// Distributed Blender animation rendering across worker machines.
    #[serde(rename = "blender.distributed")]
    BlenderDistributed,
}

impl Feature {
    pub const ALL: &'static [Feature] = &[
        Feature::Export2k,
        Feature::Export4k,
        Feature::Export8k,
        Feature::CloudRender,
        Feature::RenderBatch,
        Feature::ExportPresets,
        Feature::ColorAdvanced,
        Feature::ProCodecs,
        Feature::CaptionsWordLevel,
        Feature::CaptionsKaraoke,
        Feature::CaptionsBranded,
        Feature::EnhanceUpscale,
        Feature::AudioStems,
        Feature::ReframeTracking,
        Feature::SemanticFull,
        Feature::SearchCrossProject,
        Feature::AiAutonomous,
        Feature::BatchMultiFolder,
        Feature::PacingCustom,
        Feature::ShortsMulti,
        Feature::McpProduction,
        Feature::OtioExport,
        Feature::BlenderProduction,
        Feature::BlenderMockup,
        Feature::BlenderDistributed,
    ];

    /// Stable dotted capability id (also the serde name and the id used in license files).
    pub fn as_str(self) -> &'static str {
        match self {
            Feature::Export2k => "export.2k",
            Feature::Export4k => "export.4k",
            Feature::Export8k => "export.8k",
            Feature::CloudRender => "render.cloud",
            Feature::RenderBatch => "render.batch",
            Feature::ExportPresets => "export.presets",
            Feature::ColorAdvanced => "color.advanced",
            Feature::ProCodecs => "export.pro_codecs",
            Feature::CaptionsWordLevel => "captions.word_level",
            Feature::CaptionsKaraoke => "captions.karaoke",
            Feature::CaptionsBranded => "captions.branded",
            Feature::EnhanceUpscale => "enhance.upscale",
            Feature::AudioStems => "audio.stems",
            Feature::ReframeTracking => "reframe.tracking",
            Feature::SemanticFull => "semantic.full",
            Feature::SearchCrossProject => "search.cross_project",
            Feature::AiAutonomous => "ai.autonomous",
            Feature::BatchMultiFolder => "batch.multi_folder",
            Feature::PacingCustom => "pacing.custom",
            Feature::ShortsMulti => "shorts.multi",
            Feature::McpProduction => "mcp.production",
            Feature::OtioExport => "export.otio",
            Feature::BlenderProduction => "blender.production",
            Feature::BlenderMockup => "blender.mockup",
            Feature::BlenderDistributed => "blender.distributed",
        }
    }

    /// Included in the Free edition.
    pub fn free_set() -> &'static [Feature] {
        &[Feature::Export2k]
    }

    pub fn parse(s: &str) -> Option<Feature> {
        Feature::ALL.iter().copied().find(|f| f.as_str() == s)
    }

    /// Features included in the Pro edition today.
    pub fn pro_set() -> &'static [Feature] {
        Feature::ALL
    }
}

/// Which tool names need which feature. Everything not listed is available in every edition.
///
/// This table is the single source of truth for MCP/CLI tool gating: `tools::dispatch` consults
/// it before running any tool, so a tool that is not listed here is free in every edition.
pub fn tool_feature(tool: &str) -> Option<Feature> {
    match tool {
        "cinvar.render.cloud" => Some(Feature::CloudRender),
        n if n.starts_with("cinvar.render.batch_") => Some(Feature::RenderBatch),
        n if n.starts_with("cinvar.preset.") => Some(Feature::ExportPresets),
        "cinvar.color.grade" | "cinvar.color.grade_preset_save" | "cinvar.color.grade_preset_list"
        | "cinvar.color.grade_preset_apply" => Some(Feature::ColorAdvanced),
        // Baking an OCIO transform into a LUT or importing a third-party LUT is the advanced
        // grading path; cinvar.color.inspect stays free (read-only inspection).
        "cinvar.color.apply_transform" | "cinvar.color.import_lut" => Some(Feature::ColorAdvanced),
        // Blender production surface. inspect / preview_check / scene_build stay free so an
        // agent can explore and validate scenes before any paid rendering happens.
        "cinvar.blender.mockup_screen" => Some(Feature::BlenderMockup),
        "cinvar.blender.render_job" => Some(Feature::BlenderProduction),
        "cinvar.blender.animation" => Some(Feature::BlenderProduction),
        "cinvar.blender.attach" | "cinvar.blender.render_distributed" => {
            Some(Feature::BlenderDistributed)
        }
        // Editing capabilities.
        n if n.starts_with("cinvar.shorts.") => Some(Feature::ShortsMulti),
        "cinvar.otio.export" | "cinvar.timeline.export_otio" => Some(Feature::OtioExport),
        "cinvar.audio.stems" | "cinvar.audio.separate" => Some(Feature::AudioStems),
        "cinvar.enhance.upscale" | "cinvar.enhance.video" => Some(Feature::EnhanceUpscale),
        "cinvar.captions.word_level" | "cinvar.captions.transcribe_words" => {
            Some(Feature::CaptionsWordLevel)
        }
        "cinvar.captions.karaoke" | "cinvar.captions.ass" => Some(Feature::CaptionsKaraoke),
        "cinvar.captions.style_save" | "cinvar.captions.style_list" => {
            Some(Feature::CaptionsBranded)
        }
        "cinvar.search.cross_project" | "cinvar.semantic.search_all" => {
            Some(Feature::SearchCrossProject)
        }
        "cinvar.agent.autonomous" | "cinvar.agent.workflow_run" => Some(Feature::AiAutonomous),
        "cinvar.batch.folders" => Some(Feature::BatchMultiFolder),
        "cinvar.pacing.custom" => Some(Feature::PacingCustom),
        _ => None,
    }
}

/// Capabilities a `--pro-preview` run can execute for real, shortened and watermarked. Anything
/// outside this list is refused outright: a preview that cannot be shortened would hand over the
/// whole paid capability.
pub fn feature_supports_preview(feature: Feature) -> bool {
    matches!(
        feature,
        Feature::Export2k
            | Feature::Export4k
            | Feature::Export8k
            | Feature::ProCodecs
            | Feature::EnhanceUpscale
            | Feature::OtioExport
    )
}

/// Free semantic indexing allowance: clips a Free project may index for vision search.
pub const SEMANTIC_FREE_CLIP_LIMIT: usize = 10;

/// Length of the real run a Pro preview performs (seconds of output).
pub const PRO_PREVIEW_SECS: f64 = 10.0;
/// The notice burned into a previewed file; also matches the storefront price.
pub const PRO_PREVIEW_OVERLAY: &str = "CINVAR PRO PREVIEW - Unlock for $29.99";
/// How a gate was satisfied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grant {
    /// The edition includes the capability (or enforcement is off).
    Included,
    /// Not included, but `--pro-preview` asked for a real, shortened, watermarked run.
    Preview,
}

impl Grant {
    pub fn is_preview(self) -> bool {
        matches!(self, Grant::Preview)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Enforcement {
    /// Resolve and report the edition but never refuse anything (legacy behaviour).
    Observe,
    /// Refuse what the edition does not include.
    Enforce,
}

/// Where the current entitlements came from, for `edition status` and support.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Source {
    /// No license file: the Free edition.
    NoLicense,
    /// A verified license file.
    License { id: String, grandfathered: bool },
    /// A license file exists but was rejected; Free applies. The reason is shown to the user.
    Rejected { reason: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct Entitlements {
    pub edition: Edition,
    pub enforcement: Enforcement,
    pub source: Source,
    features: Vec<Feature>,
}

impl Entitlements {
    pub fn free(enforcement: Enforcement) -> Entitlements {
        Entitlements {
            edition: Edition::Free,
            enforcement,
            source: Source::NoLicense,
            features: Feature::free_set().to_vec(),
        }
    }

    pub fn pro(enforcement: Enforcement, id: &str, grandfathered: bool) -> Entitlements {
        Entitlements {
            edition: Edition::Pro,
            enforcement,
            source: Source::License {
                id: id.to_string(),
                grandfathered,
            },
            features: Feature::pro_set().to_vec(),
        }
    }

    pub fn with_source(mut self, source: Source) -> Entitlements {
        self.source = source;
        self
    }

    /// True if the edition includes `feature` (independent of enforcement mode).
    pub fn includes(&self, feature: Feature) -> bool {
        self.features.contains(&feature)
    }

    /// Largest export tier this edition includes, as a label for status output.
    pub fn export_limit(&self) -> &'static str {
        if self.includes(Feature::Export8k) {
            "engine limit"
        } else if self.includes(Feature::Export4k) {
            "3840x2160"
        } else {
            "2048x1080"
        }
    }

    /// The refusal every gate returns when nothing may run.
    fn feature_refusal(&self, feature: Feature) -> Error {
        let base = "the Free edition does not include it; see `cinvar edition status`";
        let hint = if feature_supports_preview(feature) {
            format!("{base}. Or run it once with --pro-preview: CINVAR runs the real capability on the first {} seconds, watermarked", PRO_PREVIEW_SECS as u32)
        } else {
            base.to_string()
        };
        Error::new(
            ErrorCode::PermissionDenied,
            format!("{} is a CINVAR Pro feature", feature.as_str()),
        )
        .with_hint(hint)
    }

    fn export_refusal(&self, width: u32, height: u32) -> Error {
        let need = export_tier(width, height);
        Error::new(
            ErrorCode::PermissionDenied,
            format!(
                "{width}x{height} exceeds the CINVAR Free export limit of {} ({} required)",
                self.export_limit(),
                need.as_str()
            ),
        )
        .with_hint(format!("Free exports up to 2048x1080 (or 1080x2048); higher resolutions are a CINVAR Pro feature. Importing and editing high-resolution sources is unaffected. Or render the first {} seconds at full size with --pro-preview (watermarked)", PRO_PREVIEW_SECS as u32))
    }

    /// Strict: refuses regardless of `--pro-preview`. Use it for capabilities with no preview surface.
    pub fn check_feature(&self, feature: Feature) -> Result<()> {
        if self.enforcement == Enforcement::Observe || self.includes(feature) {
            return Ok(());
        }
        Err(self.feature_refusal(feature))
    }

    /// Gate for a capability the caller can actually run in preview form. With `allow_preview`
    /// (the CLI's `--pro-preview`) a Free edition gets [`Grant::Preview`] instead of a refusal, and
    /// the caller must run the real capability shortened and watermarked.
    pub fn gate_feature(&self, feature: Feature, allow_preview: bool) -> Result<Grant> {
        if self.enforcement == Enforcement::Observe || self.includes(feature) {
            return Ok(Grant::Included);
        }
        if allow_preview && feature_supports_preview(feature) {
            return Ok(Grant::Preview);
        }
        Err(self.feature_refusal(feature))
    }

    /// Semantically identical to `check_export`, but a preview may render the real size.
    pub fn gate_export(&self, width: u32, height: u32, allow_preview: bool) -> Result<Grant> {
        if self.enforcement == Enforcement::Observe {
            return Ok(Grant::Included);
        }
        if self.includes(export_tier(width, height)) {
            return Ok(Grant::Included);
        }
        if allow_preview {
            return Ok(Grant::Preview);
        }
        Err(self.export_refusal(width, height))
    }

    /// Semantic indexing: Free projects may index [`SEMANTIC_FREE_CLIP_LIMIT`] clips, Pro has no cap.
    /// Called before the index is built so a Free project never gets a partial index.
    pub fn check_semantic_clips(&self, clips: usize) -> Result<()> {
        if self.enforcement == Enforcement::Observe || self.includes(Feature::SemanticFull) {
            return Ok(());
        }
        if clips <= SEMANTIC_FREE_CLIP_LIMIT {
            return Ok(());
        }
        Err(Error::new(
            ErrorCode::PermissionDenied,
            format!(
                "the CINVAR Free edition indexes {SEMANTIC_FREE_CLIP_LIMIT} clips per project; this folder has {clips} videos"
            ),
        )
        .with_hint(format!(
            "index a folder of at most {SEMANTIC_FREE_CLIP_LIMIT} videos, or include semantic.full with a CINVAR Pro license (`cinvar activate`) for unlimited indexing"
        )))
    }

    /// Strict: refuses an export larger than the edition allows, whatever `--pro-preview` says.
    /// Never clamps: the caller is told the limit and the size they asked for.
    pub fn check_export(&self, width: u32, height: u32) -> Result<()> {
        if self.enforcement == Enforcement::Observe {
            return Ok(());
        }
        if self.includes(export_tier(width, height)) {
            return Ok(());
        }
        Err(self.export_refusal(width, height))
    }

    /// If `width`x`height` is over this edition's limit, the largest even-sized box with the same
    /// aspect ratio that fits; `None` when it already fits (or enforcement is off).
    pub fn fit_export(&self, width: u32, height: u32) -> Option<(u32, u32)> {
        if self.check_export(width, height).is_ok() {
            return None;
        }
        // Fit into the familiar 1920x1080 envelope (either orientation), which sits inside the
        // 2048x1080 tier, so 16:9 becomes 1920x1080 and 9:16 becomes 1080x1920.
        let (long, short) = (width.max(height) as f64, width.min(height) as f64);
        let k = (1920.0 / long).min(1080.0 / short).min(1.0);
        let even = |v: f64| ((v * k).floor() as u32 & !1).max(16);
        let (w, h) = (even(width as f64), even(height as f64));
        // Rounding can only shrink, so this holds; the fallback is belt and braces.
        self.check_export(w, h).is_ok().then_some((w, h))
    }

    /// One-line summary for `cinvar edition status`.
    pub fn summary(&self) -> serde_json::Value {
        serde_json::json!({
            "edition": self.edition,
            "enforcement": self.enforcement,
            "source": self.source,
            "features": self.features.iter().map(|f| f.as_str()).collect::<Vec<_>>(),
            "export_limit": self.export_limit(),
            "modules": modules(),
        })
    }
}

static MODULES: std::sync::RwLock<Vec<&'static str>> = std::sync::RwLock::new(Vec::new());

/// Records that an optional (separately licensed) module is linked into this binary.
pub fn register_module(name: &'static str) {
    if let Ok(mut m) = MODULES.write() {
        if !m.contains(&name) {
            m.push(name);
        }
    }
}

/// Modules present in this binary (empty for the open-core Free build).
pub fn modules() -> Vec<&'static str> {
    MODULES.read().map(|m| m.clone()).unwrap_or_default()
}

/// Where "Upgrade to Pro" points. The storefront checkout is not live yet, so this is the CINVAR
/// product page; owner/AGY replace it with the checkout URL when selling starts.
/// Approved CINVAR Pro Early Access purchase route (the storefront page owns the live checkout link, so the
/// app never has to change when the payment link does).
pub const UPGRADE_URL: &str = "https://elyraon.com.co/cinvar/#pricing";

/// One row of the Free-vs-Pro comparison shown in the UI. Only implemented capabilities appear.
pub struct CatalogEntry {
    pub capability: Feature,
    pub title: &'static str,
    pub description: &'static str,
    /// Needs the separately licensed Pro module linked into the binary (not just a license).
    pub needs_pro_module: bool,
    pub experimental: bool,
}

pub const CATALOG: &[CatalogEntry] = &[
    CatalogEntry { capability: Feature::Export4k, title: "4K export", description: "Export up to 4096x2160. Free exports up to 2048x1080.", needs_pro_module: false, experimental: false },
    CatalogEntry { capability: Feature::Export8k, title: "8K and larger export", description: "Export above 4K, up to the engine limit (needs a machine with enough RAM; see the 8K guide).", needs_pro_module: false, experimental: false },
    CatalogEntry { capability: Feature::RenderBatch, title: "Batch rendering", description: "Queue several exports with different sizes and codecs; a failure does not stop the rest, finished files are kept, and you can cancel.", needs_pro_module: true, experimental: false },
    CatalogEntry { capability: Feature::ExportPresets, title: "Export presets", description: "Save and reuse resolution, frame rate, codec, bitrate/quality and format. Built-ins included.", needs_pro_module: true, experimental: false },
    CatalogEntry { capability: Feature::ColorAdvanced, title: "Advanced color", description: "Exposure, white balance, tint, contrast, saturation, gamma and tone curves per clip, plus reusable grade presets.", needs_pro_module: true, experimental: false },
    CatalogEntry { capability: Feature::CloudRender, title: "Cloud render (experimental)", description: "Render on a rented cloud GPU. Experimental: only tested against a simulator, never against a real account.", needs_pro_module: false, experimental: true },
];

impl Entitlements {
    /// The comparison table with this machine's state: `entitled` (license says yes),
    /// `available` (the code is in this binary), `unlocked` (both).
    pub fn catalog_status(&self) -> Vec<serde_json::Value> {
        let pro_module = modules().contains(&"pro");
        CATALOG
            .iter()
            .map(|c| {
                let entitled = self.includes(c.capability) || self.enforcement == Enforcement::Observe;
                let available = !c.needs_pro_module || pro_module;
                serde_json::json!({
                    "capability": c.capability.as_str(), "title": c.title, "description": c.description,
                    "experimental": c.experimental, "pro": true,
                    "entitled": entitled, "available": available, "unlocked": entitled && available,
                    "reason": if !entitled { "Requires a CINVAR Pro license" } else if !available { "Your license is valid, but this download does not include the Pro module; install the CINVAR Pro package" } else { "" },
                })
            })
            .collect()
    }
}

/// Release builds enforce. The `edition-observe` cargo feature (test/dev builds only) flips the
/// default to observe so the pre-existing test suite, which renders 4K/8K without a license, keeps
/// running. There is deliberately no runtime switch to *loosen* enforcement.
pub const ENFORCE_BY_DEFAULT: bool = !cfg!(feature = "edition-observe");

/// `config.edition.enforce = true` can only tighten (useful for tests in observe builds).
pub fn enforcement_for(by_default: bool, config_enforce: bool) -> Enforcement {
    if by_default || config_enforce {
        Enforcement::Enforce
    } else {
        Enforcement::Observe
    }
}

/// Resolves entitlements from the build default, `config.edition` and the license file under the
/// config dir. Never fails: an unreadable or invalid license yields Free with the reason recorded.
pub fn resolve(config: &crate::config::Config) -> Entitlements {
    let enforcement = enforcement_for(ENFORCE_BY_DEFAULT, config.edition.enforce);
    resolve_from(
        enforcement,
        &license::default_path(),
        license::production_keys(),
    )
}

/// Resolution with an explicit license path and trusted keys (tests, tooling).
pub fn resolve_from(
    enforcement: Enforcement,
    license_path: &std::path::Path,
    keys: &[license::PublicKey],
) -> Entitlements {
    match license::load(license_path, keys) {
        Ok(None) => Entitlements::free(enforcement),
        Ok(Some(lic)) => Entitlements::pro(enforcement, &lic.id, lic.grandfathered),
        Err(e) => {
            Entitlements::free(enforcement).with_source(Source::Rejected { reason: e.message })
        }
    }
}

/// Loads config from disk and resolves. Used where no config is in hand (render planning for a
/// request that carries no explicit entitlements). A missing/bad config means defaults.
pub fn resolve_from_disk() -> Arc<Entitlements> {
    let config = crate::config::Config::load().unwrap_or_default();
    Arc::new(resolve(&config))
}

/// Where an activation request comes from.
#[derive(Debug, Clone)]
pub enum ActivationSource {
    /// A license `.json` file on disk (attachment from the order email).
    File(std::path::PathBuf),
    /// An order key, or a license envelope pasted as text.
    OrderKey(String),
}

/// Result of a successful activation.
#[derive(Debug, Clone, Serialize)]
pub struct ActivatedLicense {
    pub id: String,
    pub edition: String,
    pub order_ref: String,
    pub path: String,
}

/// Install a license next to the config so this installation runs as Pro from the next
/// entitlement resolution on (there is no cache, so no restart is needed).
pub fn activate(source: ActivationSource) -> Result<ActivatedLicense> {
    activate_with(source, None)
}

/// [`activate`] with an explicit redeem URL (a CLI override or a test stub), which takes
/// precedence over the config file and the environment. See [`pick_redeem_url`].
pub fn activate_with(
    source: ActivationSource,
    redeem_url_override: Option<&str>,
) -> Result<ActivatedLicense> {
    let keys = license::production_keys();
    let dest = license::default_path();
    let verified = match source {
        ActivationSource::File(path) => {
            if !path.is_file() {
                return Err(Error::invalid(format!("license file not found: {}", path.display())));
            }
            license::install(&path, &dest, keys)?
        }
        ActivationSource::OrderKey(raw) => {
            let text = raw.trim();
            if text.starts_with('{') {
                license::install_text(text, &dest, keys)?
            } else {
                redeem_order_key(text, &dest, keys, redeem_url_override)?
            }
        }
    };
    Ok(ActivatedLicense {
        id: verified.id,
        edition: Edition::Pro.as_str().to_string(),
        order_ref: verified.order_ref,
        path: dest.to_string_lossy().to_string(),
    })
}

/// The CINVAR entitlement service that exchanges an order key for a signed license envelope.
///
/// **Deliberately `None`**: no such service exists in this repository as of 2026-11. Discovery
/// (`scripts/release*.sh`, `scripts/publish_*.sh`, `deploy/`, `docs/`, `CINVAR_FREE_PRO_PLAN.md`,
/// `docs/LICENSE_OPERATIONS.md`, `CINVAR_STOREFRONT_HANDOFF.md`, every URL in the tree) found only
/// the storefront page in [`UPGRADE_URL`]: licenses are delivered by email today, and the handoff
/// doc records the checkout/fulfilment path as still missing. Guessing a host here would turn
/// every customer's `activate` into a DNS error, so order-key redemption stays unconfigured until
/// an endpoint is published, and reports that honestly instead of pretending the key was checked.
pub const DEFAULT_REDEEM_URL: Option<&str> = None;

/// Environment variable naming the redemption endpoint.
pub const REDEEM_URL_ENV: &str = "CINVAR_LICENSE_REDEEM_URL";

const REDEEM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
/// A license envelope is at most 16 KiB ([`license`]); allow generous JSON framing and refuse to
/// buffer more than this.
const MAX_REDEEM_BODY: usize = 64 * 1024;

/// Which endpoint to redeem against, in precedence order: an explicit CLI/API value, then
/// `[edition] redeem_url` from the config file, then [`REDEEM_URL_ENV`], then
/// [`DEFAULT_REDEEM_URL`]. Blank and whitespace-only values count as unset, so an empty variable
/// cannot silently disable a configured service.
pub fn pick_redeem_url(
    explicit: Option<&str>,
    config_url: Option<&str>,
    env_url: Option<&str>,
) -> Option<String> {
    [explicit, config_url, env_url]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| DEFAULT_REDEEM_URL.map(str::to_string))
}

/// [`pick_redeem_url`] with this process's environment and config file filled in.
pub fn redeem_url(explicit: Option<&str>) -> Option<String> {
    let from_env = std::env::var(REDEEM_URL_ENV).ok();
    let from_config = crate::config::Config::load()
        .ok()
        .map(|c| c.edition.redeem_url);
    pick_redeem_url(explicit, from_config.as_deref(), from_env.as_deref())
}

/// Host part of `url` for messages: never the path, the query or a `user:pass@` prefix, so a token
/// in the URL cannot reach a log or an error report.
fn redeem_host(url: &str) -> String {
    let authority = url.split("://").nth(1).unwrap_or(url);
    let authority = authority.split(['/', '?', '#']).next().unwrap_or(authority);
    let host = authority.rsplit('@').next().unwrap_or(authority);
    if host.is_empty() {
        "the configured license service".to_string()
    } else {
        host.to_string()
    }
}

/// One distinct, actionable error per failure mode. The order key never appears in any of them.
fn redeem_http_error(status: u16, host: &str) -> Error {
    let (code, message) = match status {
        400 => (
            ErrorCode::InvalidInput,
            format!("the CINVAR license service rejected the request ({host}, HTTP 400); check the order key from your order email"),
        ),
        401 | 403 => (
            ErrorCode::PermissionDenied,
            format!("the CINVAR license service refused this request ({host}, HTTP {status}); contact support"),
        ),
        404 => (
            ErrorCode::NotFound,
            format!("unknown order key: the CINVAR license service has no license for this key ({host}, HTTP 404); check the key from your order email, or activate with its license.json using --file"),
        ),
        409 => (
            ErrorCode::Conflict,
            format!("this order key has already been redeemed ({host}, HTTP 409); use the license.json from your order email, or contact support to re-issue it"),
        ),
        410 => (
            ErrorCode::NotFound,
            format!("this order key was revoked or expired ({host}, HTTP 410); contact support for a re-issue"),
        ),
        412 => (
            ErrorCode::Unsupported,
            format!("this order key is not eligible for CINVAR Pro ({host}, HTTP 412); contact support"),
        ),
        429 => (
            ErrorCode::Provider,
            format!("the CINVAR license service is rate limiting this machine ({host}, HTTP 429); wait a minute and retry — nothing was installed"),
        ),
        s if (500..=599).contains(&s) => (
            ErrorCode::Provider,
            format!("the CINVAR license service failed ({host}, HTTP {s}); nothing was installed and any license already on this machine is unchanged — retry later"),
        ),
        s => (
            ErrorCode::Protocol,
            format!("the CINVAR license service returned an unexpected status ({host}, HTTP {s}); nothing was installed"),
        ),
    };
    Error::new(code, message)
}

/// Exchanges `order_key` for a license envelope at exactly `url`, verifies it against `keys` and
/// installs it atomically. The order key is never logged and never appears in an error message.
///
/// Nothing is written unless the response verifies ([`license::install_text`] verifies first and
/// replaces by rename), so a bad response can neither replace a working license nor leave a
/// partial file behind.
pub fn redeem_order_key_from(
    order_key: &str,
    url: &str,
    dest: &std::path::Path,
    keys: &[license::PublicKey],
) -> Result<license::VerifiedLicense> {
    use std::io::Read;

    let key = order_key.trim();
    if key.is_empty() {
        return Err(Error::invalid("order key is empty"));
    }
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(Error::invalid(format!(
            "license service URL must be an http(s) URL, not '{}'",
            url.chars().take(48).collect::<String>()
        )));
    }
    let host = redeem_host(url);
    let response = match ureq::post(url)
        .timeout(REDEEM_TIMEOUT)
        .set("accept", "application/json")
        .send_json(serde_json::json!({ "order_key": key }))
    {
        Ok(response) => response,
        Err(ureq::Error::Status(status, _)) => return Err(redeem_http_error(status, &host)),
        Err(ureq::Error::Transport(transport)) => {
            return Err(Error::new(
                ErrorCode::Provider,
                format!("could not reach the CINVAR license service at {host} ({:?}); nothing was installed — retry, or activate with the license.json from your order email using --file", transport.kind()),
            ))
        }
    };
    let mut body = String::new();
    response
        .into_reader()
        .take(MAX_REDEEM_BODY as u64 + 1)
        .read_to_string(&mut body)
        .map_err(|e| {
            Error::new(
                ErrorCode::Provider,
                format!("could not read the response from {host} ({e}); nothing was installed"),
            )
        })?;
    let body = body.trim();
    if body.len() > MAX_REDEEM_BODY {
        return Err(Error::new(
            ErrorCode::Protocol,
            format!("the CINVAR license service returned an oversized response ({host}); nothing was installed"),
        ));
    }
    if body.is_empty() {
        return Err(Error::new(
            ErrorCode::Protocol,
            format!("the CINVAR license service returned an empty response ({host}); nothing was installed"),
        ));
    }
    license::install_text(body, dest, keys).map_err(|e| {
        Error::new(
            e.code,
            format!("the CINVAR license service returned no usable license: {}", e.message),
        )
    })
}

/// Resolves the endpoint (see [`pick_redeem_url`]) and redeems. With no configured URL this fails
/// honestly instead of contacting a guessed host.
fn redeem_order_key(
    order_key: &str,
    dest: &std::path::Path,
    keys: &[license::PublicKey],
    explicit_url: Option<&str>,
) -> Result<license::VerifiedLicense> {
    let url = redeem_url(explicit_url).ok_or_else(|| {
        Error::invalid(format!(
            "order-key activation needs a license service URL and none is deployed or configured: set `[edition] redeem_url` in the config file or export {REDEEM_URL_ENV}; you can also activate with the license.json from your order email using --file"
        ))
    })?;
    redeem_order_key_from(order_key, &url, dest, keys)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiers_classify_sizes() {
        use Feature::*;
        for (w, h, t) in [
            (1280, 720, Export2k),
            (1920, 1080, Export2k),
            (2048, 1080, Export2k),
            (1080, 2048, Export2k),
            (1600, 1200, Export2k),
            (1080, 1920, Export2k),
            (2048, 1152, Export4k),
            (2560, 1440, Export4k),
            (1440, 2560, Export4k),
            (3840, 2160, Export4k),
            (2160, 3840, Export4k),
            (4096, 2160, Export4k),
            (3840, 1080, Export4k),
            (4096, 2304, Export8k),
            (7680, 4320, Export8k),
            (2049, 1080, Export4k),
        ] {
            assert_eq!(export_tier(w, h), t, "{w}x{h}");
        }
    }

    #[test]
    fn free_limit_is_2048x1080_in_either_orientation() {
        let e = Entitlements::free(Enforcement::Enforce);
        for (w, h) in [
            (1280, 720),
            (1920, 1080),
            (2048, 1080),
            (1080, 2048),
            (1080, 1920),
            (16, 16),
        ] {
            assert!(e.check_export(w, h).is_ok(), "{w}x{h}");
        }
        for (w, h) in [
            (2049, 1080),
            (2560, 1440),
            (1440, 2560),
            (3840, 2160),
            (7680, 4320),
            (2048, 1152),
        ] {
            assert!(e.check_export(w, h).is_err(), "{w}x{h}");
        }
    }

    #[test]
    fn config_can_tighten_but_never_loosen() {
        assert_eq!(enforcement_for(true, false), Enforcement::Enforce);
        assert_eq!(enforcement_for(true, true), Enforcement::Enforce);
        assert_eq!(enforcement_for(false, true), Enforcement::Enforce);
        assert_eq!(enforcement_for(false, false), Enforcement::Observe);
        assert_eq!(ENFORCE_BY_DEFAULT, !cfg!(feature = "edition-observe"));
    }

    #[test]
    fn fit_export_prefers_the_1080p_envelope_and_keeps_aspect() {
        let e = Entitlements::free(Enforcement::Enforce);
        assert_eq!(e.fit_export(3840, 2160), Some((1920, 1080)));
        assert_eq!(e.fit_export(2160, 3840), Some((1080, 1920)));
        assert_eq!(e.fit_export(7680, 4320), Some((1920, 1080)));
        assert_eq!(e.fit_export(4000, 3000), Some((1440, 1080)));
        assert_eq!(e.fit_export(3840, 1646), Some((1920, 822)));
        assert_eq!(e.fit_export(1920, 1080), None);
        assert_eq!(e.fit_export(2048, 1080), None);
        for (w, h) in [(3840, 2160), (5120, 2880), (4096, 4096), (16384, 16)] {
            if let Some((fw, fh)) = e.fit_export(w, h) {
                assert!(e.check_export(fw, fh).is_ok(), "{w}x{h} -> {fw}x{fh}");
            }
        }
        assert_eq!(
            Entitlements::pro(Enforcement::Enforce, "t", false).fit_export(3840, 2160),
            None
        );
    }

    #[test]
    fn observe_mode_never_refuses() {
        let e = Entitlements::free(Enforcement::Observe);
        assert!(e.check_export(7680, 4320).is_ok());
        assert!(e.check_feature(Feature::CloudRender).is_ok());
    }

    #[test]
    fn pro_is_unrestricted_and_free_is_not() {
        let pro = Entitlements::pro(Enforcement::Enforce, "t", false);
        assert!(pro.check_export(7680, 4320).is_ok());
        assert!(pro.check_export(3840, 2160).is_ok());
        assert!(pro.check_feature(Feature::CloudRender).is_ok());
        let free = Entitlements::free(Enforcement::Enforce);
        assert!(free.check_feature(Feature::CloudRender).is_err());
        assert_eq!(free.export_limit(), "2048x1080");
        assert_eq!(pro.export_limit(), "engine limit");
    }

    #[test]
    fn tool_table_only_lists_enforced_features() {
        assert_eq!(
            tool_feature("cinvar.render.cloud"),
            Some(Feature::CloudRender)
        );
        assert_eq!(tool_feature("cinvar.render.export"), None);
        assert_eq!(tool_feature("cinvar.timeline.trim"), None);
    }

    #[test]
    fn feature_names_round_trip_and_are_dotted() {
        for f in Feature::ALL {
            assert_eq!(Feature::parse(f.as_str()), Some(*f));
            assert!(f.as_str().contains('.'));
            assert_eq!(
                serde_json::to_value(f).unwrap(),
                serde_json::json!(f.as_str())
            );
        }
        assert_eq!(Feature::parse("nope"), None);
    }

    #[test]
    fn preview_grants_are_only_for_capabilities_that_can_be_shortened() {
        use super::*;
        let free = Entitlements::free(Enforcement::Enforce);
        // A gate that honours the flag returns Preview instead of refusing...
        assert_eq!(free.gate_export(3840, 2160, true).unwrap(), Grant::Preview);
        assert_eq!(
            free.gate_feature(Feature::ProCodecs, true).unwrap(),
            Grant::Preview
        );
        assert_eq!(
            free.gate_feature(Feature::EnhanceUpscale, true).unwrap(),
            Grant::Preview
        );
        // ...but the strict gates never do: nothing downstream can shorten a cloud render.
        let err = free.gate_feature(Feature::CloudRender, true).unwrap_err();
        assert_eq!(err.code, ErrorCode::PermissionDenied);
        assert!(!feature_supports_preview(Feature::CloudRender));
        assert!(!feature_supports_preview(Feature::BlenderProduction));
        assert!(err.to_string().contains("Pro feature"), "{err}");
        // Without the flag the message still names --pro-preview as the way to try it.
        let err = free.gate_feature(Feature::ProCodecs, false).unwrap_err();
        assert!(err.to_string().contains("--pro-preview"), "{err}");
        // Observe mode grants everything as Included, never as Preview.
        let observe = Entitlements::free(Enforcement::Observe);
        assert_eq!(
            observe.gate_export(7680, 4320, true).unwrap(),
            Grant::Included
        );
        assert_eq!(
            observe.gate_feature(Feature::ProCodecs, true).unwrap(),
            Grant::Included
        );
    }

    #[test]
    fn included_capabilities_are_granted_normally() {
        use super::*;
        let pro = Entitlements::pro(Enforcement::Enforce, "t", false);
        assert_eq!(pro.gate_export(7680, 4320, true).unwrap(), Grant::Included);
        assert_eq!(
            pro.gate_feature(Feature::ProCodecs, true).unwrap(),
            Grant::Included
        );
        let free = Entitlements::free(Enforcement::Enforce);
        assert_eq!(free.gate_export(1920, 1080, false).unwrap(), Grant::Included);
    }

    #[test]
    fn semantic_cap_states_the_free_allowance_and_the_way_out() {
        use super::*;
        let free = Entitlements::free(Enforcement::Enforce);
        assert!(free.check_semantic_clips(SEMANTIC_FREE_CLIP_LIMIT).is_ok());
        let err = free
            .check_semantic_clips(SEMANTIC_FREE_CLIP_LIMIT + 1)
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::PermissionDenied);
        assert!(err.message.contains("10 clips"), "{}", err.message);
        assert!(err.to_string().contains("cinvar activate"), "{err}");
        let pro = Entitlements::pro(Enforcement::Enforce, "t", false);
        assert!(pro.check_semantic_clips(100_000).is_ok());
    }

    #[test]
    fn license_file_drives_resolution() {
        use license::testing::*;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(license::LICENSE_FILE);
        let keys = [test_key()];
        // none -> Free
        let e = resolve_from(Enforcement::Enforce, &p, &keys);
        assert_eq!(e.edition, Edition::Free);
        assert!(e.check_export(3840, 2160).is_err());
        // valid (incl. a grandfathered pre-split license) -> Pro
        std::fs::write(&p, mint("L-1", "pro", true)).unwrap();
        let e = resolve_from(Enforcement::Enforce, &p, &keys);
        assert_eq!(e.edition, Edition::Pro);
        assert!(matches!(
            e.source,
            Source::License {
                grandfathered: true,
                ..
            }
        ));
        assert!(e.check_export(7680, 4320).is_ok());
        // forged / wrong key -> Free with a reason, never Pro
        std::fs::write(&p, mint("L-1", "pro", true)).unwrap();
        let e = resolve_from(Enforcement::Enforce, &p, &[]);
        assert_eq!(e.edition, Edition::Free);
        assert!(matches!(e.source, Source::Rejected { .. }));
        std::fs::write(&p, "garbage").unwrap();
        assert_eq!(
            resolve_from(Enforcement::Enforce, &p, &keys).edition,
            Edition::Free
        );
    }
}

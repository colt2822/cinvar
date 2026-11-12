use serde::{Deserialize, Serialize};

use crate::util::ids;
use crate::util::time::{Fps, Rational, Time};

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AssetKind {
    Video,
    Audio,
    Image,
    Font,
    Other,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Orientation {
    Landscape,
    Portrait,
    Square,
}

impl Orientation {
    pub fn from_size(w: u32, h: u32) -> Orientation {
        if w > h {
            Orientation::Landscape
        } else if h > w {
            Orientation::Portrait
        } else {
            Orientation::Square
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct MediaInfo {
    pub duration: Time,
    pub format: String,
    pub width: u32,
    pub height: u32,
    pub fps: Option<Fps>,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub sample_rate: u32,
    pub channels: u16,
    pub bitrate: u64,
    pub has_video: bool,
    pub has_audio: bool,
    pub rotation: i32,
    pub nb_frames: Option<u64>,
}

impl MediaInfo {
    pub fn orientation(&self) -> Orientation {
        Orientation::from_size(self.width, self.height)
    }
    pub fn aspect(&self) -> f64 {
        if self.height == 0 {
            0.0
        } else {
            self.width as f64 / self.height as f64
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Asset {
    pub id: String,
    pub path: String,
    pub kind: AssetKind,
    pub bytes: u64,
    pub imported_at: String,
    pub sha256: String,
    #[serde(default)]
    pub info: Option<MediaInfo>,
    #[serde(default)]
    pub missing: bool,
    #[serde(default)]
    pub provenance: Option<String>,
}

impl Asset {
    pub fn display_name(&self) -> String {
        std::path::Path::new(&self.path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| self.path.clone())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    Video,
    Audio,
    Caption,
    Overlay,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Fit {
    #[default]
    Contain,
    Cover,
    Stretch,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Transform {
    pub scale: f64,
    pub x: i32,
    pub y: i32,
    pub rotation_deg: f64,
    pub opacity: f64,
    pub fit: Fit,
}

impl Default for Transform {
    fn default() -> Self {
        Transform {
            scale: 1.0,
            x: 0,
            y: 0,
            rotation_deg: 0.0,
            opacity: 1.0,
            fit: Fit::Cover,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioClip {
    pub gain_db: f64,
    pub fade_in: Time,
    pub fade_out: Time,
    pub muted: bool,
    pub normalize: bool,
    pub duck_under_speech: bool,
    pub offset: Time,
}

impl Default for AudioClip {
    fn default() -> Self {
        AudioClip {
            gain_db: 0.0,
            fade_in: Time::ZERO,
            fade_out: Time::ZERO,
            muted: false,
            normalize: false,
            duck_under_speech: false,
            offset: Time::ZERO,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Effect {
    pub id: String,
    pub kind: String,
    pub params: serde_json::Value,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transition {
    pub kind: String,
    pub duration: Time,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TextPosition {
    Top,
    Center,
    Bottom,
    LowerThird,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TextAlign {
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TextOverlay {
    pub text: String,
    pub template: String,
    pub font: Option<String>,
    pub size: u32,
    pub color: String,
    pub outline_color: String,
    pub outline_width: u32,
    pub bg_color: Option<String>,
    pub position: TextPosition,
    pub align: TextAlign,
    pub max_width_pct: f64,
    pub animate_in: Option<String>,
    pub animate_out: Option<String>,
}

impl Default for TextOverlay {
    fn default() -> Self {
        TextOverlay {
            text: String::new(),
            template: "default".into(),
            font: None,
            size: 64,
            color: "white".into(),
            outline_color: "black".into(),
            outline_width: 3,
            bg_color: None,
            position: TextPosition::Bottom,
            align: TextAlign::Center,
            max_width_pct: 0.85,
            animate_in: None,
            animate_out: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Keyframe {
    pub at: Time,
    pub value: f64,
    pub easing: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyframeTrack {
    pub property: String,
    pub keyframes: Vec<Keyframe>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransitionSide {
    In,
    Out,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Clip {
    pub id: String,
    pub asset: Option<String>,
    pub name: String,
    pub timeline_start: Time,
    pub source_in: Time,
    pub source_out: Time,
    pub speed: f64,
    pub reversed: bool,
    pub freeze_at: Option<Time>,
    pub effects: Vec<Effect>,
    pub transition_in: Option<Transition>,
    pub transition_out: Option<Transition>,
    pub audio: AudioClip,
    pub text: Option<TextOverlay>,
    pub transform: Transform,
    pub keyframes: Vec<KeyframeTrack>,
}

impl Default for Clip {
    fn default() -> Self {
        Clip {
            id: ids::new_id("clip"),
            asset: None,
            name: String::new(),
            timeline_start: Time::ZERO,
            source_in: Time::ZERO,
            source_out: Time::from_secs_f64(5.0),
            speed: 1.0,
            reversed: false,
            freeze_at: None,
            effects: Vec::new(),
            transition_in: None,
            transition_out: None,
            audio: AudioClip::default(),
            text: None,
            transform: Transform::default(),
            keyframes: Vec::new(),
        }
    }
}

impl Clip {
    pub fn source_duration(&self) -> Time {
        (self.source_out - self.source_in).clamp_non_negative()
    }

    pub fn duration(&self) -> Time {
        let speed = if self.speed <= 0.0 { 1.0 } else { self.speed };
        self.source_duration().scale(1.0 / speed)
    }

    pub fn timeline_end(&self) -> Time {
        self.timeline_start + self.duration()
    }

    pub fn range(&self) -> crate::util::time::TimeRange {
        crate::util::time::TimeRange::new(self.timeline_start, self.timeline_end())
    }

    pub fn is_text(&self) -> bool {
        self.text.is_some()
    }

    pub fn keyframe_track(&self, property: &str) -> Option<&KeyframeTrack> {
        self.keyframes.iter().find(|k| k.property == property)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Track {
    pub id: String,
    pub name: String,
    pub kind: TrackKind,
    pub z: i32,
    pub muted: bool,
    pub hidden: bool,
    pub locked: bool,
    pub gain_db: f64,
    pub clips: Vec<Clip>,
}

impl Default for Track {
    fn default() -> Self {
        Track {
            id: ids::new_id("track"),
            name: "Track".into(),
            kind: TrackKind::Video,
            z: 0,
            muted: false,
            hidden: false,
            locked: false,
            gain_db: 0.0,
            clips: Vec::new(),
        }
    }
}

impl Track {
    pub fn sorted_clips(&self) -> Vec<&Clip> {
        let mut clips: Vec<&Clip> = self.clips.iter().collect();
        clips.sort_by_key(|c| c.timeline_start.nanos);
        clips
    }
    pub fn end_time(&self) -> Time {
        self.clips
            .iter()
            .map(|c| c.timeline_end())
            .fold(Time::ZERO, |a, b| a.max(b))
    }
    pub fn clip_at(&self, at: Time) -> Option<&Clip> {
        self.clips.iter().find(|c| c.range().contains(at))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Timeline {
    pub id: String,
    pub name: String,
    pub fps: Fps,
    pub width: u32,
    pub height: u32,
    pub sample_rate: u32,
    pub background: String,
    pub tracks: Vec<Track>,
}

impl Default for Timeline {
    fn default() -> Self {
        Timeline {
            id: ids::new_id("timeline"),
            name: "Main".into(),
            fps: Rational::new(30, 1),
            width: 1920,
            height: 1080,
            sample_rate: 48_000,
            background: "black".into(),
            tracks: Vec::new(),
        }
    }
}

impl Timeline {
    pub fn duration(&self) -> Time {
        self.tracks
            .iter()
            .map(|t| t.end_time())
            .fold(Time::ZERO, |a, b| a.max(b))
    }
    pub fn duration_frames(&self) -> i64 {
        self.duration().to_frames(self.fps)
    }
    pub fn track(&self, id: &str) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }
    pub fn track_mut(&mut self, id: &str) -> Option<&mut Track> {
        self.tracks.iter_mut().find(|t| t.id == id)
    }
    pub fn tracks_of(&self, kind: TrackKind) -> Vec<&Track> {
        self.tracks.iter().filter(|t| t.kind == kind).collect()
    }
    pub fn has_audio(&self) -> bool {
        self.tracks
            .iter()
            .any(|t| t.kind == TrackKind::Audio && t.clips.iter().any(|c| !c.audio.muted))
    }
    pub fn all_clips(&self) -> Vec<(&Track, &Clip)> {
        let mut out = Vec::new();
        for track in &self.tracks {
            for clip in &track.clips {
                out.push((track, clip));
            }
        }
        out
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CaptionStyle {
    pub font: Option<String>,
    pub size: u32,
    pub color: String,
    pub outline_color: String,
    pub outline_width: u32,
    pub position: TextPosition,
    pub max_chars_per_line: u32,
    pub uppercase: bool,
    /// Box colour behind each caption, e.g. `black@0.6`, for readability over busy footage.
    pub background: Option<String>,
}

impl Default for CaptionStyle {
    fn default() -> Self {
        CaptionStyle {
            font: None,
            size: 48,
            color: "white".into(),
            outline_color: "black".into(),
            outline_width: 4,
            position: TextPosition::Bottom,
            max_chars_per_line: 28,
            uppercase: false,
            background: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cue {
    pub start: Time,
    pub end: Time,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CaptionTrack {
    pub id: String,
    pub language: String,
    pub cues: Vec<Cue>,
    pub style: CaptionStyle,
    pub source: String,
}

impl Default for CaptionTrack {
    fn default() -> Self {
        CaptionTrack {
            id: ids::new_id("captions"),
            language: "en".into(),
            cues: Vec::new(),
            style: CaptionStyle::default(),
            source: "manual".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Marker {
    pub at: Time,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanRecord {
    pub at: String,
    pub model: Option<String>,
    pub request: String,
    pub plan: serde_json::Value,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HwMode {
    Auto,
    Off,
    Force,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RenderSettings {
    pub width: u32,
    pub height: u32,
    pub fps: String,
    pub format: String,
    pub video_codec: String,
    pub crf: u8,
    pub preset: String,
    pub audio_codec: String,
    pub audio_bitrate: String,
    pub hardware: HwMode,
    pub loudness_target: f64,
}

impl Default for RenderSettings {
    fn default() -> Self {
        RenderSettings {
            width: 1920,
            height: 1080,
            fps: "30".into(),
            format: "mp4".into(),
            video_codec: "h264".into(),
            crf: 20,
            preset: "medium".into(),
            audio_codec: "aac".into(),
            audio_bitrate: "192k".into(),
            hardware: HwMode::Auto,
            loudness_target: -14.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectSettings {
    pub autosave_seconds: u64,
    pub snapshot_limit: usize,
    pub allow_network_upload: bool,
}

impl Default for ProjectSettings {
    fn default() -> Self {
        ProjectSettings {
            autosave_seconds: 120,
            snapshot_limit: 60,
            allow_network_upload: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct StyleProfile {
    pub mood: Option<String>,
    pub notes: Vec<String>,
    pub caption_style: CaptionStyle,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Project {
    pub schema: u32,
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub updated_at: String,
    pub revision: u64,
    pub settings: ProjectSettings,
    pub style: StyleProfile,
    pub assets: Vec<Asset>,
    pub timelines: Vec<Timeline>,
    pub active_timeline: Option<String>,
    pub captions: Vec<CaptionTrack>,
    pub markers: Vec<Marker>,
    pub plans: Vec<PlanRecord>,
    pub notes: Vec<String>,
    pub render: RenderSettings,
}

impl Default for Project {
    fn default() -> Self {
        let now = crate::util::clock::now_rfc3339();
        Project {
            schema: SCHEMA_VERSION,
            id: ids::new_id("proj"),
            name: "Untitled".into(),
            created_at: now.clone(),
            updated_at: now,
            revision: 0,
            settings: ProjectSettings::default(),
            style: StyleProfile::default(),
            assets: Vec::new(),
            timelines: Vec::new(),
            active_timeline: None,
            captions: Vec::new(),
            markers: Vec::new(),
            plans: Vec::new(),
            notes: Vec::new(),
            render: RenderSettings::default(),
        }
    }
}

impl Project {
    pub fn new(name: &str) -> Project {
        Project {
            name: name.to_string(),
            ..Project::default()
        }
    }

    pub fn asset(&self, id: &str) -> Option<&Asset> {
        self.assets.iter().find(|a| a.id == id)
    }

    pub fn asset_by_path(&self, path: &str) -> Option<&Asset> {
        self.assets.iter().find(|a| a.path == path)
    }

    pub fn timeline(&self) -> Option<&Timeline> {
        match &self.active_timeline {
            Some(id) => self
                .timelines
                .iter()
                .find(|t| &t.id == id)
                .or_else(|| self.timelines.first()),
            None => self.timelines.first(),
        }
    }

    pub fn timeline_mut(&mut self) -> Option<&mut Timeline> {
        let id = self.active_timeline.clone();
        match id {
            Some(id) => {
                if self.timelines.iter().any(|t| t.id == id) {
                    self.timelines.iter_mut().find(|t| t.id == id)
                } else {
                    self.timelines.first_mut()
                }
            }
            None => self.timelines.first_mut(),
        }
    }

    pub fn timeline_by_id(&self, id: &str) -> Option<&Timeline> {
        self.timelines.iter().find(|t| t.id == id)
    }

    pub fn timeline_by_id_mut(&mut self, id: &str) -> Option<&mut Timeline> {
        self.timelines.iter_mut().find(|t| t.id == id)
    }

    pub fn find_clip(&self, clip_id: &str) -> Option<(&Timeline, &Track, &Clip)> {
        for timeline in &self.timelines {
            for track in &timeline.tracks {
                if let Some(clip) = track.clips.iter().find(|c| c.id == clip_id) {
                    return Some((timeline, track, clip));
                }
            }
        }
        None
    }

    pub fn clip(&self, clip_id: &str) -> Option<&Clip> {
        self.find_clip(clip_id).map(|(_, _, clip)| clip)
    }

    pub fn clip_location(&self, clip_id: &str) -> Option<(usize, usize, usize)> {
        for (ti, timeline) in self.timelines.iter().enumerate() {
            for (ri, track) in timeline.tracks.iter().enumerate() {
                if let Some(ci) = track.clips.iter().position(|c| c.id == clip_id) {
                    return Some((ti, ri, ci));
                }
            }
        }
        None
    }

    pub fn clip_mut(&mut self, clip_id: &str) -> Option<&mut Clip> {
        let (ti, ri, ci) = self.clip_location(clip_id)?;
        Some(&mut self.timelines[ti].tracks[ri].clips[ci])
    }

    pub fn track_of_clip(&self, clip_id: &str) -> Option<&Track> {
        let (ti, ri, _) = self.clip_location(clip_id)?;
        Some(&self.timelines[ti].tracks[ri])
    }

    pub fn remove_clip_by_id(&mut self, clip_id: &str) -> bool {
        for timeline in self.timelines.iter_mut() {
            for track in timeline.tracks.iter_mut() {
                if let Some(pos) = track.clips.iter().position(|c| c.id == clip_id) {
                    track.clips.remove(pos);
                    return true;
                }
            }
        }
        false
    }

    pub fn add_asset(&mut self, asset: Asset) -> String {
        let id = asset.id.clone();
        self.assets.push(asset);
        id
    }

    pub fn missing_assets(&self) -> Vec<&Asset> {
        self.assets.iter().filter(|a| a.missing).collect()
    }
}

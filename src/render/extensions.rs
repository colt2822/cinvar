//! Extension point for effects implemented outside the open core (e.g. the proprietary Pro
//! module). A registered effect turns its parameters into FFmpeg filter steps for one clip.
//! Unregistered effect kinds are skipped with a warning by the renderer, so a project that uses a
//! Pro effect still opens and renders (without that effect) in an edition that lacks it.

use std::collections::BTreeMap;
use std::sync::RwLock;

use serde_json::Value;

/// Returns the filter steps for one effect instance, or a reason it cannot be applied (the
/// renderer then skips it with that reason as a warning).
pub type EffectFilter = fn(&Value) -> std::result::Result<Vec<String>, String>;

static EFFECTS: RwLock<BTreeMap<String, EffectFilter>> = RwLock::new(BTreeMap::new());

pub fn register_effect(kind: &str, filter: EffectFilter) {
    if let Ok(mut map) = EFFECTS.write() {
        map.insert(kind.to_string(), filter);
    }
}

pub fn effect_filter(kind: &str) -> Option<EffectFilter> {
    EFFECTS.read().ok().and_then(|m| m.get(kind).copied())
}

pub fn is_registered(kind: &str) -> bool {
    effect_filter(kind).is_some()
}

//! Client compatibility and permission policy for the MCP server.
//!
//! * Names: CINVAR's native tool names are dotted (`cinvar.render.export`). Some clients/LLM
//!   APIs only accept `[A-Za-z0-9_-]`. The server can advertise "safe" names (dots -> `_`) and
//!   always accepts native names, safe names and the logical aliases below on `tools/call`.
//! * Permissions: the server runs in one of three modes. `read_only` exposes inspection only,
//!   `edit` (the default) adds everything that changes the project or renders locally, `full`
//!   also exposes spending/administration tools. Billable cloud tools
//!   additionally need `security.allow_cloud_render`.

use crate::tools::Registry;

/// Logical capability name -> the tool that implements it. Aliases are accepted on call; they are
/// not listed (listing duplicates would only confuse a model).
pub const ALIASES: &[(&str, &str)] = &[
    ("cinvar.inspect_media", "cinvar.media.inspect"),
    ("cinvar.import_media", "cinvar.media.import"),
    ("cinvar.detect_scenes", "cinvar.media.scenes"),
    ("cinvar.transcribe", "cinvar.captions.transcribe"),
    ("cinvar.generate_captions", "cinvar.captions.from_script"),
    ("cinvar.apply_effect", "cinvar.timeline.effect"),
    ("cinvar.create_blender_scene", "cinvar.blender.animation"),
    ("cinvar.run_ai_workflow", "cinvar.ai.run_workflow"),
    ("cinvar.render", "cinvar.render.export"),
    ("cinvar.render_cloud", "cinvar.render.cloud"),
    ("cinvar.inspect_render", "cinvar.qa.render_check"),
    ("cinvar.export_project", "cinvar.otio.export"),
];

/// Logical capabilities that are NOT a single tool, with where to find them. Reported by
/// `cinvar.system.aliases` so an agent is never told something exists that does not.
pub const NOT_A_SINGLE_TOOL: &[(&str, &str)] = &[
    ("cinvar.edit_timeline", "a family of tools: cinvar.timeline.* (create, insert_clip, trim, split, move, transition, text, transform, keyframe, ...)"),
    ("cinvar.create_project", "not exposed over MCP: the project is chosen when the server starts (cinvar connect / --project)"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Read,
    Edit,
    Spend,
    Admin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Fixed three-tool allowlist for the directory-listed "Project Inspector". Never reachable
    /// from `CINVAR_MCP_PERMISSIONS`; only the dedicated `mcp inspector` entry point sets it.
    Inspector,
    ReadOnly,
    Edit,
    Full,
}

/// The only tools the inspector entry point lists or dispatches (natives; aliases resolve to a
/// native name first, so an alias of anything outside this list is refused too).
pub const INSPECTOR_TOOLS: &[&str] = &[
    "cinvar.project.status",
    "cinvar.qa.project_check",
    "cinvar.render.preflight",
];

impl Mode {
    pub fn parse(s: &str) -> Option<Mode> {
        match s.trim().to_ascii_lowercase().as_str() {
            "read_only" | "readonly" | "read-only" => Some(Mode::ReadOnly),
            "edit" => Some(Mode::Edit),
            "full" => Some(Mode::Full),
            "" => Some(Mode::Edit), // an empty variable must never widen access
            _ => None,
        }
    }

    pub fn from_env() -> Mode {
        match std::env::var("CINVAR_MCP_PERMISSIONS") {
            Ok(v) => Mode::parse(&v).unwrap_or(Mode::ReadOnly), // unknown value: fail closed; unset = edit (no cloud spend / plugin admin unless asked)
            Err(_) => Mode::Edit,
        }
    }

    pub fn allows(self, level: Level) -> bool {
        match self {
            Mode::Inspector => false, // decided per tool by `allows_tool`, never by level
            Mode::ReadOnly => level == Level::Read,
            Mode::Edit => level <= Level::Edit,
            Mode::Full => true,
        }
    }
}

impl Mode {
    /// Whether this mode may list and call the native tool `name`.
    pub fn allows_tool(self, name: &str) -> bool {
        match self {
            Mode::Inspector => INSPECTOR_TOOLS.contains(&name),
            other => other.allows(level_of(name)),
        }
    }
}

/// MCP tool annotations (title + behaviour hints) for the inspector tools. All three only read
/// project state; none deletes or overwrites anything; none reaches the network.
pub fn inspector_annotations(name: &str) -> Option<serde_json::Value> {
    let title = match name {
        "cinvar.project.status" => "Summarize Cinvar project",
        "cinvar.qa.project_check" => "Check Cinvar project for problems",
        "cinvar.render.preflight" => "Check whether the project can be exported",
        _ => return None,
    };
    Some(serde_json::json!({
        "title": title,
        "readOnlyHint": true,
        "destructiveHint": false,
        "idempotentHint": true,
        "openWorldHint": false,
    }))
}

const READ_TOOLS: &[&str] = &[
    "cinvar.project.status",
    "cinvar.project.history",
    "cinvar.media.inspect",
    "cinvar.media.frames",
    "cinvar.media.contact_sheet",
    "cinvar.media.scenes",
    "cinvar.media.silence",
    "cinvar.media.black",
    "cinvar.render.status",
    "cinvar.render.list_jobs",
    "cinvar.render.preflight",
    "cinvar.jobs.list",
    "cinvar.color.inspect",
    "cinvar.blender.status",
    "cinvar.cloud.status",
    "cinvar.plugins.list",
    "cinvar.ai.workflows",
    "cinvar.system.aliases",
];

/// Classifies a native tool name. Unknown tools are treated as `Admin` (fail closed).
pub fn level_of(name: &str) -> Level {
    if READ_TOOLS.contains(&name)
        || name.starts_with("cinvar.audio.")
        || name.starts_with("cinvar.qa.")
        || name.starts_with("cinvar.system.")
        || name.starts_with("cinvar.skills.")
    {
        return Level::Read;
    }
    match name {
        "cinvar.render.cloud" | "cinvar.cloud.cleanup" => Level::Spend,
        "cinvar.plugins.set_enabled" | "cinvar.plugins.pin" => Level::Admin,
        _ if name.starts_with("plugin.") => Level::Admin,
        _ if name.starts_with("cinvar.") => Level::Edit,
        _ => Level::Admin,
    }
}

pub fn safe_name(name: &str) -> String {
    name.replace('.', "_")
}

/// Resolve an incoming name (native, alias or safe form) to the native tool name.
pub fn resolve(registry: &Registry, requested: &str) -> Option<String> {
    if registry.contains(requested) {
        return Some(requested.to_string());
    }
    if let Some((_, native)) = ALIASES
        .iter()
        .find(|(a, _)| *a == requested || safe_name(a) == requested)
    {
        if registry.contains(native) {
            return Some((*native).to_string());
        }
    }
    registry
        .specs()
        .iter()
        .find(|s| safe_name(&s.name) == requested)
        .map(|s| s.name.clone())
}

/// Strictest shape check shared by common clients. Returns problems (empty = compatible).
pub fn check_spec(name: &str, description: &str, schema: &serde_json::Value) -> Vec<String> {
    let mut p = Vec::new();
    let safe = safe_name(name);
    if safe.is_empty()
        || safe.len() > 64
        || !safe
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        p.push(format!(
            "{name}: safe form '{safe}' is not 1..64 chars of [A-Za-z0-9_-]"
        ));
    }
    // Claude Code prefixes `mcp__<server>__`; keep the combined name within 64.
    if "mcp__cinvar__".len() + safe.len() > 64 {
        p.push(format!(
            "{name}: too long once a client prefixes the server name"
        ));
    }
    if description.trim().is_empty() {
        p.push(format!("{name}: empty description"));
    }
    if schema["type"] != "object" {
        p.push(format!("{name}: inputSchema.type must be \"object\""));
    }
    if !schema["properties"].is_object() {
        p.push(format!("{name}: inputSchema.properties must be an object"));
    }
    if let (Some(req), Some(props)) = (
        schema["required"].as_array(),
        schema["properties"].as_object(),
    ) {
        for r in req {
            if !r.as_str().map(|k| props.contains_key(k)).unwrap_or(false) {
                p.push(format!("{name}: required '{r}' is not a declared property"));
            }
        }
    }
    let text = schema.to_string();
    if text.contains("\"$ref\"")
        || text.contains("\"oneOf\"")
        || text.contains("\"anyOf\"")
        || text.contains("\"allOf\"")
    {
        p.push(format!(
            "{name}: schema uses $ref/oneOf/anyOf/allOf, which several clients reject"
        ));
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_gate_levels() {
        assert!(Mode::ReadOnly.allows(Level::Read) && !Mode::ReadOnly.allows(Level::Edit));
        assert!(Mode::Edit.allows(Level::Edit) && !Mode::Edit.allows(Level::Spend));
        assert!(Mode::Full.allows(Level::Admin));
        assert_eq!(Mode::parse("garbage"), None);
    }

    #[test]
    fn inspector_is_not_reachable_from_the_environment() {
        assert_eq!(Mode::parse("inspector"), None);
    }

    #[test]
    fn inspector_allows_exactly_three_tools() {
        let registry = crate::tools::build_default();
        let allowed: Vec<_> = registry
            .specs()
            .iter()
            .filter(|s| Mode::Inspector.allows_tool(&s.name))
            .map(|s| s.name.clone())
            .collect();
        assert_eq!(allowed.len(), INSPECTOR_TOOLS.len(), "{allowed:?}");
        for t in INSPECTOR_TOOLS {
            assert!(inspector_annotations(t).is_some());
        }
        assert!(!Mode::Inspector.allows_tool("cinvar.ai.generate_video"));
        assert!(!Mode::Inspector.allows_tool("cinvar.media.inspect"));
    }

    #[test]
    fn classification_fails_closed() {
        assert_eq!(level_of("cinvar.media.inspect"), Level::Read);
        assert_eq!(level_of("cinvar.timeline.trim"), Level::Edit);
        assert_eq!(level_of("cinvar.render.cloud"), Level::Spend);
        assert_eq!(level_of("plugin.x.y"), Level::Admin);
        assert_eq!(level_of("something.unknown"), Level::Admin);
    }

    #[test]
    fn every_registered_tool_is_classified_and_client_compatible() {
        let registry = crate::tools::build_default();
        let mut safe_seen = std::collections::BTreeSet::new();
        let mut problems = Vec::new();
        for spec in registry.specs() {
            problems.extend(check_spec(&spec.name, &spec.description, &spec.input));
            assert!(
                safe_seen.insert(safe_name(&spec.name)),
                "safe-name collision on {}",
                spec.name
            );
        }
        assert!(
            problems.is_empty(),
            "MCP client-compat problems:\n{}",
            problems.join("\n")
        );
    }

    #[test]
    fn aliases_point_at_real_tools_and_resolution_covers_all_forms() {
        let registry = crate::tools::build_default();
        for (alias, native) in ALIASES {
            assert!(
                registry.contains(native),
                "alias {alias} -> missing tool {native}"
            );
            assert_eq!(resolve(&registry, alias).as_deref(), Some(*native));
            assert_eq!(
                resolve(&registry, &safe_name(alias)).as_deref(),
                Some(*native)
            );
        }
        assert_eq!(
            resolve(&registry, "cinvar_render_export").as_deref(),
            Some("cinvar.render.export")
        );
        assert_eq!(
            resolve(&registry, "cinvar.render.export").as_deref(),
            Some("cinvar.render.export")
        );
        assert_eq!(resolve(&registry, "nope"), None);
    }
}

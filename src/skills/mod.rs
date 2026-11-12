use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    pub name: String,
    pub title: String,
    pub summary: String,
    pub tags: Vec<String>,
    pub body: String,
    pub source: PathBuf,
}

pub fn builtin_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("CINVAR_SKILLS_DIR") {
        return PathBuf::from(dir);
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    let local = cwd.join("skills");
    if local.is_dir() {
        return local;
    }
    PathBuf::from("skills")
}

const KNOWN_KEYS: [&str; 6] = ["name", "title", "summary", "tags", "tools", "version"];

mod embedded {
    include!(concat!(env!("OUT_DIR"), "/builtin_skills.rs"));
}

/// The shipped library, compiled into the binary. Always available, whatever the
/// working directory is - an MCP host may start the server anywhere.
fn embedded_skills() -> Vec<Skill> {
    let mut out = Vec::new();
    for (name, text) in embedded::BUILTIN {
        let source = PathBuf::from(format!("builtin/{name}.md"));
        match parse_skill(&source, text) {
            Ok(skill) => out.push(skill),
            Err(err) => warn(&source, &err.message),
        }
    }
    out
}

pub fn load_all() -> Result<Vec<Skill>> {
    let mut out = embedded_skills();
    for skill in merge_dirs(&search_dirs()) {
        out.retain(|existing| existing.name != skill.name);
        out.push(skill);
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

fn search_dirs() -> Vec<PathBuf> {
    if let Ok(dir) = std::env::var("CINVAR_SKILLS_DIR") {
        if !dir.trim().is_empty() {
            return vec![PathBuf::from(dir)];
        }
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    let mut dirs: Vec<PathBuf> = vec![cwd.join("skills")];
    if let Some(exe) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
    {
        dirs.push(exe.join("skills"));
        dirs.push(exe.join("share").join("cinvar").join("skills"));
        if let Some(prefix) = exe.parent() {
            dirs.push(prefix.join("skills"));
        }
    }
    dirs.push(crate::util::paths::config_dir().join("skills"));
    let mut seen: Vec<PathBuf> = Vec::new();
    dirs.retain(|d| {
        let key = crate::util::paths::normalize(d);
        if seen.contains(&key) {
            false
        } else {
            seen.push(key);
            true
        }
    });
    dirs
}

fn merge_dirs(dirs: &[PathBuf]) -> Vec<Skill> {
    let mut merged: BTreeMap<String, Skill> = BTreeMap::new();
    for dir in dirs {
        for skill in load_dir(dir) {
            merged.insert(skill.name.clone(), skill);
        }
    }
    merged.into_values().collect()
}

fn load_dir(dir: &Path) -> Vec<Skill> {
    if !dir.is_dir() {
        return Vec::new();
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => {
            warn(dir, &err.to_string());
            return Vec::new();
        }
    };
    let mut files: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let lower = file_name.to_ascii_lowercase();
        if !lower.ends_with(".md") || lower == "readme.md" || lower.starts_with('.') {
            continue;
        }
        files.push(path);
    }
    files.sort();

    let mut out = Vec::new();
    for path in files {
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) => {
                warn(&path, &err.to_string());
                continue;
            }
        };
        match parse_skill(&path, &text) {
            Ok(skill) => out.push(skill),
            Err(err) => warn(&path, &err.message),
        }
    }
    out
}

fn warn(path: &Path, message: &str) {
    let label = path.display().to_string();
    let detail = message
        .strip_prefix(&format!("{label}: "))
        .unwrap_or(message);
    let line = format!("cinvar: skipping skill {label}: {detail}");
    eprintln!("{}", crate::secrets::redact(&line));
}

fn bad(source: &Path, message: &str) -> Error {
    Error::invalid(format!("{}: {message}", source.display()))
}

fn parse_skill(source: &Path, text: &str) -> Result<Skill> {
    let clean = text
        .strip_prefix('\u{feff}')
        .unwrap_or(text)
        .replace("\r\n", "\n");
    let lines: Vec<&str> = clean.split('\n').collect();

    let mut cursor = 0usize;
    while cursor < lines.len() && lines[cursor].trim().is_empty() {
        cursor += 1;
    }
    if cursor >= lines.len() || lines[cursor].trim() != "---" {
        return Err(bad(source, "missing opening frontmatter fence (---)"));
    }
    let first = cursor + 1;
    let mut closing = None;
    for (offset, line) in lines[first..].iter().enumerate() {
        if line.trim() == "---" {
            closing = Some(first + offset);
            break;
        }
    }
    let closing = closing.ok_or_else(|| bad(source, "frontmatter is never closed with ---"))?;

    let mut name = String::new();
    let mut title = String::new();
    let mut summary = String::new();
    let mut tags: Vec<String> = Vec::new();

    for (offset, raw) in lines[first..closing].iter().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = match line.split_once(':') {
            Some((key, value)) => (key.trim().to_ascii_lowercase(), value.trim()),
            None => {
                return Err(bad(
                    source,
                    &format!(
                        "frontmatter line {} is not 'key: value'",
                        first + offset + 1
                    ),
                ))
            }
        };
        if !KNOWN_KEYS.contains(&key.as_str()) {
            return Err(bad(source, &format!("unsupported frontmatter key '{key}'")));
        }
        let value = unquote(value);
        match key.as_str() {
            "name" => name = value,
            "title" => title = value,
            "summary" => summary = value,
            "tags" => tags = parse_tags(&value),
            _ => {}
        }
    }

    if name.is_empty() {
        name = source
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default()
            .to_string();
    }
    if name.is_empty() {
        return Err(bad(
            source,
            "skill has no name and the file name is not usable",
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(bad(source, &format!("invalid skill name '{name}'")));
    }
    if title.is_empty() {
        title = name.clone();
    }

    let rest = lines[closing + 1..].join("\n");
    let body = rest
        .strip_prefix('\n')
        .unwrap_or(&rest)
        .trim_end()
        .to_string();
    if body.trim().is_empty() {
        return Err(bad(source, "skill has an empty body"));
    }

    Ok(Skill {
        name,
        title,
        summary,
        tags,
        body,
        source: source.to_path_buf(),
    })
}

fn unquote(raw: &str) -> String {
    let trimmed = raw.trim();
    for quote in ['"', '\''] {
        if trimmed.len() >= 2 && trimmed.starts_with(quote) && trimmed.ends_with(quote) {
            return trimmed[1..trimmed.len() - 1].trim().to_string();
        }
    }
    trimmed.to_string()
}

fn parse_tags(raw: &str) -> Vec<String> {
    let trimmed = raw.trim();
    let inner = trimmed
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(trimmed);
    inner
        .split(',')
        .map(|tag| unquote(tag.trim()))
        .filter(|tag| !tag.is_empty())
        .map(|tag| tag.to_ascii_lowercase())
        .collect()
}

pub fn render_prompt(skills: &[Skill], selected: &[String], budget_chars: usize) -> String {
    let limit = if budget_chars == 0 {
        usize::MAX
    } else {
        budget_chars
    };
    if selected.is_empty() {
        let mut out = String::from("Installed skills (load the relevant one with cinvar.skills.load before a style-specific edit):\n");
        if skills.is_empty() {
            out.push_str("- none installed\n");
        }
        for skill in skills {
            out.push_str(&format!("- {}: {}\n", skill.name, skill.summary));
        }
        return clip(&out, limit);
    }

    let mut out = String::new();
    let mut missing: Vec<&str> = Vec::new();
    for wanted in selected {
        let skill = match skills.iter().find(|skill| &skill.name == wanted) {
            Some(skill) => skill,
            None => {
                missing.push(wanted.as_str());
                continue;
            }
        };
        out.push_str(&format!("# {} ({})\n", skill.title, skill.name));
        if !skill.tags.is_empty() {
            out.push_str(&format!("tags: {}\n", skill.tags.join(", ")));
        }
        out.push('\n');
        out.push_str(skill.body.trim_end());
        out.push_str("\n\n");
    }
    while out.ends_with("\n\n") {
        out.pop();
    }
    if !missing.is_empty() {
        out.push_str(&format!("Not installed: {}\n", missing.join(", ")));
    }
    clip(&out, limit)
}

fn clip(text: &str, limit: usize) -> String {
    if limit == usize::MAX || text.chars().count() <= limit {
        return text.to_string();
    }
    let note = format!("\n[truncated at {limit} characters]");
    let note_len = note.chars().count();
    if limit <= note_len {
        return text.chars().take(limit).collect();
    }
    let mut head: String = text.chars().take(limit - note_len).collect();
    if let Some(cut) = head.rfind('\n') {
        head.truncate(cut);
    }
    head.push_str(&note);
    head
}

#[cfg(test)]
mod tests {
    use super::*;

    const REQUIRED_SECTIONS: [&str; 7] = [
        "## Objective",
        "## Creative principles",
        "## Planning sequence",
        "## Timing guidance",
        "## Evaluation checklist",
        "## Failure conditions",
        "## Worked example",
    ];

    const GOOD: &str = "---\nname: cinematic\ntitle: Cinematic Edit\nsummary: Cut long footage into a restrained cinematic sequence.\ntags: [cinematic, narrative]\nversion: 2\n---\n\n## Objective\nMake it feel like a film.\n";

    fn temp_dir(tag: &str) -> tempfile::TempDir {
        tempfile::Builder::new().prefix(tag).tempdir().unwrap()
    }

    fn write_skill(dir: &Path, name: &str, title: &str, summary: &str) -> PathBuf {
        let path = dir.join(format!("{name}.md"));
        let text = format!(
            "---\nname: {name}\ntitle: {title}\nsummary: {summary}\ntags: [test, style]\n---\n\n## Objective\n{title} body.\n"
        );
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn parses_frontmatter_and_body() {
        let skill = parse_skill(Path::new("/skills/cinematic.md"), GOOD).unwrap();
        assert_eq!(skill.name, "cinematic");
        assert_eq!(skill.title, "Cinematic Edit");
        assert_eq!(
            skill.summary,
            "Cut long footage into a restrained cinematic sequence."
        );
        assert_eq!(
            skill.tags,
            vec!["cinematic".to_string(), "narrative".to_string()]
        );
        assert!(skill.body.starts_with("## Objective"));
        assert!(skill.body.ends_with("Make it feel like a film."));
        assert!(!skill.body.contains("---"));
    }

    #[test]
    fn parses_comma_and_bracket_tags() {
        assert_eq!(parse_tags("[a, b]"), vec!["a".to_string(), "b".to_string()]);
        assert_eq!(parse_tags("a, b"), vec!["a".to_string(), "b".to_string()]);
        assert_eq!(
            parse_tags("[ a , \"B\" ]"),
            vec!["a".to_string(), "b".to_string()]
        );
        assert!(parse_tags("[]").is_empty());
        assert!(parse_tags("").is_empty());
    }

    #[test]
    fn name_falls_back_to_file_stem() {
        let text = "---\ntitle: No Name\nsummary: s\ntags: [x]\n---\nbody text\n";
        let skill = parse_skill(Path::new("/skills/short-form.md"), text).unwrap();
        assert_eq!(skill.name, "short-form");
        assert_eq!(skill.title, "No Name");
    }

    #[test]
    fn rejects_malformed_frontmatter_naming_the_file() {
        let cases = [
            "no frontmatter here\n",
            "---\nname: broken\ntitle: x\n",
            "---\nname broken\n---\nbody\n",
            "---\nname: broken\ncolour: red\n---\nbody\n",
            "---\nname: broken\ntitle: x\n---\n\n",
        ];
        for case in cases {
            let err = parse_skill(Path::new("/skills/broken.md"), case).unwrap_err();
            assert!(
                err.message.contains("broken.md"),
                "message was: {}",
                err.message
            );
            assert_eq!(err.code, crate::error::ErrorCode::InvalidInput);
        }
    }

    #[test]
    fn bad_file_does_not_fail_whole_load() {
        let dir = temp_dir("skills-bad");
        write_skill(dir.path(), "good", "Good", "Loads fine.");
        std::fs::write(dir.path().join("bad.md"), "not a skill\n").unwrap();
        std::fs::write(dir.path().join("README.md"), "# docs\n").unwrap();
        let skills = load_dir(dir.path());
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "good");
    }

    #[test]
    fn later_directory_overrides_by_name() {
        let builtin = temp_dir("skills-builtin");
        let user = temp_dir("skills-user");
        write_skill(
            builtin.path(),
            "cinematic",
            "Builtin Cinematic",
            "builtin summary",
        );
        write_skill(builtin.path(), "podcast", "Podcast", "podcast summary");
        write_skill(user.path(), "cinematic", "User Cinematic", "user summary");

        let skills = merge_dirs(&[builtin.path().to_path_buf(), user.path().to_path_buf()]);
        assert_eq!(skills.len(), 2);
        let cinematic = skills.iter().find(|s| s.name == "cinematic").unwrap();
        assert_eq!(cinematic.title, "User Cinematic");
        assert_eq!(cinematic.source, user.path().join("cinematic.md"));
        assert!(skills.iter().any(|s| s.name == "podcast"));
    }

    fn fixture() -> Vec<Skill> {
        let alpha = "---\nname: alpha\ntitle: Alpha\nsummary: ALPHA_SUMMARY\ntags: [a]\n---\n\n## Objective\nALPHA_BODY line one\nALPHA_BODY line two\nALPHA_BODY line three\n";
        let beta = "---\nname: beta\ntitle: Beta\nsummary: BETA_SUMMARY\ntags: [b]\n---\n\n## Objective\nBETA_BODY\n";
        vec![
            parse_skill(Path::new("/skills/alpha.md"), alpha).unwrap(),
            parse_skill(Path::new("/skills/beta.md"), beta).unwrap(),
        ]
    }

    #[test]
    fn empty_selection_renders_index_only() {
        let prompt = render_prompt(&fixture(), &[], 10_000);
        assert!(prompt.contains("alpha: ALPHA_SUMMARY"));
        assert!(prompt.contains("beta: BETA_SUMMARY"));
        assert!(!prompt.contains("ALPHA_BODY"));
        assert!(!prompt.contains("BETA_BODY"));
    }

    #[test]
    fn selection_renders_only_selected_bodies() {
        let prompt = render_prompt(&fixture(), &["alpha".to_string()], 10_000);
        assert!(prompt.contains("ALPHA_BODY line one"));
        assert!(prompt.contains("## Objective"));
        assert!(!prompt.contains("BETA_BODY"));
        assert!(!prompt.contains("BETA_SUMMARY"));
        assert!(!prompt.contains("beta"));

        let both = render_prompt(
            &fixture(),
            &["alpha".to_string(), "beta".to_string()],
            10_000,
        );
        assert!(both.contains("ALPHA_BODY") && both.contains("BETA_BODY"));
    }

    #[test]
    fn unknown_selection_is_reported_not_invented() {
        let prompt = render_prompt(
            &fixture(),
            &["alpha".to_string(), "ghost".to_string()],
            10_000,
        );
        assert!(prompt.contains("ALPHA_BODY"));
        assert!(prompt.contains("Not installed: ghost"));
        assert!(!prompt.contains("BETA_BODY"));
    }

    #[test]
    fn budget_truncates_on_a_line_boundary() {
        let prompt = render_prompt(&fixture(), &["alpha".to_string()], 70);
        assert!(
            prompt.chars().count() <= 70,
            "length was {}",
            prompt.chars().count()
        );
        assert!(prompt.contains("[truncated at 70 characters]"));
        assert!(prompt.ends_with("[truncated at 70 characters]"));
        assert!(!prompt.contains("ALPHA_BODY line three"));
        let kept: Vec<&str> = prompt
            .lines()
            .filter(|l| l.starts_with("ALPHA_BODY"))
            .collect();
        assert!(kept.len() < 3);
        assert!(!prompt.contains("\n\n\n"));

        let unlimited = render_prompt(&fixture(), &["alpha".to_string()], 0);
        assert!(!unlimited.contains("[truncated"));
        assert!(unlimited.contains("ALPHA_BODY line three"));
    }

    #[test]
    fn shipped_skills_parse_with_required_sections() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("skills");
        let skills = load_dir(&dir);
        let expected = [
            "cinematic",
            "youtube",
            "short-form",
            "podcast",
            "gaming",
            "documentary",
            "commercial",
            "motion-graphics",
            "blender-animation",
            "video-restoration",
            "decision-framework",
        ];
        assert_eq!(
            skills.len(),
            expected.len(),
            "found {:?}",
            skills.iter().map(|s| &s.name).collect::<Vec<_>>()
        );
        for name in expected {
            let skill = skills
                .iter()
                .find(|skill| skill.name == name)
                .unwrap_or_else(|| panic!("missing shipped skill '{name}'"));
            assert!(!skill.summary.is_empty(), "{name} has no summary");
            assert!(
                skill.summary.chars().count() <= 140,
                "{name} summary is {} chars",
                skill.summary.chars().count()
            );
            assert!(!skill.tags.is_empty(), "{name} has no tags");
            assert!(
                skill.body.chars().count() > 1500,
                "{name} body is {} chars",
                skill.body.chars().count()
            );
            for heading in REQUIRED_SECTIONS {
                assert!(
                    skill.body.contains(heading),
                    "{name} is missing '{heading}'"
                );
            }
            assert_eq!(
                skill.source.file_stem().and_then(|s| s.to_str()),
                Some(name),
                "file name must equal the skill name"
            );
            assert_ne!(skill.title, name, "{name} has no real title");
        }
    }

    #[test]
    fn shipped_skills_only_reference_real_tools() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("skills");
        let skills = load_dir(&dir);
        let registry = crate::tools::build_default();
        let known: Vec<String> = registry.specs().iter().map(|t| t.name.clone()).collect();
        for skill in &skills {
            for token in skill
                .body
                .split(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-'))
            {
                let token = token.trim_end_matches('.');
                if token.starts_with("cinvar.") {
                    assert!(
                        known.iter().any(|k| k == token),
                        "{} references unknown tool {token}",
                        skill.name
                    );
                }
            }
        }
    }
}

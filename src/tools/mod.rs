pub mod groups;
pub mod groups_blender;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::Config;
use crate::error::{Error, Result};
use crate::jobs::Jobs;
use crate::project::history::History;
use crate::project::model::Project;
use crate::project::store::ProjectStore;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub group: String,
    pub description: String,
    pub input: Value,
    #[serde(default)]
    pub destructive: bool,
}

pub struct ToolCtx<'a> {
    pub project: &'a mut Project,
    pub store: &'a ProjectStore,
    pub config: &'a Config,
    pub jobs: &'a Jobs,
    pub history: &'a mut History,
    pub dry_run: bool,
}

impl<'a> ToolCtx<'a> {
    pub fn persist(&mut self) -> Result<()> {
        if self.dry_run {
            return Ok(());
        }
        self.store.save(self.project)
    }

    pub fn record_history(&mut self) {
        if !self.dry_run {
            self.history.record(self.project);
        }
    }
}

/// Gates that depend on the arguments rather than the tool name alone.
///
/// A tool can be free in principle and Pro past a limit (semantic indexing), so the tool table is
/// not enough. Everything here runs before the tool body, so the caller never gets a partial
/// result it would have to undo.
fn gate_arguments(config: &Config, name: &str, args: &Value) -> Result<()> {
    if name == "cinvar.scenes.index" {
        let entitlements = crate::edition::resolve(config);
        if let Some(root) = args.get("media_root").and_then(|v| v.as_str()) {
            entitlements.check_semantic_clips(count_indexable_clips(std::path::Path::new(root)))?;
        }
    }
    Ok(())
}

/// How many videos an index run over `dir` would cover (the same discovery the indexer uses).
/// A path that does not resolve counts as zero: the tool itself reports the real problem.
pub fn count_indexable_clips(dir: &std::path::Path) -> usize {
    let abs = crate::util::paths::absolutize(
        dir,
        &std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
    );
    crate::analysis::autoedit::collect_video_files(&abs)
        .map(|files| files.len())
        .unwrap_or(0)
}

pub type ToolFn = Box<dyn Fn(&mut ToolCtx, Value) -> Result<Value> + Send + Sync>;

pub struct Tool {
    pub spec: ToolSpec,
    pub run: ToolFn,
}

#[derive(Default, Clone)]
pub struct Registry {
    tools: BTreeMap<String, std::sync::Arc<Tool>>,
}

impl Registry {
    pub fn new() -> Registry {
        Registry {
            tools: BTreeMap::new(),
        }
    }

    pub fn add<F>(&mut self, spec: ToolSpec, run: F)
    where
        F: Fn(&mut ToolCtx, Value) -> Result<Value> + Send + Sync + 'static,
    {
        self.tools.insert(
            spec.name.clone(),
            std::sync::Arc::new(Tool {
                spec,
                run: Box::new(run),
            }),
        );
    }

    pub fn contains(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.values().map(|t| t.spec.clone()).collect()
    }

    pub fn get(&self, name: &str) -> Option<&ToolSpec> {
        self.tools.get(name).map(|t| &t.spec)
    }

    pub fn groups(&self) -> BTreeMap<String, Vec<ToolSpec>> {
        let mut out: BTreeMap<String, Vec<ToolSpec>> = BTreeMap::new();
        for tool in self.tools.values() {
            out.entry(tool.spec.group.clone())
                .or_default()
                .push(tool.spec.clone());
        }
        out
    }

    pub fn call(&self, name: &str, ctx: &mut ToolCtx, args: Value) -> Result<Value> {
        let tool = self.tools.get(name).ok_or_else(|| {
            Error::not_found(format!("unknown tool '{name}'"))
                .with_hint("call tools/list to discover available tools")
        })?;
        if let Some(feature) = crate::edition::tool_feature(name) {
            crate::edition::resolve(ctx.config).check_feature(feature)?;
        }
        gate_arguments(ctx.config, name, &args)?;
        let args = if args.is_null() {
            Value::Object(Default::default())
        } else {
            args
        };
        let revision_before = ctx.project.revision;
        let recorded_args = ctx.config.actions.record.then(|| args.clone());
        let result = (tool.run)(ctx, args);
        if let Some(args) = recorded_args {
            crate::actions::record(ctx, name, &args, revision_before, &result);
        }
        result
    }

    pub fn to_schemas(&self) -> Vec<crate::providers::ToolSchema> {
        self.tools
            .values()
            .map(|t| crate::providers::ToolSchema {
                name: t.spec.name.clone(),
                description: t.spec.description.clone(),
                input_schema: t.spec.input.clone(),
            })
            .collect()
    }

    pub fn prompt_index(&self) -> String {
        let mut out = String::new();
        for (group, specs) in self.groups() {
            out.push_str(&format!("## {group}\n"));
            for spec in specs {
                let short: String = spec
                    .description
                    .split('.')
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(110)
                    .collect();
                out.push_str(&format!("- {}: {}\n", spec.name, short));
            }
            out.push('\n');
        }
        out
    }
}

static EXTENSIONS: std::sync::RwLock<Vec<fn(&mut Registry)>> = std::sync::RwLock::new(Vec::new());

/// Adds a function that registers extra tools into every registry built from now on (used by the
/// separately licensed Pro module). Idempotent per function pointer.
pub fn register_extension(f: fn(&mut Registry)) {
    if let Ok(mut list) = EXTENSIONS.write() {
        if !list.iter().any(|g| *g as usize == f as usize) {
            list.push(f);
        }
    }
}

pub fn build_default() -> Registry {
    let mut registry = Registry::new();
    groups::register_project(&mut registry);
    groups_blender::register_blender(&mut registry);
    groups::register_media(&mut registry);
    groups::register_timeline(&mut registry);
    groups::register_audio(&mut registry);
    groups::register_captions(&mut registry);
    groups::register_render(&mut registry);
    groups::register_creative(&mut registry);
    groups::register_color(&mut registry);
    groups::register_system(&mut registry);
    if let Ok(list) = EXTENSIONS.read() {
        for f in list.iter() {
            f(&mut registry);
        }
    }
    registry
}

/// Default tools plus every enabled, compatible external plugin (see `plugins::registry`).
/// Plugins that are disabled, incompatible, missing dependencies, or that need subprocess
/// permission the config withholds are skipped with a diagnostic, never half-registered.
pub fn build_with_plugins(config: &Config) -> Registry {
    let mut registry = build_default();
    let dir = crate::util::paths::config_dir().join("plugins");
    let state = crate::plugins::registry::PluginState::load(
        &crate::plugins::registry::default_state_path(),
    );
    let (runnable, skipped) = crate::plugins::registry::runnable(&dir, &state);
    for (id, why) in skipped {
        eprintln!("cinvar: plugin '{id}' not loaded: {why}");
    }
    for manifest in runnable {
        let one = [manifest];
        match crate::plugins::register(&mut registry, &one, config.security.allow_plugin_subprocess)
        {
            Ok(_) => {}
            Err(e) => eprintln!("cinvar: plugin '{}' not loaded: {}", one[0].name, e.message),
        }
    }
    registry
}

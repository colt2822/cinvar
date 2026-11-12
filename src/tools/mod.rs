pub mod groups;

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
        let args = if args.is_null() {
            Value::Object(Default::default())
        } else {
            args
        };
        (tool.run)(ctx, args)
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

pub fn build_default() -> Registry {
    let mut registry = Registry::new();
    groups::register_project(&mut registry);
    groups::register_media(&mut registry);
    groups::register_timeline(&mut registry);
    groups::register_audio(&mut registry);
    groups::register_captions(&mut registry);
    groups::register_render(&mut registry);
    groups::register_creative(&mut registry);
    groups::register_color(&mut registry);
    groups::register_system(&mut registry);
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

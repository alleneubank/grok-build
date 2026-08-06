//! Shared hook source path discovery.

use std::path::{Path, PathBuf};

use xai_grok_config::resolve_global_hook_sources;
use xai_grok_hooks::discovery::HookSource;
use xai_grok_hooks::error::HookError;

/// Owned paths for hook sources. Callers borrow via `as_sources()`.
pub(crate) struct HookSourcePaths {
    pub global: Vec<PathBuf>,
    pub project: Vec<PathBuf>,
}

impl HookSourcePaths {
    /// Borrow as `HookSource` refs. Project sources are excluded when untrusted.
    pub(crate) fn as_sources(
        &self,
        include_project: bool,
    ) -> (Vec<HookSource<'_>>, Vec<HookSource<'_>>) {
        let global = self.global.iter().map(|p| path_to_source(p)).collect();
        let project = if include_project {
            self.project.iter().map(|p| path_to_source(p)).collect()
        } else {
            vec![]
        };
        (global, project)
    }
}

fn path_to_source(p: &Path) -> HookSource<'_> {
    if p.is_dir() {
        HookSource::Directory(p)
    } else {
        HookSource::SettingsFile(p)
    }
}

fn include_claude_hooks(compat: &xai_grok_tools::types::compat::CompatConfig) -> bool {
    compat.claude.hooks
        && !crate::claude_import::is_claude_import_marked_with_log("discover_hook_source_paths")
}

fn include_cursor_hooks(compat: &xai_grok_tools::types::compat::CompatConfig) -> bool {
    compat.cursor.hooks
}

/// Global + project hook source paths. Registry file is never a discovery
/// source; compatible vendor globals are appended when their gates are on.
pub(crate) fn discover_hook_source_paths(
    git_root: Option<&Path>,
    compat: &xai_grok_tools::types::compat::CompatConfig,
) -> HookSourcePaths {
    let grok = xai_grok_config::user_grok_home();
    let home = dirs::home_dir();
    let include_claude = include_claude_hooks(compat);
    let include_cursor = include_cursor_hooks(compat);

    // Soft hooks-paths I/O keeps fixed slots; hard resolve omits Grok globals.
    let mut global: Vec<PathBuf> =
        match resolve_global_hook_sources(grok.as_deref(), /* reject_symlinks */ false) {
            Ok(resolved) => {
                if let Some(e) = &resolved.configured_error {
                    tracing::warn!(
                        error = %e,
                        "hooks-paths unreadable; retaining fixed Grok hook discovery sources only"
                    );
                }
                resolved
                    .discovery_sources()
                    .map(|s| s.path.clone())
                    .collect()
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "global hook source resolve hard-failed; omitting Grok global sources"
                );
                Vec::new()
            }
        };

    if let Some(h) = home.as_deref() {
        if include_claude {
            global.push(h.join(".claude").join("settings.json"));
            global.push(h.join(".claude").join("settings.local.json"));
        }
        if include_cursor {
            global.push(h.join(".cursor").join("hooks.json"));
        }
    }

    let mut project = Vec::new();
    if let Some(root) = git_root {
        if include_claude {
            project.push(root.join(".claude").join("settings.json"));
            project.push(root.join(".claude").join("settings.local.json"));
        }
        project.push(root.join(".grok").join("hooks"));
        if include_cursor {
            project.push(root.join(".cursor").join("hooks.json"));
        }
    }

    HookSourcePaths { global, project }
}

/// Single load entry point: build compat-aware sources, gate project sources on
/// trust, then load. Every session-startup and mid-session reload site routes
/// through here so the source policy stays in one place.
///
/// Does **not** include plugin-contributed hooks; call
/// [`append_active_plugin_hooks`] after this so cold start matches mid-session
/// `/hooks` reload (plugin hooks must not only appear after an explicit reload).
pub(crate) fn discover_hooks(
    git_root: Option<&Path>,
    compat: &xai_grok_tools::types::compat::CompatConfig,
    trusted: bool,
) -> (xai_grok_hooks::discovery::HookRegistry, Vec<HookError>) {
    // Read fresh each call (not cached): a mid-session `/hooks` reload must see an
    // updated `config.toml` / `managed_config.toml`. This is lighter than
    // `ConfigLayers::load` (only the small per-layer files, no campaigns, version
    // overrides, or MDM).
    let config_layers = xai_grok_config::hook_config_layers();
    assemble_hooks(&config_layers, git_root, compat, trusted)
}

/// Collect hook specs from all active plugins (file `hooks/hooks.json` + inline
/// manifest hooks), with `CLAUDE_PLUGIN_ROOT` injection via the plugin adapter.
/// Shared by session spawn and mid-session reload so both paths stay identical.
pub(crate) fn collect_active_plugin_hook_specs(
    plugin_registry: &xai_grok_agent::plugins::PluginRegistry,
) -> Vec<xai_grok_hooks::config::HookSpec> {
    let mut specs = Vec::new();
    for plugin in plugin_registry.active_plugins() {
        if let Some(ref hooks_path) = plugin.hooks_path {
            let (parsed, warnings) = xai_grok_agent::plugins::hooks_adapter::parse_plugin_hooks(
                hooks_path,
                &plugin.name,
                &plugin.root_str(),
                &plugin.data_dir_str(),
            );
            for w in &warnings {
                tracing::warn!("{w}");
            }
            specs.extend(parsed);
        }
        if let Some(ref inline_value) = plugin.inline_hooks {
            let (parsed, warnings) =
                xai_grok_agent::plugins::hooks_adapter::parse_plugin_hooks_from_value(
                    inline_value,
                    &plugin.name,
                    &plugin.root_str(),
                    &plugin.data_dir_str(),
                );
            for w in &warnings {
                tracing::warn!("{w}");
            }
            specs.extend(parsed);
        }
    }
    specs
}

/// Append hooks from active plugins onto a registry. Call after
/// [`discover_hooks`] at cold start and on `/hooks` reload.
pub(crate) fn append_active_plugin_hooks(
    registry: &mut xai_grok_hooks::discovery::HookRegistry,
    plugin_registry: &xai_grok_agent::plugins::PluginRegistry,
) {
    let specs = collect_active_plugin_hook_specs(plugin_registry);
    if !specs.is_empty() {
        tracing::info!(
            plugin_hook_count = specs.len(),
            "appending plugin-contributed hooks"
        );
        registry.append_specs(specs);
    }
}

/// Pure, injectable core: combine config-layer hooks with file-source hooks and
/// dedup once. Config-layer specs are placed first so that, under the first-wins
/// dedup in [`xai_grok_hooks::discovery::registry_from_specs_deduped`], a config
/// hook wins over a byte-identical file hook. `config_layers` is a parameter (not
/// read here) so tests can drive it with hand-built layers.
pub(crate) fn assemble_hooks(
    config_layers: &[xai_grok_config::HookConfigLayer],
    git_root: Option<&Path>,
    compat: &xai_grok_tools::types::compat::CompatConfig,
    trusted: bool,
) -> (xai_grok_hooks::discovery::HookRegistry, Vec<HookError>) {
    let (mut specs, mut errors) =
        xai_grok_hooks::config::parse_hooks_from_config_layers(config_layers);

    let source_paths = discover_hook_source_paths(git_root, compat);
    let (global_sources, project_sources) = source_paths.as_sources(trusted);
    let (file_specs, file_errors) =
        xai_grok_hooks::discovery::collect_specs_from_sources(&global_sources, &project_sources);
    specs.extend(file_specs);
    errors.extend(file_errors);

    (
        xai_grok_hooks::discovery::registry_from_specs_deduped(specs),
        errors,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use xai_grok_agent::plugins::discovery::{
        DiscoveredPlugin, PluginId, PluginOrigin, PluginScope,
    };
    use xai_grok_agent::plugins::manifest::PluginManifest;
    use xai_grok_agent::plugins::PluginRegistry;
    use xai_grok_hooks::event::HookEventName;

    fn plugin_with_hooks(tmp: &std::path::Path, name: &str) -> DiscoveredPlugin {
        let root = tmp.join(name);
        let hooks_dir = root.join("hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        std::fs::write(
            hooks_dir.join("hooks.json"),
            r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"echo stop"}]}]}}"#,
        )
        .unwrap();
        DiscoveredPlugin {
            manifest: PluginManifest {
                name: name.to_string(),
                version: Some("1.0.0".to_string()),
                description: None,
                author: None,
                homepage: None,
                repository: None,
                license: None,
                keywords: vec![],
                skills: None,
                commands: None,
                agents: None,
                hooks: None,
                mcp_servers: None,
                lsp_servers: None,
            },
            id: PluginId::new(PluginScope::User, &root, name),
            root: root.clone(),
            canonical_root: root.clone(),
            scope: PluginScope::User,
            origin: PluginOrigin::UserGrok,
            trusted: true,
            skill_dirs: vec![],
            command_dirs: vec![],
            agent_dirs: vec![],
            hooks_path: Some(hooks_dir.join("hooks.json")),
            mcp_config_path: None,
            lsp_config_path: None,
            conflict: None,
        }
    }

    #[test]
    fn append_active_plugin_hooks_loads_plugin_hooks_at_cold_start_path() {
        // Regression: cold start must append the same plugin hooks as mid-session
        // reload, otherwise Claude-compat plugins only fire after /hooks → r.
        let tmp = tempfile::tempdir().unwrap();
        let dp = plugin_with_hooks(tmp.path(), "sox-test");
        let name = dp.manifest.name.clone();
        let pr = PluginRegistry::from_discovered(vec![dp], &[], &[name]);
        assert_eq!(pr.active_plugins().len(), 1, "plugin must be active");

        let mut registry = xai_grok_hooks::discovery::HookRegistry::default();
        assert!(registry.is_empty());
        append_active_plugin_hooks(&mut registry, &pr);

        let stop = registry.hooks_for(HookEventName::Stop);
        assert_eq!(
            stop.len(),
            1,
            "plugin Stop hook must be present without a mid-session reload"
        );
        assert!(
            stop[0].name.starts_with("plugin/"),
            "plugin hook names are namespaced: {}",
            stop[0].name
        );
    }

    #[test]
    fn append_active_plugin_hooks_skips_disabled_or_untrusted() {
        let tmp = tempfile::tempdir().unwrap();
        let mut dp = plugin_with_hooks(tmp.path(), "disabled-sox");
        dp.trusted = false;
        let name = dp.manifest.name.clone();
        // Enabled in config but untrusted → not active_plugins().
        let pr = PluginRegistry::from_discovered(vec![dp], &[], &[name]);
        assert!(pr.active_plugins().is_empty());

        let mut registry = xai_grok_hooks::discovery::HookRegistry::default();
        append_active_plugin_hooks(&mut registry, &pr);
        assert!(registry.is_empty());
    }
}

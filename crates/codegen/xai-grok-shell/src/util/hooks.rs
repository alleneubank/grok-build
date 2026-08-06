//! Host bindings for [`xai_grok_hooks::discovery`]. Discovery itself lives in the hooks crate.
//! Bound here are the two inputs that crate cannot read: the Claude import cutoff and the managed-settings hooks pin.

use std::path::Path;

use xai_grok_hooks::error::HookError;

/// The `[claude_compat] imported = true` cutoff, read once per process in [`crate::claude_import`].
/// Every entry point below passes it down so the hooks crate stays free of shell state.
fn claude_import_marked() -> bool {
    crate::claude_import::is_claude_import_marked_with_log("discover_hook_source_paths")
}

/// The disabled-hooks file plus the resolved `allow_managed_hooks_only` pin.
pub(crate) fn disabled_hooks_snapshot() -> xai_grok_hooks::trust::DisabledHooks {
    let managed_only = xai_grok_workspace::permission::resolution::managed_settings()
        .non_managed_hooks
        .is_disabled();
    xai_grok_hooks::trust::DisabledHooks::load(managed_only)
}

/// Single load entry point: [`xai_grok_hooks::discovery::discover_hooks`] with the Claude cutoff applied.
/// Every session-startup and mid-session reload site routes through here so the source policy stays in one place.
///
/// Does **not** include plugin-contributed hooks; call
/// [`append_active_plugin_hooks`] after this so cold start matches mid-session
/// `/hooks` reload (plugin hooks must not only appear after an explicit reload).
pub(crate) fn discover_hooks(
    git_root: Option<&Path>,
    compat: &xai_grok_tools::types::compat::CompatConfig,
    trusted: bool,
) -> (xai_grok_hooks::discovery::HookRegistry, Vec<HookError>) {
    xai_grok_hooks::discovery::discover_hooks(git_root, compat, claude_import_marked(), trusted)
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

/// [`xai_grok_hooks::discovery::assemble_hooks`] with the Claude cutoff applied, for callers that
/// supply their own config layers.
pub(crate) fn assemble_hooks(
    config_layers: &[xai_grok_config::HookConfigLayer],
    git_root: Option<&Path>,
    compat: &xai_grok_tools::types::compat::CompatConfig,
    trusted: bool,
) -> (xai_grok_hooks::discovery::HookRegistry, Vec<HookError>) {
    xai_grok_hooks::discovery::assemble_hooks(
        config_layers,
        git_root,
        compat,
        claude_import_marked(),
        trusted,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use xai_grok_agent::plugins::PluginRegistry;
    use xai_grok_agent::plugins::discovery::{
        DiscoveredPlugin, PluginId, PluginOrigin, PluginScope,
    };
    use xai_grok_agent::plugins::manifest::PluginManifest;
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

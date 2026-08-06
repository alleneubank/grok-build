//! Lifecycle hook emission from the permission manager.
//!
//! Fires Claude-compatible `PermissionRequest` when an interactive permission
//! chooser is about to be shown — not on auto-approve paths. Keeps the fire
//! site next to `prompter.request` so TUI, ACP, and hub permission share one
//! emission.

use std::sync::Arc;

use xai_grok_hooks::discovery::HookRegistry;
use xai_grok_hooks::dispatcher::dispatch_non_blocking;
use xai_grok_hooks::event::{HookEventEnvelope, HookEventName, HookPayload, truncate_payload};
use xai_grok_hooks::runner::RunContext;

/// Session + registry snapshot used when the manager is about to prompt.
/// Updated via [`super::PermissionHandle::set_permission_request_hooks`] so
/// reloads can refresh the registry without restarting the actor.
#[derive(Clone)]
pub struct PermissionRequestHookContext {
    pub registry: Arc<HookRegistry>,
    pub session_id: String,
    pub cwd: String,
    pub workspace_root: String,
}

/// Emit observe-only `PermissionRequest` hooks when any are registered.
///
/// No-op when the registry has no enabled hooks for the event (cheap hot-path
/// guard). Fail-open: hook failures never block the permission prompt.
pub async fn fire_permission_request(
    ctx: &PermissionRequestHookContext,
    tool_name: &str,
    tool_use_id: &str,
    tool_input: serde_json::Value,
    permission_mode: &str,
) {
    if !ctx
        .registry
        .has_enabled_hooks_for_canonical(HookEventName::PermissionRequest)
    {
        return;
    }

    let (tool_input, tool_input_truncated) = truncate_payload(tool_input);
    let envelope = HookEventEnvelope {
        hook_event_name: HookEventName::PermissionRequest,
        session_id: ctx.session_id.clone(),
        cwd: ctx.cwd.clone(),
        workspace_root: ctx.workspace_root.clone(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        transcript_path: None,
        client_identifier: None,
        prompt_id: None,
        permission_mode: Some(permission_mode.to_owned()),
        payload: HookPayload::PermissionRequest {
            tool_name: tool_name.to_owned(),
            tool_use_id: tool_use_id.to_owned(),
            tool_input,
            tool_input_truncated,
        },
    };
    let run_ctx = RunContext {
        session_id: &ctx.session_id,
        workspace_root: &ctx.workspace_root,
        process_scope: None,
    };
    let _results = dispatch_non_blocking(
        &ctx.registry,
        HookEventName::PermissionRequest,
        &envelope,
        &run_ctx,
    )
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use xai_grok_hooks::config::HookSpec;
    use xai_grok_hooks::event::HookEventName;

    fn registry_with_permission_request(command: &str) -> Arc<HookRegistry> {
        let mut registry = HookRegistry::default();
        registry.append_specs(vec![HookSpec {
            name: "test/perm-request".into(),
            event: HookEventName::PermissionRequest,
            handler_type: xai_grok_hooks::config::HandlerType::Command,
            configured_matcher: None,
            matcher: None,
            enabled: true,
            command: Some(std::path::PathBuf::from(command)),
            command_raw: Some(command.into()),
            url: None,
            url_raw: None,
            timeout_ms: 5_000,
            source_dir: std::path::PathBuf::from("/tmp"),
            extra_env: Default::default(),
            layer: xai_grok_hooks::config::HookProvenance::File,
        }]);
        Arc::new(registry)
    }

    #[tokio::test]
    async fn fire_permission_request_runs_registered_hook() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("seen.txt");
        // Inline `sh -c` command (same pattern as hooks integration tests):
        // capture GROK_HOOK_EVENT without a chmod +x script (noexec tmpdirs).
        let out_str = out.to_string_lossy();
        let command = format!("printf '%s' \"$GROK_HOOK_EVENT\" > {out_str}");

        let registry = registry_with_permission_request(&command);
        let ctx = PermissionRequestHookContext {
            registry,
            session_id: "sess-1".into(),
            cwd: "/tmp".into(),
            workspace_root: "/tmp".into(),
        };
        fire_permission_request(
            &ctx,
            "run_terminal_command",
            "call-1",
            serde_json::json!({"command": "rm -rf /"}),
            "default",
        )
        .await;

        let seen = std::fs::read_to_string(&out).expect("hook should have written output");
        assert_eq!(
            seen.trim(),
            "permission_request",
            "GROK_HOOK_EVENT must be permission_request, got {seen:?}"
        );
    }

    #[tokio::test]
    async fn fire_permission_request_noop_without_hooks() {
        let ctx = PermissionRequestHookContext {
            registry: Arc::new(HookRegistry::default()),
            session_id: "sess-1".into(),
            cwd: "/tmp".into(),
            workspace_root: "/tmp".into(),
        };
        // Must not panic or hang when nothing is registered.
        fire_permission_request(
            &ctx,
            "run_terminal_command",
            "call-1",
            serde_json::json!({}),
            "default",
        )
        .await;
    }
}

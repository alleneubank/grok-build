//! Async command-backed custom status line for [`super::AgentView`].
//!
//! Mirrors Codex/Claude Code: on status changes, spawn the configured shell
//! command with a Claude-compatible JSON snapshot on stdin; apply the first
//! ANSI line under the prompt. Failed / empty / timed-out output hides the row.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{Value, json};
use wait_timeout::ChildExt;
use xai_grok_shell::agent::config::{CustomStatusLineConfig, CustomStatusLineType, UiConfig};
use xai_grok_version::VERSION;

use super::AgentView;
use crate::views::custom_status_line::{self, MAX_PADDING};

const TIMEOUT: Duration = Duration::from_secs(5);
const MAX_STDOUT_BYTES: usize = 4096;
const DEFAULT_COLUMNS: u16 = 80;

/// Result of one background statusline render attempt.
pub(crate) struct CustomStatusLineResult {
    pub request_id: u64,
    pub output: Option<String>,
}

#[derive(Default)]
pub(crate) struct CustomStatusLineState {
    next_request_id: u64,
    pending_request_id: Option<u64>,
    /// Receiver for in-flight renders; polled each frame.
    result_rx: Option<mpsc::Receiver<CustomStatusLineResult>>,
    last_request_key: Option<String>,
    /// Cached resolved config (Grok ui or Claude settings fallback).
    resolved_config: Option<Option<CustomStatusLineConfig>>,
    /// First renderable ANSI line from the command (themed at paint time).
    rendered_ansi: Option<String>,
    padding: u16,
}

impl CustomStatusLineState {
    pub fn height(&self) -> u16 {
        custom_status_line::height(self.rendered_ansi.is_some(), self.padding)
    }

    pub fn rendered_ansi(&self) -> Option<&str> {
        self.rendered_ansi.as_deref()
    }

    pub fn padding(&self) -> u16 {
        self.padding
    }

    fn invalidate_key(&mut self) {
        self.last_request_key = None;
    }
}

impl AgentView {
    /// Poll completed renders and kick a refresh when the snapshot changes.
    pub(crate) fn tick_custom_status_line(&mut self, ui: &UiConfig, columns: u16) {
        self.poll_custom_status_line_results();
        self.refresh_custom_status_line(ui, columns);
    }

    fn poll_custom_status_line_results(&mut self) {
        let Some(rx) = self.custom_status_line_state.result_rx.as_ref() else {
            return;
        };
        let mut latest: Option<CustomStatusLineResult> = None;
        while let Ok(result) = rx.try_recv() {
            latest = Some(result);
        }
        let Some(result) = latest else {
            return;
        };
        if self.custom_status_line_state.pending_request_id != Some(result.request_id) {
            return;
        }
        self.custom_status_line_state.pending_request_id = None;
        if result.request_id != self.custom_status_line_state.next_request_id {
            return;
        }
        match result
            .output
            .as_deref()
            .and_then(custom_status_line::raw_line_from_command_output)
        {
            Some(ansi) => {
                self.custom_status_line_state.rendered_ansi = Some(ansi);
            }
            None => {
                self.custom_status_line_state.rendered_ansi = None;
                self.custom_status_line_state.invalidate_key();
            }
        }
    }

    fn refresh_custom_status_line(&mut self, ui: &UiConfig, columns: u16) {
        let config = self.resolve_custom_status_line_config(ui);
        let Some(config) = config else {
            if self.custom_status_line_state.rendered_ansi.is_some()
                || self.custom_status_line_state.pending_request_id.is_some()
            {
                self.custom_status_line_state.rendered_ansi = None;
                self.custom_status_line_state.pending_request_id = None;
                self.custom_status_line_state.invalidate_key();
                self.custom_status_line_state.padding = 0;
            }
            return;
        };
        if config.command.trim().is_empty() {
            self.custom_status_line_state.rendered_ansi = None;
            self.custom_status_line_state.invalidate_key();
            return;
        }

        self.custom_status_line_state.padding = config.padding.min(MAX_PADDING);
        let cwd = self.session.cwd.clone();
        let columns = if columns == 0 { DEFAULT_COLUMNS } else { columns };
        let payload = self.custom_status_line_payload(&cwd);
        let request_key = format!(
            "{}|{}|{}|{}",
            config.command,
            cwd.display(),
            columns,
            payload
        );
        if self.custom_status_line_state.last_request_key.as_ref() == Some(&request_key) {
            return;
        }
        // Coalesce while a request is in flight: keep the key unset so we
        // re-issue after the pending one completes with the latest payload.
        if self.custom_status_line_state.pending_request_id.is_some() {
            return;
        }
        self.custom_status_line_state.last_request_key = Some(request_key);
        self.custom_status_line_state.next_request_id = self
            .custom_status_line_state
            .next_request_id
            .saturating_add(1);
        let request_id = self.custom_status_line_state.next_request_id;
        self.custom_status_line_state.pending_request_id = Some(request_id);

        let (tx, rx) = mpsc::channel();
        self.custom_status_line_state.result_rx = Some(rx);

        let command = config.command.clone();
        let env = config.env.clone();
        std::thread::Builder::new()
            .name("custom-statusline".into())
            .spawn(move || {
                let output =
                    run_custom_status_line_command(&command, &env, &cwd, &payload, columns);
                let _ = tx.send(CustomStatusLineResult { request_id, output });
            })
            .ok();
    }

    fn resolve_custom_status_line_config(&mut self, ui: &UiConfig) -> Option<CustomStatusLineConfig> {
        if let Some(cached) = self.custom_status_line_state.resolved_config.as_ref() {
            // Prefer live UiConfig when set so hot-reload / tests win.
            if let Some(cfg) = ui.custom_status_line.as_ref() {
                if !cfg.command.trim().is_empty() {
                    return Some(cfg.clone());
                }
            }
            return cached.clone();
        }
        let resolved = if let Some(cfg) = ui.custom_status_line.as_ref() {
            if cfg.command.trim().is_empty() {
                load_claude_status_line_config()
            } else {
                Some(cfg.clone())
            }
        } else {
            load_claude_status_line_config()
        };
        self.custom_status_line_state.resolved_config = Some(resolved.clone());
        resolved
    }

    fn custom_status_line_payload(&self, cwd: &Path) -> Value {
        let model_id = self
            .session
            .models
            .current_model_id_str()
            .map(|s| s.to_string())
            .or_else(|| self.session.models.current_model_name())
            .unwrap_or_else(|| "unknown".to_string());
        let model_display = self
            .session
            .models
            .current_model_name()
            .unwrap_or_else(|| model_id.clone());
        let effort = self
            .session
            .models
            .reasoning_effort
            .map(|e| e.to_string());
        let (used, total, usage_pct) = match self.context_state.as_ref() {
            Some(c) => {
                let total = if c.total > 0 {
                    c.total
                } else {
                    self.session.models.get_context_window().unwrap_or(0)
                };
                let pct = if total > 0 {
                    ((c.used as f64 / total as f64) * 100.0).round() as u64
                } else {
                    c.usage_pct as u64
                };
                (c.used, total, pct)
            }
            None => {
                let total = self.session.models.get_context_window().unwrap_or(0);
                (0, total, 0)
            }
        };
        let session_id = self
            .session
            .session_id
            .as_ref()
            .map(|s| s.0.to_string());
        let project_dir = find_project_dir(cwd).unwrap_or_else(|| cwd.to_path_buf());
        let permission_mode = if self.session.yolo_mode {
            "yolo"
        } else if self.session.auto_mode {
            "auto"
        } else {
            "ask"
        };

        json!({
            "hook_event_name": "Status",
            "session_id": session_id,
            "version": VERSION,
            "workspace": {
                "current_dir": cwd.display().to_string(),
                "project_dir": project_dir.display().to_string(),
            },
            "model": {
                "id": model_id,
                "display_name": model_display,
            },
            "effort": {
                "level": effort,
            },
            "context_window": {
                "current_usage": {
                    "input_tokens": used,
                    "output_tokens": 0,
                    "cache_read_input_tokens": 0,
                    "cache_creation_input_tokens": 0,
                    "total_tokens": used,
                },
                "context_window_size": total,
                "used_percentage": usage_pct,
                "total_input_tokens": used,
                "total_output_tokens": 0,
                "total_tokens": used,
            },
            "cost": {
                "total_cost_usd": null,
                "total_duration_ms": null,
            },
            "permissions": {
                "mode": permission_mode,
                "label": permission_mode,
                "yolo": self.session.yolo_mode,
            },
        })
    }
}

fn run_custom_status_line_command(
    command: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
    payload: &Value,
    columns: u16,
) -> Option<String> {
    let payload_bytes = match serde_json::to_vec(payload) {
        Ok(b) => b,
        Err(err) => {
            tracing::debug!(error = %err, "custom status line: serialize payload failed");
            return None;
        }
    };

    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(command)
        .current_dir(cwd)
        .env("COLUMNS", columns.to_string())
        .env("LINES", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for (k, v) in env {
        cmd.env(k, v);
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid is async-signal-safe; keeps the statusline child out
        // of the TUI process group so Ctrl+C does not kill it mid-render.
        unsafe {
            cmd.pre_exec(|| {
                nix::unistd::setsid().ok();
                Ok(())
            });
        }
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(err) => {
            tracing::debug!(error = %err, command, "custom status line: spawn failed");
            return None;
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(&payload_bytes);
        // Drop closes stdin so the renderer can finish.
    }

    match child.wait_timeout(TIMEOUT) {
        Ok(Some(status)) if status.success() => {
            let mut stdout = child.stdout.take()?;
            let mut buf = Vec::new();
            let _ = std::io::Read::read_to_end(&mut stdout, &mut buf);
            if buf.len() > MAX_STDOUT_BYTES {
                buf.truncate(MAX_STDOUT_BYTES);
            }
            let text = String::from_utf8_lossy(&buf).into_owned();
            if custom_status_line::first_renderable_line(&text).is_some() {
                Some(text)
            } else {
                None
            }
        }
        Ok(Some(status)) => {
            tracing::debug!(
                status = ?status.code(),
                "custom status line: command exited non-zero"
            );
            None
        }
        Ok(None) => {
            let _ = child.kill();
            let _ = child.wait();
            tracing::debug!("custom status line: command timed out");
            None
        }
        Err(err) => {
            tracing::debug!(error = %err, "custom status line: wait failed");
            None
        }
    }
}

/// Best-effort git root (or cwd) for `workspace.project_dir`.
fn find_project_dir(cwd: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() {
        None
    } else {
        Some(PathBuf::from(path))
    }
}

/// Read Claude Code `statusLine` from user settings so Grok reuses the same
/// renderer without a duplicate Grok config entry.
fn load_claude_status_line_config() -> Option<CustomStatusLineConfig> {
    let home = dirs_next_home()?;
    let candidates = [
        home.join(".claude").join("settings.local.json"),
        home.join(".claude").join("settings.json"),
    ];
    for path in candidates {
        if let Some(cfg) = parse_claude_status_line_file(&path) {
            return Some(cfg);
        }
    }
    None
}

fn dirs_next_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn parse_claude_status_line_file(path: &Path) -> Option<CustomStatusLineConfig> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    let status = value.get("statusLine")?;
    let kind = status
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("command");
    if kind != "command" {
        return None;
    }
    let command = status.get("command")?.as_str()?.trim();
    if command.is_empty() {
        return None;
    }
    let mut env = BTreeMap::new();
    if let Some(map) = status.get("env").and_then(|v| v.as_object()) {
        for (k, v) in map {
            if let Some(s) = v.as_str() {
                env.insert(k.clone(), s.to_string());
            }
        }
    }
    let padding = status
        .get("padding")
        .and_then(|v| v.as_u64())
        .map(|n| n as u16)
        .unwrap_or(0);
    Some(CustomStatusLineConfig {
        kind: CustomStatusLineType::Command,
        command: command.to_string(),
        env,
        padding,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_claude_status_line_command() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{ "statusLine": { "type": "command", "command": "sox-agent-statusline" } }"#,
        )
        .unwrap();
        let cfg = parse_claude_status_line_file(&path).expect("parsed");
        assert_eq!(cfg.command, "sox-agent-statusline");
        assert_eq!(cfg.kind, CustomStatusLineType::Command);
    }

    #[test]
    fn parse_claude_status_line_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, r#"{ "theme": "dark" }"#).unwrap();
        assert!(parse_claude_status_line_file(&path).is_none());
    }

    #[test]
    fn run_echo_command_returns_stdout() {
        let payload = json!({"hook_event_name": "Status"});
        let out = run_custom_status_line_command(
            "cat >/dev/null; printf 'status-ok\\n'",
            &BTreeMap::new(),
            Path::new("/tmp"),
            &payload,
            80,
        );
        assert_eq!(out.as_deref().map(str::trim), Some("status-ok"));
    }
}

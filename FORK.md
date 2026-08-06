# Fork maintenance (`alleneubank/grok-build`)

Personal fork of [xai-org/grok-build](https://github.com/xai-org/grok-build) for
fleet dogfood (mise `github:` backend + optional Nix overlays).

Product commits stay on feature branches and may be PR'd upstream. Distribution
plumbing lives in fork-only commits (`[fork]` prefix) so an upstream diff stays
reviewable.

## Fleet platforms (required)

The shared mise pin must install on **every** host OS the fleet runs:

| Platform token   | How it is built                                                       |
|------------------|-----------------------------------------------------------------------|
| `darwin.arm64`   | Native `cargo build` on Apple Silicon Mac                             |
| `linux.x86_64`   | OrbStack/Docker Linux container, **cross-compile** to x86_64-gnu      |

(The Linux container usually runs as arm64 on Apple Silicon; the cargo target
is always `x86_64-unknown-linux-gnu` so mise `linux-x64` hosts get a real
amd64 binary.)

**A macOS-only GitHub release is not fleet-ready.** Linux hosts will fail
`mise install` / `dotfiles-upgrade` if the pin is shared and the linux asset is
missing. The release script refuses `--publish` until both archives exist.

Default required set (override only for intentional subset dogfood):

```text
FORK_REQUIRED_PLATFORMS=darwin.arm64,linux.x86_64
```

## Release from a Mac (canonical path)

Same shape as the codex fork: **one Apple Silicon Mac** builds both platforms
(the Linux binary via OrbStack/Docker). Do not publish from a single native
package step.

### Prerequisites

- Apple Silicon Mac (`uname -m` → `arm64`)
- Clean git tree at the commit to ship
- Rust toolchain from `rust-toolchain.toml`
- OrbStack or Docker Desktop (`docker info` works)
- `gh` authenticated to push tags and create releases on `alleneubank/grok-build`

### Commands

```sh
# 1) Plan (no side effects)
./scripts/release-fork.sh

# 2) Build macOS natively + Linux in linux/amd64 container
./scripts/release-fork.sh --run

# 3) Publish prerelease (after reviewing staged dist/fork-release/)
./scripts/release-fork.sh --publish
```

One-shot when publish is already authorized:

```sh
./scripts/release-fork.sh --run --publish
```

Artifacts:

```text
dist/fork-release/
  grok.<version>.darwin.arm64.tar.gz   # root entry: grok
  grok.<version>.linux.x86_64.tar.gz
  checksums.txt
```

Version scheme: `<cargo-base>-fork.<YYYYMMDD>.g<sha9>`  
Example: `0.2.120-fork.20260806.g7de4b33ec`  
Tag: `v` + version, **prerelease** so it never hijacks upstream “latest”.

### Linux container

Image: `.github/docker/fork-release-linux.Dockerfile`  
Tag: `grok-fork-release-linux:rust-1.94.0` (matches `rust-toolchain.toml`)

Built on demand by the release script:

```sh
docker build --platform linux/amd64 \
  -t grok-fork-release-linux:rust-1.94.0 \
  -f .github/docker/fork-release-linux.Dockerfile \
  .github/docker
```

## Auto-update

The binary still embeds the official xAI updater client. While the fork is
PATH-primary:

```toml
# ~/.grok/config.toml
[cli]
auto_update = false
```

or `GROK_DISABLE_AUTOUPDATER=1` / `grok --no-auto-update`.

## Consume (dotfiles / mise)

After a **fleet-complete** release:

```toml
# config/.config/mise/config.toml
"github:alleneubank/grok-build" = {
  version = "<version-without-v-prefix>",
  exe = "grok",
  os = ["linux", "macos"],
}
```

Then:

```sh
mise lock
# commit config.toml + mise.lock
```

Do **not** set `os = ["linux", "macos"]` until the release has both assets.
A temporary mac-only pin uses `os = ["macos"]` and is dogfood-only.

## Product notes (this fork)

- Lifecycle `PermissionRequest` when an interactive tool permission chooser shows
- Plugin hooks (`hooks/hooks.json` from active Claude-compat plugins) load at
  session cold start — no `/hooks` → reload workaround
- Command-backed custom status line under the prompt (Claude Code `statusLine` /
  Codex `tui.custom_status_line` shape). Resolution order:
  1. `[ui.custom_status_line]` in `~/.grok/config.toml`
  2. `statusLine` from `~/.claude/settings.local.json` / `settings.json`

  Example (optional — Claude settings already work without this):

  ```toml
  [ui.custom_status_line]
  type = "command"
  command = "sox-agent-statusline"
  padding = 0
  ```

## Related

- Gist: [Releasing a fork for mise + Nix](https://gist.github.com/alleneubank/bf7d25542a49b136671db0e4bb65226d)
- Codex fork release (same Mac + Linux container idea):
  `openai/codex` → `.github/scripts/fork-release.sh`

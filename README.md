# agy-t3-sync

Single-binary, cross-platform CLI & background daemon that keeps **T3 Code**'s built-in `oauth-personal` **Antigravity** provider synchronized with `antigravity-tools` (`agy`), automatically rotates accounts based on quota or schedule, and safely recycles idle T3 `antigravity acp` worker processes without interrupting active turns.

## Features

- **Real-time token sync (`oauth-personal`)**: Watches `~/.gemini/antigravity-cli/antigravity-oauth-token` every 5 seconds and atomically syncs it into all T3 Code `oauth-personal` Antigravity profiles (skipping `gemini-api-key` / proxy profiles), with automatic backup rotation (keeping the latest 3 backups).
- **Quota & schedule auto-rotate**: Monitors active account quota via `antigravity-server` (`http://127.0.0.1:8045`), automatically triggers `antigravity-tools switch <email> --target agy` when remaining quota falls below threshold (`<= 10%` by default), and supports `quota`, `schedule`, and `hybrid` modes.
- **Idle-safe process recycling (`auto_restart_idle`)**: Reads T3 Code's `statev2.sqlite` strictly read-only to identify stale `antigravity acp` workers from before the latest rotation. Idle workers are restarted cleanly (so the next turn resumes on the new account via `session/resume`), while running turns, pending approvals, or background tasks remain untouched until finished + grace period.
- **Per-bucket status view (`status --refresh`)**: Displays remaining quota across **Gemini Models** and **Claude & GPT Models** with both **5-hour** and **Weekly** windows plus reset countdowns.
- **Zero-dependency single binary**: Native Rust binary (bundled SQLite & rustls TLS) with automatic background service setup (`systemd --user` on Linux, Startup folder shortcut on Windows, LaunchAgent on macOS).

## Quick Install

### Linux / macOS

```sh
curl -fsSL https://raw.githubusercontent.com/samudrareggi/agy-t3-sync/main/install.sh | sh
```

### Windows (PowerShell)

```powershell
irm https://raw.githubusercontent.com/samudrareggi/agy-t3-sync/main/install.ps1 | iex
```

Both installers verify the release SHA-256 checksum against `SHA256SUMS`, install `agy-t3-sync` into your user bin directory, run `agy-t3-sync doctor`, and register the background service (`agy-t3-sync install`).

## Prerequisites

1. **T3 Code** signed in with an Antigravity **Google account** (`oauth-personal`).
2. **[Antigravity-Manager](https://github.com/lbjlaq/Antigravity-Manager/releases)** (`antigravity-tools` + `antigravity-server`) installed with your accounts imported.
3. **Linux only**: `libsecret-tools` (`secret-tool`) and an unlocked GNOME Keyring / Secret Service session (`gnome-keyring-daemon --unlock`) so `antigravity-tools switch` can update `agy` credentials in the background.

Run diagnostics anytime:

```sh
agy-t3-sync doctor
```

## CLI Usage

```sh
agy-t3-sync status                              # Cached sync, service, and 5h/weekly quota status
agy-t3-sync status --refresh                    # Refresh live quotas from antigravity-server first
agy-t3-sync doctor                              # Verify T3 profiles, binaries, keyring, and services
agy-t3-sync rotate-now                          # Switch immediately to highest-quota healthy account
agy-t3-sync rotate-next                         # Switch round-robin to next healthy account
agy-t3-sync restart-agy                         # Stop idle/other stale T3 agy ACP processes now
agy-t3-sync once                                # Run a single token sync pass
agy-t3-sync install [--no-server]               # Install & start background watcher (and server)
agy-t3-sync uninstall [--with-server]           # Stop & remove background service
```

### Auto-Rotate Configuration

```sh
agy-t3-sync config --groups gemini,claude --threshold 15
agy-t3-sync config --mode hybrid --schedule-minutes 60
agy-t3-sync config --auto-restart-idle --idle-grace 5
agy-t3-sync config --disable                    # Keep token sync running without auto-rotation
```

Legacy `--status`, `--doctor`, `--install`, `--uninstall`, `--rotate-now`, `--rotate-next`, `--restart-agy`, `--rotate-config`, and `--once` flags are also supported.

## Security & Privacy

- Reads `~/.gemini/antigravity-cli/antigravity-oauth-token` directly from `$HOME` (immune to T3's `GEMINI_HOME` override).
- Enforces private file permissions on Unix (`0700` for config/backup directories, `0600` for tokens, logs, config, and lock files).
- Never logs OAuth tokens, refresh tokens, API keys, or admin passwords; error lines are sanitized before logging.
- Binds `antigravity-server` strictly to `127.0.0.1:8045` when managed by `install`.

## Build from Source

```sh
git clone https://github.com/samudrareggi/agy-t3-sync.git
cd agy-t3-sync
cargo build --release
./target/release/agy-t3-sync install
```

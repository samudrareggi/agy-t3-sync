---
name: agy-t3-sync
description: >-
  Manage and inspect Antigravity multi-account synchronization, 5h/weekly quotas,
  auto-rotation, and T3 Code sync status via the agy-t3-sync CLI. Activate immediately
  when invoked with /agy-t3-sync or when asked to check Antigravity quota ("cek kuota agy",
  "sisa kuota antigravity"), rotate/switch accounts ("rotate akun agy", "ganti akun antigravity"),
  run diagnostics ("status agy-t3-sync", "doctor agy-t3-sync"), or configure auto-rotate.
---

# agy-t3-sync

Operates the `agy-t3-sync` CLI for account quota inspection, rotation, diagnostics, and auto-rotate configuration.

## Locating the Binary

Prefer `agy-t3-sync` directly in `PATH`. If not found on `PATH`, check:
- **Linux / macOS**: `"$HOME/.local/bin/agy-t3-sync"`
- **Windows (PowerShell)**: `"$env:LOCALAPPDATA\agy-t3-sync\bin\agy-t3-sync.exe"`

If the binary is missing, share the 1-line installer from `https://github.com/samudrareggi/agy-t3-sync` and ask the user before running an installation:
- Linux / macOS: `curl -fsSL https://raw.githubusercontent.com/samudrareggi/agy-t3-sync/main/install.sh | sh`
- Windows: `irm https://raw.githubusercontent.com/samudrareggi/agy-t3-sync/main/install.ps1 | iex`

## Command Routing (`/agy-t3-sync [action]`)

Map the user's request or `/agy-t3-sync` arguments as follows:

| Intent / Argument | Command | Notes |
| :--- | :--- | :--- |
| *(none)*, `status` | `agy-t3-sync status` | Refreshes live quotas via `antigravity-server` and shows 5h/weekly breakdown |
| `cached`, `--no-refresh` | `agy-t3-sync status --no-refresh` | Reads cached quota data immediately without contacting `antigravity-server` |
| `rotate`, `rotate-now`, `best` | `agy-t3-sync rotate-now && agy-t3-sync status --no-refresh` | Refreshes quotas, switches to the highest-quota healthy account, then prints status |
| `next`, `rotate-next` | `agy-t3-sync rotate-next && agy-t3-sync status --no-refresh` | Refreshes quotas, switches round-robin to the next healthy account, then prints status |
| `once`, `sync` | `agy-t3-sync once` | Runs a single token sync pass into T3 Code profiles |
| `doctor` | `agy-t3-sync doctor` | Verifies T3 profiles, state DB, `antigravity-tools`, `antigravity-server`, keyring/OS prereqs, and watcher |
| `config <flags>` | `agy-t3-sync config <flags>` | Updates auto-rotate settings (see valid flags below) |
| `install` | `agy-t3-sync install` | Registers/restarts watcher and `antigravity-server` service (optional `--no-server`) |
| `uninstall` | `agy-t3-sync uninstall` | Stops/removes watcher service (optional `--with-server`) |

### Valid `config` Flags

- `--enable` / `--disable`
- `--mode quota|schedule|hybrid`
- `--threshold <0..99>`
- `--groups <comma-list>` (e.g. `gemini`, `claude`, `gemini,claude`)
- `--schedule-minutes <N>`
- `--cooldown <seconds >= 10>`
- `--auto-restart-idle` / `--no-auto-restart-idle`
- `--idle-grace <seconds>`

## Reporting Guidance

After running `status` (or `rotate-now` / `rotate-next` + `status --no-refresh`), concisely summarize:
1. Active account (`agy / source`), T3 profile sync status (`[in sync]` vs `DIFFERENT`), and watcher/`antigravity-server` state.
2. Stale/fresh T3 `antigravity acp` worker counts and last rotation event (if present).
3. Both **Gemini Models** and **Claude & GPT Models** quota tables (5-hour and Weekly percentages + reset countdowns).

## Guardrails

- **NEVER run `agy-t3-sync restart-agy` or `agy-t3-sync --restart-agy`** from inside an active T3 Code / Antigravity agent session: stopping ancestor `antigravity acp` worker processes will terminate the active turn. Explain instead that stale idle workers recycle automatically when `auto_restart_idle` is enabled, or instruct the user to run `agy-t3-sync restart-agy` in an external terminal if needed.
- **NEVER read, print, or echo secrets** such as OAuth access/refresh tokens (`antigravity-oauth-token`), API keys, or `antigravity-server` admin passwords (`gui_config.json`).

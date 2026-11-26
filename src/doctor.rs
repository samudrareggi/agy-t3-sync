use crate::config::load_config;
use crate::lock::get_running_watcher_pid;
use crate::paths::{find_antigravity_server_bin, find_antigravity_tools_bin, AppPaths};
use crate::quota::{check_server_health, load_accounts};
use crate::service::get_systemd_status;
use crate::token::{get_t3_profiles, read_token_info};

#[cfg(any(target_os = "linux", windows))]
use crate::paths::executable_in_path;

pub fn cmd_doctor(paths: &AppPaths) -> i32 {
    let cfg = load_config(paths);
    let mut notes: Vec<String> = Vec::new();

    println!("agy-t3-sync doctor\n");

    // 1. T3 Code profiles & statev2.sqlite
    let profiles = get_t3_profiles(paths);
    if !paths.t3_home.is_dir() {
        println!("[MISSING] T3 Code data dir: not found ({})", paths.t3_home.display());
        notes.push("Install/run T3 Code and sign in to Antigravity (Google account) first.".into());
    } else if profiles.is_empty() {
        println!(
            "[WARN]    T3 Antigravity oauth-personal profiles: none found in {}",
            paths.profiles_root.display()
        );
        notes.push("Add/sign in an Antigravity (Google account / oauth-personal) provider in T3 Code.".into());
    } else {
        let names: Vec<&str> = profiles.iter().map(|p| p.name.as_str()).collect();
        println!(
            "[OK]      T3 oauth-personal profile(s): {} ({})",
            profiles.len(),
            names.join(", ")
        );
    }

    if paths.t3_state_db.is_file() {
        println!("[OK]      T3 state DB: {}", paths.t3_state_db.display());
    } else {
        println!(
            "[WARN]    T3 state DB missing ({}); idle-safe auto-restart will stay passive.",
            paths.t3_state_db.display()
        );
    }

    // 2. antigravity-tools CLI + accounts
    if let Some(bin) = find_antigravity_tools_bin(paths) {
        let (accounts, _, _) = load_accounts(paths, &cfg.monitored_groups);
        println!(
            "[OK]      antigravity-tools: {} ({} account(s) configured)",
            bin.display(),
            accounts.len()
        );
        if accounts.is_empty() {
            notes.push("Import or add accounts in Antigravity Tools (`antigravity-tools import <file>`).".into());
        }
    } else {
        println!("[MISSING] antigravity-tools binary not found");
        notes.push(
            "Install Antigravity-Manager (https://github.com/lbjlaq/Antigravity-Manager/releases) or set AGY_TOOLS_BIN."
                .into(),
        );
    }

    // 3. antigravity-server + health
    let srv_bin = find_antigravity_server_bin(paths);
    let srv_ok = check_server_health(&cfg.server_url);
    if srv_ok {
        println!("[OK]      antigravity-server: reachable at {}", cfg.server_url);
    } else if let Some(bin) = srv_bin {
        println!(
            "[WARN]    antigravity-server found ({}) but not responding at {}",
            bin.display(),
            cfg.server_url
        );
        notes.push("Run `agy-t3-sync install` to start antigravity-server for live quota auto-refresh.".into());
    } else {
        println!(
            "[WARN]    antigravity-server not running at {} (quota auto-refresh unavailable)",
            cfg.server_url
        );
        notes.push("Install Antigravity-Manager headless server or start `antigravity-server` for live quota rotation.".into());
    }

    // 4. Source token
    match read_token_info(&paths.source_token) {
        Some(tok) if !tok.refresh.is_empty() => {
            let email = if tok.email.is_empty() {
                "(unknown email)"
            } else {
                &tok.email
            };
            println!(
                "[OK]      agy source token: {} ({email})",
                paths.source_token.display()
            );
        }
        _ => {
            println!(
                "[WARN]    agy source token missing/empty at {}",
                paths.source_token.display()
            );
            notes.push("Run `antigravity-tools switch <account>` (or `agy --login`) to populate the source token.".into());
        }
    }

    // 5. OS-specific prerequisites
    #[cfg(target_os = "linux")]
    {
        if let Some(st) = executable_in_path(&["secret-tool"]) {
            println!("[OK]      Linux secret-tool: {}", st.display());
        } else {
            println!("[MISSING] Linux secret-tool not found (`antigravity-tools switch` requires it)");
            notes.push("Install libsecret-tools: `sudo apt install -y libsecret-tools` (or distro equivalent).".into());
        }

        let dbus_ok = std::process::Command::new("busctl")
            .args(["--user", "status", "org.freedesktop.secrets"])
            .output()
            .is_ok_and(|o| o.status.success());
        if dbus_ok {
            println!("[OK]      GNOME Keyring / Secret Service: available on user D-Bus");
        } else {
            println!("[WARN]    Secret Service (org.freedesktop.secrets) unavailable on user D-Bus");
            notes.push("Start/unlock keyring (`gnome-keyring-daemon --unlock`) so `antigravity-tools switch` works.".into());
        }

        let systemd_ok = std::process::Command::new("systemctl")
            .args(["--user", "show-environment"])
            .output()
            .is_ok_and(|o| o.status.success());
        if systemd_ok {
            println!("[OK]      systemd --user: active");
        } else {
            println!("[WARN]    systemd --user unreachable in current shell");
        }
    }

    #[cfg(windows)]
    {
        if executable_in_path(&["powershell.exe"]).is_some() {
            println!("[OK]      Windows PowerShell: available");
        } else {
            println!("[WARN]    powershell.exe not found in PATH");
        }
    }

    #[cfg(target_os = "macos")]
    {
        let agents_dir = paths.home_dir.join("Library").join("LaunchAgents");
        println!("[OK]      macOS LaunchAgents dir: {}", agents_dir.display());
    }

    // 6. Watcher status
    let lock_pid = get_running_watcher_pid(paths);
    let (systemd_active, main_pid) = get_systemd_status("agy-t3-sync.service");
    if systemd_active {
        let pid = lock_pid.unwrap_or(main_pid);
        println!("[OK]      watcher: running (systemd active, pid {pid})");
    } else if let Some(pid) = lock_pid {
        println!("[OK]      watcher: running (pid {pid})");
    } else {
        println!("[WARN]    watcher: not running");
        notes.push("Run `agy-t3-sync install` to install and start the background watcher.".into());
    }

    if !notes.is_empty() {
        println!("\nNext steps:");
        for note in notes {
            println!("  - {note}");
        }
    }
    0
}

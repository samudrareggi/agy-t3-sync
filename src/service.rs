use crate::config::load_config;
use crate::lock::get_running_watcher_pid;
use crate::paths::{find_antigravity_server_bin, AppPaths};
use crate::process::terminate_pid;
use std::env;
use std::process::Command;

#[cfg(target_os = "linux")]
pub fn get_systemd_status(unit_name: &str) -> (bool, u32) {
    let is_active = Command::new("systemctl")
        .args(["--user", "is-active", unit_name])
        .output()
        .is_ok_and(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "active");

    let main_pid = Command::new("systemctl")
        .args(["--user", "show", "-p", "MainPID", "--value", unit_name])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<u32>().ok())
        .unwrap_or(0);

    (is_active, main_pid)
}

#[cfg(not(target_os = "linux"))]
pub fn get_systemd_status(_unit_name: &str) -> (bool, u32) {
    (false, 0)
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;
    use crate::log::ensure_private_dir;
    use std::fs;
    use std::path::PathBuf;
    use std::thread;
    use std::time::Duration;

    const WATCHER_UNIT: &str = "agy-t3-sync.service";
    const SERVER_UNIT: &str = "antigravity-server.service";

    fn run_systemctl(args: &[&str]) -> Result<(), String> {
        let out = Command::new("systemctl")
            .arg("--user")
            .args(args)
            .output()
            .map_err(|e| format!("failed to run systemctl: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
            Err(if !err.is_empty() { err } else { stdout })
        }
    }

    pub fn install_service(
        paths: &AppPaths,
        interval: u64,
        with_server: bool,
    ) -> Result<(), String> {
        ensure_private_dir(&paths.base_dir)
            .map_err(|e| format!("failed to create config dir: {e}"))?;
        let _ = load_config(paths);

        let unit_dir = paths.home_dir.join(".config").join("systemd").join("user");
        fs::create_dir_all(&unit_dir)
            .map_err(|e| format!("failed to create systemd user dir: {e}"))?;

        if with_server {
            if let Some(server_bin) = find_antigravity_server_bin(paths) {
                let server_unit_path = unit_dir.join(SERVER_UNIT);
                let server_content = format!(
                    "[Unit]\n\
                     Description=Antigravity Manager headless server (quota refresh and local Web UI)\n\
                     After=network-online.target\n\
                     Wants=network-online.target\n\n\
                     [Service]\n\
                     Type=simple\n\
                     WorkingDirectory=%h\n\
                     ExecStart={} --host 127.0.0.1 --port 8045\n\
                     Restart=on-failure\n\
                     RestartSec=5\n\n\
                     [Install]\n\
                     WantedBy=default.target\n",
                    server_bin.display()
                );
                fs::write(&server_unit_path, server_content)
                    .map_err(|e| format!("failed to write {SERVER_UNIT}: {e}"))?;
                run_systemctl(&["daemon-reload"])?;
                run_systemctl(&["enable", "--now", SERVER_UNIT])?;
                println!("antigravity-server: enabled and running ({})", server_unit_path.display());
            } else {
                eprintln!(
                    "Note: antigravity-server binary not found; skipping {SERVER_UNIT} setup."
                );
            }
        }

        let exe_path: PathBuf = env::current_exe()
            .map_err(|e| format!("failed to determine current executable path: {e}"))?;
        let watch_args = if interval == 5 {
            "--watch".to_string()
        } else {
            format!("--watch --interval {interval}")
        };

        let watcher_unit_path = unit_dir.join(WATCHER_UNIT);
        let watcher_content = format!(
            "[Unit]\n\
             Description=Keep T3 Antigravity synced and rotated with agy token\n\
             After=default.target {SERVER_UNIT}\n\n\
             [Service]\n\
             Type=simple\n\
             WorkingDirectory=%h\n\
             ExecStart={} {watch_args}\n\
             Restart=on-failure\n\
             RestartSec=5\n\n\
             [Install]\n\
             WantedBy=default.target\n",
            exe_path.display()
        );
        fs::write(&watcher_unit_path, watcher_content)
            .map_err(|e| format!("failed to write {WATCHER_UNIT}: {e}"))?;

        if let Some(lock_pid) = get_running_watcher_pid(paths) {
            let (_, main_pid) = get_systemd_status(WATCHER_UNIT);
            if main_pid == 0 || lock_pid != main_pid {
                terminate_pid(lock_pid, 3.0);
                thread::sleep(Duration::from_millis(200));
            }
        }

        run_systemctl(&["daemon-reload"])?;
        run_systemctl(&["enable", WATCHER_UNIT])?;
        run_systemctl(&["restart", WATCHER_UNIT])?;

        println!(
            "agy-t3-sync: installed systemd user unit ({}) and started watcher.",
            watcher_unit_path.display()
        );
        Ok(())
    }

    pub fn uninstall_service(paths: &AppPaths, with_server: bool) -> Result<(), String> {
        let unit_dir = paths.home_dir.join(".config").join("systemd").join("user");
        let _ = run_systemctl(&["disable", "--now", WATCHER_UNIT]);
        let watcher_unit = unit_dir.join(WATCHER_UNIT);
        if watcher_unit.exists() {
            let _ = fs::remove_file(&watcher_unit);
        }

        if with_server {
            let _ = run_systemctl(&["disable", "--now", SERVER_UNIT]);
            let server_unit = unit_dir.join(SERVER_UNIT);
            if server_unit.exists() {
                let _ = fs::remove_file(&server_unit);
            }
        }

        let _ = run_systemctl(&["daemon-reload"]);
        if let Some(lock_pid) = get_running_watcher_pid(paths) {
            terminate_pid(lock_pid, 3.0);
        }

        println!(
            "agy-t3-sync: watcher stopped and systemd unit ({}) removed.",
            watcher_unit.display()
        );
        Ok(())
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use crate::log::ensure_private_dir;
    use crate::quota::check_server_health;
    use std::fs;
    use std::os::windows::process::CommandExt;
    use std::path::PathBuf;
    use std::thread;
    use std::time::Duration;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const DETACHED_PROCESS: u32 = 0x0000_0008;

    fn ps_quote(s: &str) -> String {
        s.replace('\'', "''")
    }

    fn run_hidden_ps(script: &str) -> Result<String, String> {
        let out = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| format!("failed to run powershell.exe: {e}"))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }

    fn startup_dir() -> Result<PathBuf, String> {
        if let Ok(path_str) = run_hidden_ps("[Environment]::GetFolderPath('Startup')") {
            if !path_str.is_empty() {
                return Ok(PathBuf::from(path_str));
            }
        }
        let appdata = env::var_os("APPDATA")
            .ok_or_else(|| "cannot resolve Windows Startup folder".to_string())?;
        Ok(PathBuf::from(appdata)
            .join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs")
            .join("Startup"))
    }

    fn create_shortcut(
        lnk_path: &std::path::Path,
        target: &std::path::Path,
        args: &str,
        workdir: &std::path::Path,
        desc: &str,
    ) -> Result<(), String> {
        let script = format!(
            "$ws = New-Object -ComObject WScript.Shell; \
             $lnk = $ws.CreateShortcut('{}'); \
             $lnk.TargetPath = '{}'; \
             $lnk.Arguments = '{}'; \
             $lnk.WorkingDirectory = '{}'; \
             $lnk.WindowStyle = 7; \
             $lnk.Description = '{}'; \
             $lnk.Save()",
            ps_quote(&lnk_path.to_string_lossy()),
            ps_quote(&target.to_string_lossy()),
            ps_quote(args),
            ps_quote(&workdir.to_string_lossy()),
            ps_quote(desc),
        );
        run_hidden_ps(&script).map(|_| ())
    }

    pub fn install_service(
        paths: &AppPaths,
        interval: u64,
        with_server: bool,
    ) -> Result<(), String> {
        ensure_private_dir(&paths.base_dir)
            .map_err(|e| format!("failed to create config dir: {e}"))?;
        let cfg = load_config(paths);
        let startup = startup_dir()?;
        fs::create_dir_all(&startup)
            .map_err(|e| format!("failed to access Startup folder: {e}"))?;

        if with_server {
            if let Some(server_bin) = find_antigravity_server_bin(paths) {
                let srv_lnk = startup.join("antigravity-server.lnk");
                create_shortcut(
                    &srv_lnk,
                    &server_bin,
                    "--host 127.0.0.1 --port 8045",
                    &paths.home_dir,
                    "Antigravity Manager headless server",
                )?;
                if !check_server_health(&cfg.server_url) {
                    let _ = Command::new(&server_bin)
                        .args(["--host", "127.0.0.1", "--port", "8045"])
                        .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS)
                        .spawn();
                }
                println!("antigravity-server: Startup shortcut created ({})", srv_lnk.display());
            } else {
                eprintln!("Note: antigravity-server.exe not found; skipping server Startup entry.");
            }
        }

        let exe_path = env::current_exe()
            .map_err(|e| format!("failed to determine current executable path: {e}"))?;
        let watch_args = if interval == 5 {
            "--watch".to_string()
        } else {
            format!("--watch --interval {interval}")
        };
        let lnk_path = startup.join("agy-t3-sync.lnk");
        create_shortcut(
            &lnk_path,
            &exe_path,
            &watch_args,
            &paths.base_dir,
            "Keep T3 Antigravity synced and rotated with agy token",
        )?;

        if let Some(lock_pid) = get_running_watcher_pid(paths) {
            terminate_pid(lock_pid, 3.0);
            thread::sleep(Duration::from_millis(300));
        }

        let mut cmd = Command::new(&exe_path);
        cmd.arg("--watch");
        if interval != 5 {
            cmd.args(["--interval", &interval.to_string()]);
        }
        cmd.creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS);
        cmd.spawn()
            .map_err(|e| format!("failed to start background watcher: {e}"))?;

        println!(
            "agy-t3-sync: Startup shortcut created ({}) and watcher started.",
            lnk_path.display()
        );
        Ok(())
    }

    pub fn uninstall_service(paths: &AppPaths, with_server: bool) -> Result<(), String> {
        if let Some(lock_pid) = get_running_watcher_pid(paths) {
            terminate_pid(lock_pid, 3.0);
        }
        let startup = startup_dir()?;
        let lnk = startup.join("agy-t3-sync.lnk");
        if lnk.exists() {
            let _ = fs::remove_file(&lnk);
        }
        if with_server {
            let srv_lnk = startup.join("antigravity-server.lnk");
            if srv_lnk.exists() {
                let _ = fs::remove_file(&srv_lnk);
            }
        }
        println!(
            "agy-t3-sync: watcher stopped and Startup shortcut ({}) removed.",
            lnk.display()
        );
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use crate::log::ensure_private_dir;
    use std::fs;
    use std::thread;
    use std::time::Duration;

    const WATCHER_LABEL: &str = "com.agy-t3-sync.watcher";
    const SERVER_LABEL: &str = "com.agy-t3-sync.server";

    fn xml_escape(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }

    pub fn install_service(
        paths: &AppPaths,
        interval: u64,
        with_server: bool,
    ) -> Result<(), String> {
        ensure_private_dir(&paths.base_dir)
            .map_err(|e| format!("failed to create config dir: {e}"))?;
        let _ = load_config(paths);

        let agents_dir = paths.home_dir.join("Library").join("LaunchAgents");
        fs::create_dir_all(&agents_dir)
            .map_err(|e| format!("failed to create LaunchAgents dir: {e}"))?;

        if with_server {
            if let Some(server_bin) = find_antigravity_server_bin(paths) {
                let server_plist = agents_dir.join(format!("{SERVER_LABEL}.plist"));
                let content = format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
                     <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
                     <plist version=\"1.0\">\n\
                     <dict>\n\
                       <key>Label</key><string>{SERVER_LABEL}</string>\n\
                       <key>ProgramArguments</key>\n\
                       <array>\n\
                         <string>{}</string>\n\
                         <string>--host</string><string>127.0.0.1</string>\n\
                         <string>--port</string><string>8045</string>\n\
                       </array>\n\
                       <key>RunAtLoad</key><true/>\n\
                       <key>KeepAlive</key><true/>\n\
                     </dict>\n\
                     </plist>\n",
                    xml_escape(&server_bin.to_string_lossy())
                );
                let _ = Command::new("launchctl")
                    .args(["unload", &server_plist.to_string_lossy()])
                    .output();
                fs::write(&server_plist, content)
                    .map_err(|e| format!("failed to write server plist: {e}"))?;
                let _ = Command::new("launchctl")
                    .args(["load", "-w", &server_plist.to_string_lossy()])
                    .status();
            }
        }

        let exe_path = env::current_exe()
            .map_err(|e| format!("failed to determine current executable path: {e}"))?;
        let plist_path = agents_dir.join(format!("{WATCHER_LABEL}.plist"));
        let mut args_xml = format!(
            "<string>{}</string>\n    <string>--watch</string>",
            xml_escape(&exe_path.to_string_lossy())
        );
        if interval != 5 {
            args_xml.push_str(&format!(
                "\n    <string>--interval</string>\n    <string>{interval}</string>"
            ));
        }
        let plist_content = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\">\n\
             <dict>\n\
               <key>Label</key><string>{WATCHER_LABEL}</string>\n\
               <key>ProgramArguments</key>\n\
               <array>\n    {args_xml}\n  </array>\n\
               <key>RunAtLoad</key><true/>\n\
               <key>KeepAlive</key><true/>\n\
             </dict>\n\
             </plist>\n"
        );

        let _ = Command::new("launchctl")
            .args(["unload", &plist_path.to_string_lossy()])
            .output();
        if let Some(lock_pid) = get_running_watcher_pid(paths) {
            terminate_pid(lock_pid, 3.0);
            thread::sleep(Duration::from_millis(200));
        }
        fs::write(&plist_path, plist_content)
            .map_err(|e| format!("failed to write LaunchAgent plist: {e}"))?;
        let out = Command::new("launchctl")
            .args(["load", "-w", &plist_path.to_string_lossy()])
            .output()
            .map_err(|e| format!("failed to run launchctl: {e}"))?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
        }

        println!(
            "agy-t3-sync: installed LaunchAgent ({}) and started watcher.",
            plist_path.display()
        );
        Ok(())
    }

    pub fn uninstall_service(paths: &AppPaths, with_server: bool) -> Result<(), String> {
        let agents_dir = paths.home_dir.join("Library").join("LaunchAgents");
        let plist_path = agents_dir.join(format!("{WATCHER_LABEL}.plist"));
        let _ = Command::new("launchctl")
            .args(["unload", &plist_path.to_string_lossy()])
            .output();
        if plist_path.exists() {
            let _ = fs::remove_file(&plist_path);
        }
        if with_server {
            let srv_plist = agents_dir.join(format!("{SERVER_LABEL}.plist"));
            let _ = Command::new("launchctl")
                .args(["unload", &srv_plist.to_string_lossy()])
                .output();
            if srv_plist.exists() {
                let _ = fs::remove_file(&srv_plist);
            }
        }
        if let Some(lock_pid) = get_running_watcher_pid(paths) {
            terminate_pid(lock_pid, 3.0);
        }
        println!(
            "agy-t3-sync: watcher stopped and LaunchAgent ({}) removed.",
            plist_path.display()
        );
        Ok(())
    }
}

pub use platform::{install_service, uninstall_service};

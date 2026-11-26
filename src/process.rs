use crate::config::{load_config, load_state};
use crate::log::write_sync_log;
use crate::paths::{canonical_or_raw, AppPaths};
use crate::t3_state::get_busy_thread_ids;
use crate::token::{get_t3_profiles, invoke_sync, read_token_info, sync_source_generation};
use std::collections::HashSet;
use std::path::Path;
use std::process;

#[derive(Clone, Debug)]
pub struct T3AgyProcess {
    pub pid: u32,
    pub thread_id: Option<String>,
    pub start_ts: Option<f64>,
    #[allow(dead_code)]
    pub gemini_dir: Option<String>,
}

fn parse_gemini_dir_arg(args: &[String]) -> Option<String> {
    for (idx, arg) in args.iter().enumerate() {
        if arg == "--gemini-dir" {
            if let Some(next) = args.get(idx + 1) {
                return Some(next.clone());
            }
        } else if let Some(rest) = arg.strip_prefix("--gemini-dir=") {
            return Some(rest.to_string());
        }
    }
    None
}

fn is_t3_acp_cmdline(args: &[String]) -> bool {
    if args.is_empty() {
        return false;
    }
    let exe_norm = args[0].replace('\\', "/").to_ascii_lowercase();
    exe_norm.contains("/.t3/tools/antigravity-acp/") && args[1..].iter().any(|a| a == "acp")
}

fn oauth_profile_dir_set(paths: &AppPaths) -> HashSet<String> {
    let mut set = HashSet::new();
    for profile in get_t3_profiles(paths) {
        if let Some(parent) = profile.acp_dir.parent() {
            set.insert(normalize_path_key(parent));
            set.insert(normalize_path_key(&canonical_or_raw(parent)));
        }
    }
    set
}

fn normalize_path_key(p: &Path) -> String {
    let raw = p.to_string_lossy().to_string();
    #[cfg(windows)]
    {
        let stripped = raw.strip_prefix(r"\\?\").unwrap_or(&raw);
        stripped.replace('\\', "/").to_ascii_lowercase()
    }
    #[cfg(not(windows))]
    {
        raw
    }
}

fn matches_oauth_dir(gdir: Option<&str>, oauth_dirs: &HashSet<String>) -> bool {
    let Some(dir_str) = gdir else {
        return false;
    };
    let p = Path::new(dir_str);
    oauth_dirs.contains(&normalize_path_key(p))
        || oauth_dirs.contains(&normalize_path_key(&canonical_or_raw(p)))
}

#[cfg(any(windows, target_os = "macos", test))]
fn split_command_line(cmd: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut in_token = false;

    for ch in cmd.chars() {
        match quote {
            Some(q) if ch == q => {
                quote = None;
            }
            Some(_) => {
                cur.push(ch);
                in_token = true;
            }
            None if ch == '"' || ch == '\'' => {
                quote = Some(ch);
                in_token = true;
            }
            None if ch.is_whitespace() => {
                if in_token {
                    args.push(std::mem::take(&mut cur));
                    in_token = false;
                }
            }
            None => {
                cur.push(ch);
                in_token = true;
            }
        }
    }
    if in_token {
        args.push(cur);
    }
    args
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;
    use std::fs;
    use std::thread;
    use std::time::{Duration, Instant};

    fn get_boot_time() -> Option<f64> {
        let raw = fs::read_to_string("/proc/stat").ok()?;
        for line in raw.lines() {
            if let Some(rest) = line.strip_prefix("btime ") {
                if let Ok(val) = rest.trim().parse::<f64>() {
                    return Some(val);
                }
            }
        }
        None
    }

    fn get_process_start_time(pid: u32, boot_time: Option<f64>) -> Option<f64> {
        let btime = boot_time.or_else(get_boot_time)?;
        let stat_text = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let rparen = stat_text.rfind(')')?;
        let after = stat_text.get(rparen + 2..)?;
        let fields: Vec<&str> = after.split_whitespace().collect();
        let starttime_ticks: f64 = fields.get(19)?.parse().ok()?;
        let clk_tck = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        if clk_tck <= 0 {
            return None;
        }
        Some(btime + (starttime_ticks / clk_tck as f64))
    }

    fn get_pid_thread_id(pid: u32) -> Option<String> {
        let raw = fs::read(format!("/proc/{pid}/environ")).ok()?;
        for item in raw.split(|&b| b == 0) {
            if let Some(val) = item.strip_prefix(b"T3CODE_THREAD_ID=") {
                let s = String::from_utf8_lossy(val).trim().to_string();
                if !s.is_empty() {
                    return Some(s);
                }
            }
        }
        None
    }

    pub fn is_pid_alive(pid: u32) -> bool {
        Path::new(&format!("/proc/{pid}")).exists()
    }

    pub fn get_ancestor_pids() -> HashSet<u32> {
        let mut ancestors = HashSet::new();
        let mut pid = process::id();
        while pid > 1 && ancestors.insert(pid) {
            let Ok(stat_text) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
                break;
            };
            let Some(rparen) = stat_text.rfind(')') else {
                break;
            };
            let Some(after) = stat_text.get(rparen + 2..) else {
                break;
            };
            let fields: Vec<&str> = after.split_whitespace().collect();
            let Ok(ppid) = fields.get(1).copied().unwrap_or("0").parse::<u32>() else {
                break;
            };
            if ppid == 0 {
                break;
            }
            pid = ppid;
        }
        ancestors
    }

    pub fn terminate_pid(pid: u32, timeout_secs: f64) -> bool {
        let rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        if rc != 0 {
            return false;
        }

        let deadline = Instant::now() + Duration::from_secs_f64(timeout_secs);
        while Instant::now() < deadline {
            let probe = unsafe { libc::kill(pid as libc::pid_t, 0) };
            if probe != 0 {
                let err = std::io::Error::last_os_error().raw_os_error();
                if err == Some(libc::ESRCH) {
                    return true;
                }
            }
            thread::sleep(Duration::from_millis(100));
        }

        let kill_rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
        kill_rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }

    pub fn find_t3_agy_processes(paths: &AppPaths, oauth_only: bool) -> Vec<T3AgyProcess> {
        let oauth_dirs = if oauth_only {
            oauth_profile_dir_set(paths)
        } else {
            HashSet::new()
        };
        let boot_time = get_boot_time();
        let Ok(entries) = fs::read_dir("/proc") else {
            return Vec::new();
        };

        let mut results = Vec::new();
        for entry in entries.filter_map(|e| e.ok()) {
            let name = entry.file_name();
            let Some(name_str) = name.to_str() else {
                continue;
            };
            let Ok(pid) = name_str.parse::<u32>() else {
                continue;
            };
            let Ok(raw_cmd) = fs::read(entry.path().join("cmdline")) else {
                continue;
            };
            if raw_cmd.is_empty() {
                continue;
            }
            let args: Vec<String> = raw_cmd
                .split(|&b| b == 0)
                .filter(|s| !s.is_empty())
                .map(|s| String::from_utf8_lossy(s).to_string())
                .collect();
            if !is_t3_acp_cmdline(&args) {
                continue;
            }

            let gdir = parse_gemini_dir_arg(&args);
            if oauth_only && !matches_oauth_dir(gdir.as_deref(), &oauth_dirs) {
                continue;
            }

            results.push(T3AgyProcess {
                pid,
                thread_id: get_pid_thread_id(pid),
                start_ts: get_process_start_time(pid, boot_time),
                gemini_dir: gdir,
            });
        }

        results.sort_by_key(|p| p.pid);
        results
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use crate::t3_state::now_epoch_secs;
    use std::collections::HashMap;
    use std::process::Command;
    use std::thread;
    use std::time::{Duration, Instant};

    fn parse_ps_etime_seconds(etime: &str) -> Option<f64> {
        let trimmed = etime.trim();
        let (days, rest) = match trimmed.split_once('-') {
            Some((d, r)) => (d.parse::<u64>().ok()?, r),
            None => (0, trimmed),
        };
        let parts: Vec<u64> = rest
            .split(':')
            .map(|p| p.parse::<u64>())
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        let secs = match parts.as_slice() {
            [m, s] => m * 60 + s,
            [h, m, s] => h * 3600 + m * 60 + s,
            _ => return None,
        };
        Some((days * 86400 + secs) as f64)
    }

    fn get_pid_thread_id(pid: u32) -> Option<String> {
        let out = Command::new("ps")
            .args(["eww", "-p", &pid.to_string(), "-o", "command="])
            .output()
            .ok()?;
        let line = String::from_utf8_lossy(&out.stdout);
        for token in line.split_whitespace() {
            if let Some(val) = token.strip_prefix("T3CODE_THREAD_ID=") {
                if !val.is_empty() {
                    return Some(val.to_string());
                }
            }
        }
        None
    }

    pub fn is_pid_alive(pid: u32) -> bool {
        let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
        rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    pub fn get_ancestor_pids() -> HashSet<u32> {
        let mut ppid_map = HashMap::new();
        if let Ok(out) = Command::new("ps").args(["-axo", "pid=,ppid="]).output() {
            let text = String::from_utf8_lossy(&out.stdout);
            for line in text.lines() {
                let mut cols = line.split_whitespace();
                if let (Some(p), Some(pp)) = (cols.next(), cols.next()) {
                    if let (Ok(pid), Ok(ppid)) = (p.parse::<u32>(), pp.parse::<u32>()) {
                        ppid_map.insert(pid, ppid);
                    }
                }
            }
        }
        let mut ancestors = HashSet::new();
        let mut cur = process::id();
        while cur > 1 && ancestors.insert(cur) {
            match ppid_map.get(&cur) {
                Some(&ppid) if ppid > 0 => cur = ppid,
                _ => break,
            }
        }
        ancestors
    }

    pub fn terminate_pid(pid: u32, timeout_secs: f64) -> bool {
        let rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        if rc != 0 {
            return false;
        }
        let deadline = Instant::now() + Duration::from_secs_f64(timeout_secs);
        while Instant::now() < deadline {
            if !is_pid_alive(pid) {
                return true;
            }
            thread::sleep(Duration::from_millis(100));
        }
        let kill_rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
        kill_rc == 0 || !is_pid_alive(pid)
    }

    pub fn find_t3_agy_processes(paths: &AppPaths, oauth_only: bool) -> Vec<T3AgyProcess> {
        let oauth_dirs = if oauth_only {
            oauth_profile_dir_set(paths)
        } else {
            HashSet::new()
        };
        let Ok(out) = Command::new("ps")
            .args(["-axo", "pid=,etime=,command="])
            .output()
        else {
            return Vec::new();
        };

        let now = now_epoch_secs();
        let mut results = Vec::new();
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let trimmed = line.trim_start();
            let mut parts = trimmed.splitn(3, char::is_whitespace);
            let Some(pid_str) = parts.next() else {
                continue;
            };
            let rest = parts.collect::<Vec<_>>().join(" ");
            let mut rest_parts = rest.trim_start().splitn(2, char::is_whitespace);
            let Some(etime_str) = rest_parts.next() else {
                continue;
            };
            let Some(cmd_str) = rest_parts.next() else {
                continue;
            };

            let Ok(pid) = pid_str.parse::<u32>() else {
                continue;
            };
            let args = split_command_line(cmd_str);
            if !is_t3_acp_cmdline(&args) {
                continue;
            }

            let gdir = parse_gemini_dir_arg(&args);
            if oauth_only && !matches_oauth_dir(gdir.as_deref(), &oauth_dirs) {
                continue;
            }

            let start_ts = parse_ps_etime_seconds(etime_str).map(|e| now - e);
            results.push(T3AgyProcess {
                pid,
                thread_id: get_pid_thread_id(pid),
                start_ts,
                gemini_dir: gdir,
            });
        }
        results.sort_by_key(|p| p.pid);
        results
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use serde_json::Value;
    use std::collections::HashMap;
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    fn run_hidden_powershell(script: &str) -> Option<String> {
        let out = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .ok()?;
        if out.status.success() {
            Some(String::from_utf8_lossy(&out.stdout).to_string())
        } else {
            None
        }
    }

    pub fn is_pid_alive(pid: u32) -> bool {
        let script = format!(
            "if (Get-Process -Id {pid} -ErrorAction SilentlyContinue) {{ exit 0 }} else {{ exit 1 }}"
        );
        Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .creation_flags(CREATE_NO_WINDOW)
            .status()
            .is_ok_and(|s| s.success())
    }

    pub fn get_ancestor_pids() -> HashSet<u32> {
        let mut ppid_map = HashMap::new();
        let script = "Get-CimInstance Win32_Process | ForEach-Object { \"$($_.ProcessId) $($_.ParentProcessId)\" }";
        if let Some(out) = run_hidden_powershell(script) {
            for line in out.lines() {
                let mut cols = line.split_whitespace();
                if let (Some(p), Some(pp)) = (cols.next(), cols.next()) {
                    if let (Ok(pid), Ok(ppid)) = (p.parse::<u32>(), pp.parse::<u32>()) {
                        ppid_map.insert(pid, ppid);
                    }
                }
            }
        }

        let mut ancestors = HashSet::new();
        let mut cur = process::id();
        while cur > 0 && ancestors.insert(cur) {
            match ppid_map.get(&cur) {
                Some(&ppid) if ppid > 0 && ppid != cur => cur = ppid,
                _ => break,
            }
        }
        ancestors
    }

    pub fn terminate_pid(pid: u32, _timeout_secs: f64) -> bool {
        Command::new("taskkill.exe")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .creation_flags(CREATE_NO_WINDOW)
            .status()
            .is_ok_and(|s| s.success())
    }

    pub fn find_t3_agy_processes(paths: &AppPaths, oauth_only: bool) -> Vec<T3AgyProcess> {
        let oauth_dirs = if oauth_only {
            oauth_profile_dir_set(paths)
        } else {
            HashSet::new()
        };

        let script = r#"
$items = @(Get-CimInstance Win32_Process | Where-Object {
  $_.ExecutablePath -like '*\.t3\tools\antigravity-acp\*' -and $_.CommandLine -match '\bacp\b'
} | ForEach-Object {
  [pscustomobject]@{
    pid = [uint32]$_.ProcessId
    cmd = [string]$_.CommandLine
    start_ms = if ($_.CreationDate) { ([DateTimeOffset]$_.CreationDate).ToUnixTimeMilliseconds() } else { $null }
  }
})
$items | ConvertTo-Json -Compress
"#;

        let Some(raw) = run_hidden_powershell(script) else {
            return Vec::new();
        };
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed == "null" {
            return Vec::new();
        }
        let Ok(val) = serde_json::from_str::<Value>(trimmed) else {
            return Vec::new();
        };
        let items: Vec<&Value> = match &val {
            Value::Array(arr) => arr.iter().collect(),
            Value::Object(_) => vec![&val],
            _ => Vec::new(),
        };

        let mut results = Vec::new();
        for item in items {
            let Some(obj) = item.as_object() else {
                continue;
            };
            let Some(pid) = obj.get("pid").and_then(|v| v.as_u64()).map(|n| n as u32) else {
                continue;
            };
            let cmd = obj.get("cmd").and_then(|v| v.as_str()).unwrap_or("");
            let args = split_command_line(cmd);
            if !is_t3_acp_cmdline(&args) {
                continue;
            }
            let gdir = parse_gemini_dir_arg(&args);
            if oauth_only && !matches_oauth_dir(gdir.as_deref(), &oauth_dirs) {
                continue;
            }
            let start_ts = obj
                .get("start_ms")
                .and_then(|v| v.as_i64())
                .map(|ms| ms as f64 / 1000.0);

            results.push(T3AgyProcess {
                pid,
                thread_id: None,
                start_ts,
                gemini_dir: gdir,
            });
        }
        results.sort_by_key(|p| p.pid);
        results
    }
}

pub use platform::{find_t3_agy_processes, get_ancestor_pids, is_pid_alive, terminate_pid};

pub fn reconcile_idle_agy_processes(paths: &AppPaths, quiet: bool) -> usize {
    let cfg = load_config(paths);
    if !cfg.auto_restart_idle {
        return 0;
    }

    let active_since = match read_token_info(&paths.source_token) {
        Some(ref src) if !src.refresh.is_empty() => sync_source_generation(paths, src),
        _ => load_state(paths).active_source_since,
    };
    if active_since <= 0.0 {
        return 0;
    }

    let procs = find_t3_agy_processes(paths, true);
    let stale: Vec<T3AgyProcess> = procs
        .into_iter()
        .filter(|p| p.start_ts.is_some_and(|st| st < (active_since - 1.0)))
        .collect();
    if stale.is_empty() {
        return 0;
    }

    let Some(busy) = get_busy_thread_ids(&paths.t3_state_db, cfg.idle_grace_seconds) else {
        return 0;
    };

    let ancestors = get_ancestor_pids();
    let mut stopped = 0;

    for proc in stale {
        if ancestors.contains(&proc.pid) {
            continue;
        }
        match proc.thread_id.as_deref() {
            Some(tid) if busy.contains(tid) => continue,
            None if !busy.is_empty() => continue,
            _ => {}
        }

        if terminate_pid(proc.pid, 3.0) {
            stopped += 1;
            let tid_short: String = proc
                .thread_id
                .as_deref()
                .map(|s| s.chars().take(8).collect())
                .unwrap_or_else(|| "unknown".to_string());
            write_sync_log(
                paths,
                &format!(
                    "[restart-idle] stopped idle stale T3 agy pid {} (thread {tid_short})",
                    proc.pid
                ),
                quiet,
            );
        }
    }

    if stopped > 0 {
        invoke_sync(paths, quiet);
    }
    stopped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cmdline_handles_quotes_and_gemini_dir() {
        let cmd = r#""C:\Users\Example\.t3\tools\antigravity-acp\agy.exe" acp --gemini-dir "C:\Users\Example\.t3\userdata\providers\antigravity\abcd" --app-data-dir antigravity-acp"#;
        let args = split_command_line(cmd);
        assert!(is_t3_acp_cmdline(&args));
        assert_eq!(
            parse_gemini_dir_arg(&args).as_deref(),
            Some(r"C:\Users\Example\.t3\userdata\providers\antigravity\abcd")
        );
    }
}

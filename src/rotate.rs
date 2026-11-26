use crate::config::{load_config, load_state, save_state, RotateState};
use crate::log::{sanitize_error_line, write_sync_log};
use crate::paths::{find_antigravity_tools_bin, AppPaths};
use crate::quota::{load_accounts, refresh_account_quota, refresh_all_quotas, select_candidate};
use crate::t3_state::now_epoch_secs;
use crate::token::invoke_sync;
use std::io::Read;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

fn run_switch_with_timeout(bin: &std::path::Path, target_email: &str, timeout: Duration) -> Result<(i32, String), String> {
    let mut cmd = Command::new(bin);
    cmd.args(["switch", target_email, "--target", "agy"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }

    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();

    let out_handle = thread::spawn(move || -> String {
        let mut buf = String::new();
        if let Some(mut r) = stdout_pipe {
            let _ = r.read_to_string(&mut buf);
        }
        buf
    });
    let err_handle = thread::spawn(move || -> String {
        let mut buf = String::new();
        if let Some(mut r) = stderr_pipe {
            let _ = r.read_to_string(&mut buf);
        }
        buf
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Ok(s),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err("command timed out after 30s".to_string());
                }
                thread::sleep(Duration::from_millis(100));
            }
            Err(e) => break Err(e.to_string()),
        }
    };

    let stdout = out_handle.join().unwrap_or_default();
    let stderr = err_handle.join().unwrap_or_default();

    match status {
        Ok(exit) => Ok((exit.code().unwrap_or(-1), format!("{stderr}\n{stdout}"))),
        Err(msg) => Err(msg),
    }
}

pub fn perform_switch(
    paths: &AppPaths,
    target_email: &str,
    reason: &str,
    from_label: &str,
    from_pct: &str,
    to_pct: &str,
    quiet: bool,
    state: &mut RotateState,
) -> bool {
    let Some(bin) = find_antigravity_tools_bin(paths) else {
        state.last_rotate_attempt_at = now_epoch_secs();
        let _ = save_state(paths, state);
        write_sync_log(
            paths,
            "[rotate] antigravity-tools binary not found in PATH or standard directories",
            quiet,
        );
        return false;
    };

    match run_switch_with_timeout(&bin, target_email, Duration::from_secs(30)) {
        Ok((0, _)) => {
            let now = now_epoch_secs();
            state.last_rotate_at = now;
            state.last_rotate_attempt_at = now;
            state.last_rotate_from = from_label.to_string();
            state.last_rotate_to = target_email.to_string();
            state.last_rotate_reason = reason.to_string();
            let _ = save_state(paths, state);

            write_sync_log(
                paths,
                &format!("[rotate] {from_label} ({from_pct}) -> {target_email} ({to_pct}) [{reason}]"),
                quiet,
            );
            invoke_sync(paths, quiet);
            true
        }
        Ok((rc, output)) => {
            state.last_rotate_attempt_at = now_epoch_secs();
            let _ = save_state(paths, state);
            let err_detail = sanitize_error_line(&output);
            write_sync_log(
                paths,
                &format!("[rotate] switch to {target_email} failed (rc={rc}): {err_detail}"),
                quiet,
            );
            false
        }
        Err(msg) => {
            state.last_rotate_attempt_at = now_epoch_secs();
            let _ = save_state(paths, state);
            write_sync_log(
                paths,
                &format!(
                    "[rotate] switch to {target_email} failed: {}",
                    sanitize_error_line(&msg)
                ),
                quiet,
            );
            false
        }
    }
}

pub fn maybe_auto_rotate(paths: &AppPaths, quiet: bool, force_mode: Option<&str>) -> bool {
    let cfg = load_config(paths);
    let mut state = load_state(paths);
    let now = now_epoch_secs();

    let groups = &cfg.monitored_groups;
    let threshold = cfg.quota_threshold;
    let server_url = &cfg.server_url;

    let (mut accounts, mut current, mut source_email) = if force_mode.is_none() {
        if !cfg.auto_rotate {
            return false;
        }
        let last_attempt = state.last_rotate_at.max(state.last_rotate_attempt_at);
        if last_attempt > 0.0 && (now - last_attempt) < cfg.cooldown_seconds as f64 {
            return false;
        }

        let (mut accs, mut cur, mut src_email) = load_accounts(paths, groups);
        if let Some(ref cur_acc) = cur {
            if (now - state.last_refresh_at) >= cfg.refresh_interval_seconds as f64
                && refresh_account_quota(paths, server_url, &cur_acc.id)
            {
                state.last_refresh_at = now_epoch_secs();
                let _ = save_state(paths, &state);
                (accs, cur, src_email) = load_accounts(paths, groups);
            }
        }
        (accs, cur, src_email)
    } else {
        load_accounts(paths, groups)
    };

    let reason: Option<&str> = match force_mode {
        Some("best") => Some("manual-best"),
        Some("next") => Some("manual-next"),
        _ => {
            let mode = cfg.rotate_mode.as_str();
            let mut trig = None;
            if matches!(mode, "quota" | "hybrid") {
                let depleted = current.as_ref().is_none_or(|c| {
                    !c.eligible || c.monitored_pct.is_some_and(|p| p <= threshold)
                });
                if depleted {
                    trig = Some("quota");
                }
            }
            if trig.is_none() && matches!(mode, "schedule" | "hybrid") && cfg.schedule_minutes > 0 {
                if state.last_rotate_at <= 0.0 {
                    state.last_rotate_at = now;
                    let _ = save_state(paths, &state);
                } else if (now - state.last_rotate_at) >= (cfg.schedule_minutes * 60) as f64 {
                    trig = Some("schedule");
                }
            }
            trig
        }
    };

    let Some(reason_str) = reason else {
        return false;
    };

    if refresh_all_quotas(paths, server_url) {
        state.last_refresh_at = now_epoch_secs();
        let _ = save_state(paths, &state);
        (accounts, current, source_email) = load_accounts(paths, groups);
    }

    if reason_str == "quota" && force_mode.is_none() {
        if let Some(ref c) = current {
            if c.eligible && c.monitored_pct.is_some_and(|p| p > threshold) {
                return false;
            }
        }
    }

    let from_label = current
        .as_ref()
        .map(|c| c.email.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| (!source_email.is_empty()).then_some(source_email.as_str()))
        .unwrap_or("(none)")
        .to_string();
    let from_pct = current
        .as_ref()
        .and_then(|c| c.monitored_pct)
        .map(|p| format!("{p}%"))
        .unwrap_or_else(|| "n/a".to_string());

    let Some(cand) = select_candidate(&accounts, current.as_ref(), threshold, reason_str) else {
        state.last_rotate_attempt_at = now_epoch_secs();
        let _ = save_state(paths, &state);
        write_sync_log(
            paths,
            &format!("[rotate] no better eligible candidate available (current={from_label} {from_pct})"),
            quiet,
        );
        return false;
    };

    let to_pct = cand
        .monitored_pct
        .map(|p| format!("{p}%"))
        .unwrap_or_else(|| "n/a".to_string());
    let target_email = cand.email.clone();

    perform_switch(
        paths,
        &target_email,
        reason_str,
        &from_label,
        &from_pct,
        &to_pct,
        quiet,
        &mut state,
    )
}

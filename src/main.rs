mod config;
mod doctor;
mod lock;
mod log;
mod paths;
mod process;
mod quota;
mod rotate;
mod service;
mod t3_state;
mod token;

use chrono::{DateTime, Local, TimeZone};
use clap::{Args, Parser, Subcommand, ValueEnum};
use config::{load_config, load_state, save_config, save_state};
use doctor::cmd_doctor;
use lock::{get_running_watcher_pid, WatcherLock};
use log::{sanitize_error_line, write_sync_log};
use paths::AppPaths;
use process::{
    find_t3_agy_processes, get_ancestor_pids, reconcile_idle_agy_processes, terminate_pid,
};
use quota::{
    check_server_health, format_account_status, format_bucket_cell, load_account_quota_buckets,
    load_accounts, refresh_all_quotas,
};
use rotate::maybe_auto_rotate;
use service::{get_systemd_status, install_service, uninstall_service};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use t3_state::{get_busy_thread_ids, now_epoch_secs};
use token::{get_t3_profiles, invoke_sync, read_token_info};

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
enum RotateModeArg {
    Quota,
    Schedule,
    Hybrid,
}

impl RotateModeArg {
    fn as_str(self) -> &'static str {
        match self {
            Self::Quota => "quota",
            Self::Schedule => "schedule",
            Self::Hybrid => "hybrid",
        }
    }
}

#[derive(Args, Clone, Debug, Default)]
struct ConfigArgs {
    /// Enable auto-rotate
    #[arg(long, conflicts_with = "disable")]
    enable: bool,
    /// Disable auto-rotate
    #[arg(long, conflicts_with = "enable")]
    disable: bool,
    /// Enable automatic restart of stale idle T3 agy processes
    #[arg(long, conflicts_with = "no_auto_restart_idle")]
    auto_restart_idle: bool,
    /// Disable automatic restart of stale idle T3 agy processes
    #[arg(long, conflicts_with = "auto_restart_idle")]
    no_auto_restart_idle: bool,
    /// Rotation mode
    #[arg(long, value_enum)]
    mode: Option<RotateModeArg>,
    /// Quota percentage threshold <= N% to trigger rotation (0-99)
    #[arg(long, value_parser = clap::value_parser!(i64).range(0..=99))]
    threshold: Option<i64>,
    /// Comma-separated model groups to monitor (e.g. gemini or gemini,claude)
    #[arg(long)]
    groups: Option<String>,
    /// Time-based rotation interval in minutes for schedule/hybrid mode
    #[arg(long, value_parser = clap::value_parser!(u64))]
    schedule_minutes: Option<u64>,
    /// Minimum seconds between rotation attempts (>= 10)
    #[arg(long, value_parser = clap::value_parser!(u64).range(10..))]
    cooldown: Option<u64>,
    /// Grace seconds after thread completion before stopping stale T3 agy process
    #[arg(long, value_parser = clap::value_parser!(u64))]
    idle_grace: Option<u64>,
}

impl ConfigArgs {
    fn any_set(&self) -> bool {
        self.enable
            || self.disable
            || self.auto_restart_idle
            || self.no_auto_restart_idle
            || self.mode.is_some()
            || self.threshold.is_some()
            || self.groups.is_some()
            || self.schedule_minutes.is_some()
            || self.cooldown.is_some()
            || self.idle_grace.is_some()
    }
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Run background sync + auto-rotate + idle-restart loop
    Watch {
        /// Token sync polling interval in seconds
        #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u64).range(1..))]
        interval: u64,
    },
    /// Run one token sync pass and exit
    Once,
    /// Show sync, watcher, server, and 5h/weekly quota breakdown (refreshes live quotas by default)
    Status {
        /// Skip live quota refresh and read cached quota data immediately
        #[arg(long, conflicts_with = "refresh")]
        no_refresh: bool,
        /// Refresh live quotas via antigravity-server before printing (default; kept for compatibility)
        #[arg(long, conflicts_with = "no_refresh")]
        refresh: bool,
    },
    /// Check prerequisites and diagnose setup issues
    Doctor,
    /// Sync once and restart running oauth-personal T3 agy processes
    RestartAgy,
    /// Refresh quotas and switch to highest-quota account
    RotateNow,
    /// Refresh quotas and switch round-robin to next healthy account
    RotateNext,
    /// Update auto-rotate settings in config.json
    Config(ConfigArgs),
    /// Install background service / startup entry and start watcher
    Install {
        /// Token sync polling interval in seconds
        #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u64).range(1..))]
        interval: u64,
        /// Skip setting up antigravity-server alongside the watcher
        #[arg(long)]
        no_server: bool,
    },
    /// Stop and remove background service / startup entry
    Uninstall {
        /// Also stop and remove antigravity-server service / startup entry
        #[arg(long)]
        with_server: bool,
    },
}

#[derive(Parser, Debug)]
#[command(
    name = "agy-t3-sync",
    version,
    about = "Cross-platform Antigravity account sync, quota auto-rotator, and idle-safe T3 Code restarter",
    args_conflicts_with_subcommands = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Run background sync + auto-rotate loop (legacy flag)
    #[arg(long, group = "legacy_action")]
    watch: bool,
    /// Run one sync pass and exit (legacy flag)
    #[arg(long, group = "legacy_action")]
    once: bool,
    /// Show sync, watcher, server, and 5h/weekly quota breakdown (refreshes live quotas by default; legacy flag)
    #[arg(long, group = "legacy_action")]
    status: bool,
    /// Check prerequisites and diagnose setup issues (legacy flag)
    #[arg(long, group = "legacy_action")]
    doctor: bool,
    /// Sync once and restart T3 agy ACP processes (legacy flag)
    #[arg(long, group = "legacy_action")]
    restart_agy: bool,
    /// Refresh quotas and switch to highest-quota account (legacy flag)
    #[arg(long, group = "legacy_action")]
    rotate_now: bool,
    /// Refresh quotas and switch round-robin to next healthy account (legacy flag)
    #[arg(long, group = "legacy_action")]
    rotate_next: bool,
    /// Update auto-rotate settings in config.json (legacy flag)
    #[arg(long, group = "legacy_action")]
    rotate_config: bool,
    /// Install background service / startup entry and start watcher (legacy flag)
    #[arg(long, group = "legacy_action")]
    install: bool,
    /// Stop and remove background service / startup entry (legacy flag)
    #[arg(long, group = "legacy_action")]
    uninstall: bool,

    /// Token sync polling interval in seconds
    #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u64).range(1..))]
    interval: u64,
    /// Skip live quota refresh and read cached quota data immediately (with --status)
    #[arg(long, conflicts_with = "refresh")]
    no_refresh: bool,
    /// Refresh live quotas via antigravity-server before printing (default with --status)
    #[arg(long, conflicts_with = "no_refresh")]
    refresh: bool,
    /// Skip setting up antigravity-server alongside the watcher (with --install)
    #[arg(long)]
    no_server: bool,
    /// Also stop and remove antigravity-server service / startup entry (with --uninstall)
    #[arg(long)]
    with_server: bool,

    #[command(flatten)]
    config_args: ConfigArgs,
}

fn format_local_epoch(secs: f64) -> String {
    let whole = secs.floor() as i64;
    if let Some(dt) = Local.timestamp_opt(whole, 0).single() {
        dt.format("%Y-%m-%d %H:%M:%S").to_string()
    } else if let Some(dt) = DateTime::from_timestamp(whole, 0) {
        dt.with_timezone(&Local)
            .format("%Y-%m-%d %H:%M:%S")
            .to_string()
    } else {
        "unknown".to_string()
    }
}

fn cmd_watch(paths: &AppPaths, interval: u64) -> i32 {
    let Some(_lock) = WatcherLock::acquire(paths) else {
        return 0;
    };

    let running = Arc::new(AtomicBool::new(true));
    let signal_flag = Arc::clone(&running);
    let _ = ctrlc::set_handler(move || {
        signal_flag.store(false, Ordering::SeqCst);
    });

    write_sync_log(
        paths,
        &format!("watcher started (pid {})", std::process::id()),
        true,
    );

    let mut last_rotate_check: Option<Instant> = None;
    while running.load(Ordering::SeqCst) {
        invoke_sync(paths, true);

        let cfg = load_config(paths);
        let should_check = match last_rotate_check {
            None => true,
            Some(prev) => prev.elapsed() >= Duration::from_secs(cfg.check_interval_seconds),
        };
        if should_check {
            last_rotate_check = Some(Instant::now());
            maybe_auto_rotate(paths, true, None);
        }

        reconcile_idle_agy_processes(paths, true);

        let sleep_deadline = Instant::now() + Duration::from_secs(interval);
        while running.load(Ordering::SeqCst) && Instant::now() < sleep_deadline {
            let remaining = sleep_deadline.saturating_duration_since(Instant::now());
            thread::sleep(remaining.min(Duration::from_millis(250)));
        }
    }

    0
}

fn cmd_status(paths: &AppPaths, refresh: bool) -> i32 {
    let cfg = load_config(paths);
    let mut state = load_state(paths);
    let mut refreshed_live = false;

    if refresh {
        if refresh_all_quotas(paths, &cfg.server_url) {
            refreshed_live = true;
            state.last_refresh_at = now_epoch_secs();
            let _ = save_state(paths, &state);
            state = load_state(paths);
        } else {
            eprintln!(
                "Warning: live refresh failed (antigravity-server unavailable), showing cached quota."
            );
        }
    }

    let src = read_token_info(&paths.source_token);
    let src_label = src
        .as_ref()
        .map(|s| s.email.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("(no token)");
    println!("{:<14} {}", "agy / source", src_label);

    for profile in get_t3_profiles(paths) {
        let dst = read_token_info(&profile.token);
        let in_sync = match (&dst, &src) {
            (Some(d), Some(s)) => d.refresh == s.refresh && d.email == s.email,
            _ => false,
        };
        let status_tag = if in_sync { "in sync" } else { "DIFFERENT" };
        let legacy = if profile.acp_dir.join("acp_token.json").is_file() {
            " (+ legacy acp_token.json)"
        } else {
            ""
        };
        let dst_label = dst
            .as_ref()
            .map(|d| d.email.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("(no token)");
        println!(
            "{:<14} {}  [{}]{}",
            format!("T3 {}", profile.name),
            dst_label,
            status_tag,
            legacy
        );
    }

    let lock_pid = get_running_watcher_pid(paths);
    let (systemd_active, main_pid) = get_systemd_status("agy-t3-sync.service");
    let watcher_text = if systemd_active && (lock_pid.is_some() || main_pid > 0) {
        let pid_val = lock_pid.unwrap_or(main_pid);
        format!("running (systemd active, pid {pid_val})")
    } else if let Some(pid) = lock_pid {
        format!("running (pid {pid})")
    } else {
        "not running".to_string()
    };
    println!("{:<14} {}", "watcher", watcher_text);

    let (srv_systemd, _) = get_systemd_status("antigravity-server.service");
    let srv_healthy = check_server_health(&cfg.server_url);
    let server_text = if srv_healthy && srv_systemd {
        format!("running (systemd active, {})", cfg.server_url)
    } else if srv_healthy {
        format!("running ({})", cfg.server_url)
    } else {
        format!("not running ({})", cfg.server_url)
    };
    println!("{:<14} {}", "server", server_text);

    println!("{:<14} {}", "auto-rotate", cfg.summary());

    let procs = find_t3_agy_processes(paths, true);
    let proc_summary = if procs.is_empty() {
        "0 running".to_string()
    } else {
        let since = state.active_source_since;
        let busy = get_busy_thread_ids(&paths.t3_state_db, cfg.idle_grace_seconds);
        let mut stale_busy = 0usize;
        let mut stale_idle = 0usize;
        let mut fresh = 0usize;
        for proc in &procs {
            let is_stale =
                since > 0.0 && proc.start_ts.is_some_and(|st| st < (since - 1.0));
            if !is_stale {
                fresh += 1;
            } else {
                let is_busy = match &busy {
                    None => true,
                    Some(b) => match proc.thread_id.as_deref() {
                        Some(tid) => b.contains(tid),
                        None => !b.is_empty(),
                    },
                };
                if is_busy {
                    stale_busy += 1;
                } else {
                    stale_idle += 1;
                }
            }
        }
        format!(
            "{} running ({stale_busy} stale/busy, {stale_idle} stale/idle, {fresh} fresh)",
            procs.len()
        )
    };
    println!("{:<14} {}", "T3 agy procs", proc_summary);

    if state.last_rotate_at > 0.0 {
        let ts = format_local_epoch(state.last_rotate_at);
        let frm = if state.last_rotate_from.is_empty() {
            "?"
        } else {
            &state.last_rotate_from
        };
        let to = if state.last_rotate_to.is_empty() {
            "?"
        } else {
            &state.last_rotate_to
        };
        let rsn = if state.last_rotate_reason.is_empty() {
            "?"
        } else {
            &state.last_rotate_reason
        };
        println!("{:<14} {}  {} -> {} [{}]", "last rotate", ts, frm, to, rsn);
    }

    let (accounts, active, _) = load_accounts(paths, &cfg.monitored_groups);
    if !accounts.is_empty() {
        let now = now_epoch_secs();
        let mut bucket_rows = Vec::with_capacity(accounts.len());
        let mut latest_updated = 0i64;
        for acc in accounts {
            let (buckets, lu) = load_account_quota_buckets(paths, &acc.id);
            latest_updated = latest_updated.max(lu);
            bucket_rows.push((acc, buckets));
        }

        let print_group_table = |title: &str, group_key: &str, b5h_key: &str, bwk_key: &str| {
            println!("\n{title}");
            println!(
                "  {:<1} {:<28} {:<6} {:>4} {:<16} {:<16} Status",
                "", "Email", "Tier", "Eff", "5h (reset)", "Weekly (reset)"
            );
            for (acc, buckets) in &bucket_rows {
                let mark = if active.as_ref().is_some_and(|a| a.id == acc.id) {
                    "*"
                } else {
                    " "
                };
                let eff_str = acc
                    .group_pcts
                    .get(group_key)
                    .copied()
                    .flatten()
                    .map(|v| format!("{v}%"))
                    .unwrap_or_else(|| "n/a".to_string());
                let cell_5h = format_bucket_cell(buckets.get(b5h_key), now);
                let cell_wk = format_bucket_cell(buckets.get(bwk_key), now);
                let st = format_account_status(acc);
                println!(
                    "  {mark} {:<28} {:<6} {:>4} {:<16} {:<16} {st}",
                    acc.email, acc.tier, eff_str, cell_5h, cell_wk
                );
            }
        };

        print_group_table(
            "Gemini Models (Neon, Carbon, Argon, Hydrogen, Orchestrator, Flash):",
            "gemini",
            "gemini-5h",
            "gemini-weekly",
        );
        print_group_table(
            "Claude & GPT Models (Opus 5.5, Sonnet 5.5, GPT-OSS 120B):",
            "claude",
            "3p-5h",
            "3p-weekly",
        );

        let updated_str = if latest_updated > 0 {
            format_local_epoch(latest_updated as f64)
        } else {
            "unknown".to_string()
        };
        let mode_hint = if refreshed_live {
            "live; use --no-refresh for cached"
        } else if refresh {
            "cached; server unavailable"
        } else {
            "cached; omit --no-refresh for live"
        };
        println!("\nQuota updated: {updated_str}  (* = active; {mode_hint})");
    }

    0
}

fn cmd_restart_agy(paths: &AppPaths) -> i32 {
    invoke_sync(paths, false);
    let procs = find_t3_agy_processes(paths, true);
    let ancestors = get_ancestor_pids();

    let mut skipped = Vec::new();
    let mut targets = Vec::new();
    for p in procs {
        if ancestors.contains(&p.pid) {
            skipped.push(p.pid);
        } else {
            targets.push(p.pid);
        }
    }

    if !skipped.is_empty() {
        eprintln!(
            "Warning: skipped ancestor T3 agy process(es) {:?} because this command is running inside them.",
            skipped
        );
    }

    if targets.is_empty() {
        if skipped.is_empty() {
            println!("No T3 agy processes running.");
        }
        return 0;
    }

    let mut stopped = 0usize;
    for pid in targets {
        if terminate_pid(pid, 3.0) {
            stopped += 1;
            write_sync_log(paths, &format!("stopped T3 agy pid {pid}"), true);
        }
    }
    invoke_sync(paths, false);
    println!(
        "Stopped {stopped} T3 agy process(es). The next Antigravity message starts a fresh one on the synced account."
    );
    0
}

fn cmd_rotate_config(paths: &AppPaths, args: &ConfigArgs) -> i32 {
    let mut cfg = load_config(paths);
    if args.enable {
        cfg.auto_rotate = true;
    } else if args.disable {
        cfg.auto_rotate = false;
    }
    if let Some(mode) = args.mode {
        cfg.rotate_mode = mode.as_str().to_string();
    }
    if let Some(threshold) = args.threshold {
        cfg.quota_threshold = threshold as i32;
    }
    if let Some(ref groups_str) = args.groups {
        let groups: Vec<String> = groups_str
            .split(',')
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        if groups.is_empty() {
            eprintln!("Error: --groups must contain at least one model group.");
            return 2;
        }
        cfg.monitored_groups = groups;
    }
    if let Some(mins) = args.schedule_minutes {
        cfg.schedule_minutes = mins;
    }
    if let Some(cd) = args.cooldown {
        cfg.cooldown_seconds = cd;
    }
    if args.auto_restart_idle {
        cfg.auto_restart_idle = true;
    } else if args.no_auto_restart_idle {
        cfg.auto_restart_idle = false;
    }
    if let Some(grace) = args.idle_grace {
        cfg.idle_grace_seconds = grace;
    }

    if let Err(err) = save_config(paths, &cfg) {
        eprintln!("Failed to save config: {}", sanitize_error_line(&err.to_string()));
        return 1;
    }
    let saved = load_config(paths);
    println!(
        "agy-t3-sync config saved ({}): {}",
        paths.config_path.display(),
        saved.summary()
    );
    0
}

fn should_refresh(no_refresh: bool, refresh: bool) -> bool {
    refresh || !no_refresh
}

fn main() {
    let cli = Cli::parse();
    let paths = AppPaths::detect();

    let code = match cli.command {
        Some(Commands::Watch { interval }) => cmd_watch(&paths, interval),
        Some(Commands::Once) => {
            invoke_sync(&paths, false);
            0
        }
        Some(Commands::Status { no_refresh, refresh }) => {
            cmd_status(&paths, should_refresh(no_refresh, refresh))
        }
        Some(Commands::Doctor) => cmd_doctor(&paths),
        Some(Commands::RestartAgy) => cmd_restart_agy(&paths),
        Some(Commands::RotateNow) => {
            if maybe_auto_rotate(&paths, false, Some("best")) {
                0
            } else {
                1
            }
        }
        Some(Commands::RotateNext) => {
            if maybe_auto_rotate(&paths, false, Some("next")) {
                0
            } else {
                1
            }
        }
        Some(Commands::Config(cfg_args)) => cmd_rotate_config(&paths, &cfg_args),
        Some(Commands::Install {
            interval,
            no_server,
        }) => match install_service(&paths, interval, !no_server) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("Failed to install service: {e}");
                1
            }
        },
        Some(Commands::Uninstall { with_server }) => {
            match uninstall_service(&paths, with_server) {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("Failed to uninstall service: {e}");
                    1
                }
            }
        }
        None => {
            if cli.config_args.any_set() && !cli.rotate_config {
                eprintln!(
                    "Error: config flags require either `agy-t3-sync config ...` or `--rotate-config`."
                );
                2
            } else if cli.refresh && !cli.status {
                eprintln!("Error: --refresh requires --status");
                2
            } else if cli.no_refresh && !cli.status {
                eprintln!("Error: --no-refresh requires --status");
                2
            } else if cli.no_server && !cli.install {
                eprintln!("Error: --no-server requires --install");
                2
            } else if cli.with_server && !cli.uninstall {
                eprintln!("Error: --with-server requires --uninstall");
                2
            } else if cli.once {
                invoke_sync(&paths, false);
                0
            } else if cli.status {
                cmd_status(&paths, should_refresh(cli.no_refresh, cli.refresh))
            } else if cli.doctor {
                cmd_doctor(&paths)
            } else if cli.restart_agy {
                cmd_restart_agy(&paths)
            } else if cli.rotate_now {
                if maybe_auto_rotate(&paths, false, Some("best")) {
                    0
                } else {
                    1
                }
            } else if cli.rotate_next {
                if maybe_auto_rotate(&paths, false, Some("next")) {
                    0
                } else {
                    1
                }
            } else if cli.rotate_config {
                cmd_rotate_config(&paths, &cli.config_args)
            } else if cli.install {
                match install_service(&paths, cli.interval, !cli.no_server) {
                    Ok(()) => 0,
                    Err(e) => {
                        eprintln!("Failed to install service: {e}");
                        1
                    }
                }
            } else if cli.uninstall {
                match uninstall_service(&paths, cli.with_server) {
                    Ok(()) => 0,
                    Err(e) => {
                        eprintln!("Failed to uninstall service: {e}");
                        1
                    }
                }
            } else {
                cmd_watch(&paths, cli.interval)
            }
        }
    };

    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_subcommand_defaults_to_live_refresh() {
        let cli = Cli::try_parse_from(["agy-t3-sync", "status"]).unwrap();
        match cli.command {
            Some(Commands::Status { no_refresh, refresh }) => {
                assert!(!no_refresh);
                assert!(!refresh);
                assert!(should_refresh(no_refresh, refresh));
            }
            other => panic!("unexpected command: {other:?}"),
        }

        let cli_no = Cli::try_parse_from(["agy-t3-sync", "status", "--no-refresh"]).unwrap();
        match cli_no.command {
            Some(Commands::Status { no_refresh, refresh }) => {
                assert!(no_refresh);
                assert!(!refresh);
                assert!(!should_refresh(no_refresh, refresh));
            }
            other => panic!("unexpected command: {other:?}"),
        }

        let cli_yes = Cli::try_parse_from(["agy-t3-sync", "status", "--refresh"]).unwrap();
        match cli_yes.command {
            Some(Commands::Status { no_refresh, refresh }) => {
                assert!(!no_refresh);
                assert!(refresh);
                assert!(should_refresh(no_refresh, refresh));
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn legacy_status_flag_defaults_to_live_refresh() {
        let cli = Cli::try_parse_from(["agy-t3-sync", "--status"]).unwrap();
        assert!(cli.status);
        assert!(should_refresh(cli.no_refresh, cli.refresh));

        let cli_no = Cli::try_parse_from(["agy-t3-sync", "--status", "--no-refresh"]).unwrap();
        assert!(cli_no.status);
        assert!(!should_refresh(cli_no.no_refresh, cli_no.refresh));

        let cli_yes = Cli::try_parse_from(["agy-t3-sync", "--status", "--refresh"]).unwrap();
        assert!(cli_yes.status);
        assert!(should_refresh(cli_yes.no_refresh, cli_yes.refresh));
    }

    #[test]
    fn status_refresh_and_no_refresh_conflict() {
        assert!(Cli::try_parse_from(["agy-t3-sync", "status", "--refresh", "--no-refresh"]).is_err());
        assert!(
            Cli::try_parse_from(["agy-t3-sync", "--status", "--refresh", "--no-refresh"]).is_err()
        );
    }
}

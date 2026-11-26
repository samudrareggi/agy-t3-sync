use crate::log::{ensure_private_dir, write_private_bytes_atomic};
use crate::paths::AppPaths;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Config {
    pub auto_rotate: bool,
    pub rotate_mode: String,
    pub quota_threshold: i32,
    pub monitored_groups: Vec<String>,
    pub check_interval_seconds: u64,
    pub refresh_interval_seconds: u64,
    pub cooldown_seconds: u64,
    pub schedule_minutes: u64,
    pub auto_restart_idle: bool,
    pub idle_grace_seconds: u64,
    pub max_backups: usize,
    pub server_url: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            auto_rotate: true,
            rotate_mode: "quota".to_string(),
            quota_threshold: 10,
            monitored_groups: vec!["gemini".to_string()],
            check_interval_seconds: 60,
            refresh_interval_seconds: 300,
            cooldown_seconds: 120,
            schedule_minutes: 0,
            auto_restart_idle: true,
            idle_grace_seconds: 5,
            max_backups: 3,
            server_url: "http://127.0.0.1:8045".to_string(),
        }
    }
}

impl Config {
    pub fn normalize(mut self) -> Self {
        let mode = self.rotate_mode.trim().to_ascii_lowercase();
        self.rotate_mode = match mode.as_str() {
            "quota" | "schedule" | "hybrid" => mode,
            _ => "quota".to_string(),
        };

        self.quota_threshold = self.quota_threshold.clamp(0, 99);

        let mut groups = Vec::new();
        for g in &self.monitored_groups {
            let norm = g.trim().to_ascii_lowercase();
            if !norm.is_empty() && !groups.contains(&norm) {
                groups.push(norm);
            }
        }
        if groups.is_empty() {
            groups.push("gemini".to_string());
        }
        self.monitored_groups = groups;

        self.check_interval_seconds = self.check_interval_seconds.max(5);
        self.refresh_interval_seconds = self.refresh_interval_seconds.max(10);
        self.cooldown_seconds = self.cooldown_seconds.max(10);
        self.max_backups = self.max_backups.clamp(1, 50);

        let trimmed = self.server_url.trim().trim_end_matches('/').to_string();
        self.server_url = if trimmed.is_empty() {
            "http://127.0.0.1:8045".to_string()
        } else {
            trimmed
        };
        self
    }

    pub fn summary(&self) -> String {
        let state_str = if self.auto_rotate { "enabled" } else { "disabled" };
        let mut parts = vec![
            state_str.to_string(),
            format!("mode={}", self.rotate_mode),
            format!("threshold<={}%", self.quota_threshold),
            format!("groups={}", self.monitored_groups.join(",")),
            format!("cooldown={}s", self.cooldown_seconds),
        ];
        if self.schedule_minutes > 0 || matches!(self.rotate_mode.as_str(), "schedule" | "hybrid") {
            parts.push(format!("schedule={}m", self.schedule_minutes));
        }
        if self.auto_restart_idle {
            parts.push(format!("restart-idle=on({}s)", self.idle_grace_seconds));
        } else {
            parts.push("restart-idle=off".to_string());
        }
        parts.join(" | ")
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RotateState {
    pub last_rotate_at: f64,
    pub last_rotate_attempt_at: f64,
    pub last_refresh_at: f64,
    pub last_rotate_from: String,
    pub last_rotate_to: String,
    pub last_rotate_reason: String,
    pub active_source_fp: String,
    pub active_source_since: f64,
}

pub fn load_config(paths: &AppPaths) -> Config {
    let _ = ensure_private_dir(&paths.base_dir);
    if !paths.config_path.is_file() {
        let cfg = Config::default();
        let _ = save_config(paths, &cfg);
        return cfg;
    }
    if let Ok(raw) = fs::read_to_string(&paths.config_path) {
        if let Ok(parsed) = serde_json::from_str::<Config>(&raw) {
            return parsed.normalize();
        }
    }
    Config::default()
}

pub fn save_config(paths: &AppPaths, cfg: &Config) -> io::Result<()> {
    ensure_private_dir(&paths.base_dir)?;
    let normalized = cfg.clone().normalize();
    let mut json = serde_json::to_string_pretty(&normalized)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    json.push('\n');
    write_private_bytes_atomic(&paths.config_path, json.as_bytes())
}

pub fn load_state(paths: &AppPaths) -> RotateState {
    if !paths.state_path.is_file() {
        return RotateState::default();
    }
    fs::read_to_string(&paths.state_path)
        .ok()
        .and_then(|raw| serde_json::from_str::<RotateState>(&raw).ok())
        .unwrap_or_default()
}

pub fn save_state(paths: &AppPaths, state: &RotateState) -> io::Result<()> {
    ensure_private_dir(&paths.base_dir)?;
    let mut json = serde_json::to_string_pretty(state)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    json.push('\n');
    write_private_bytes_atomic(&paths.state_path, json.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_normalizes_bounds_and_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_roots(
            dir.path().to_path_buf(),
            dir.path().join(".t3"),
            dir.path().join("cfg"),
            dir.path().join("tools"),
        );

        let mut cfg = Config {
            rotate_mode: "INVALID".into(),
            quota_threshold: 150,
            monitored_groups: vec![" Gemini ".into(), "gemini".into(), "CLAUDE".into()],
            check_interval_seconds: 1,
            cooldown_seconds: 2,
            max_backups: 0,
            server_url: " http://127.0.0.1:8045/ ".into(),
            ..Config::default()
        };
        cfg = cfg.normalize();
        assert_eq!(cfg.rotate_mode, "quota");
        assert_eq!(cfg.quota_threshold, 99);
        assert_eq!(cfg.monitored_groups, vec!["gemini", "claude"]);
        assert_eq!(cfg.check_interval_seconds, 5);
        assert_eq!(cfg.cooldown_seconds, 10);
        assert_eq!(cfg.max_backups, 1);
        assert_eq!(cfg.server_url, "http://127.0.0.1:8045");

        save_config(&paths, &cfg).unwrap();
        let loaded = load_config(&paths);
        assert_eq!(loaded, cfg);
    }
}

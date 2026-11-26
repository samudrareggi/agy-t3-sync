use crate::config::{load_config, load_state, save_state};
use crate::log::{ensure_private_dir, set_private_file_perms, write_private_bytes_atomic, write_sync_log};
use crate::paths::AppPaths;
use crate::t3_state::now_epoch_secs;
use chrono::Local;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

#[derive(Clone, Debug)]
pub struct TokenInfo {
    pub email: String,
    pub refresh: String,
    pub raw: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct T3Profile {
    pub name: String,
    pub acp_dir: PathBuf,
    pub token: PathBuf,
}

pub fn read_token_info(path: &Path) -> Option<TokenInfo> {
    if !path.is_file() {
        return None;
    }
    let raw = fs::read(path).ok()?;
    let text = std::str::from_utf8(&raw).ok()?;
    let data: Value = serde_json::from_str(text).ok()?;
    let obj = data.as_object()?;
    let refresh = obj
        .get("token")
        .and_then(|v| v.as_object())
        .and_then(|t| t.get("refresh_token"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let email = obj
        .get("id_token_email")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Some(TokenInfo {
        email,
        refresh,
        raw,
    })
}

pub fn compute_source_fp(src: &TokenInfo) -> String {
    let mut hasher = Sha256::new();
    hasher.update(src.email.as_bytes());
    hasher.update(b"\0");
    hasher.update(src.refresh.as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(16);
    for b in &digest[..8] {
        hex.push_str(&format!("{b:02x}"));
    }
    hex
}

pub fn sync_source_generation(paths: &AppPaths, src: &TokenInfo) -> f64 {
    let fp = compute_source_fp(src);
    let mut state = load_state(paths);
    if state.active_source_fp != fp {
        let since = if state.active_source_fp.is_empty() {
            if state.last_rotate_at > 0.0 {
                state.last_rotate_at
            } else {
                fs::metadata(&paths.source_token)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs_f64())
                    .unwrap_or_else(now_epoch_secs)
            }
        } else {
            now_epoch_secs()
        };
        state.active_source_fp = fp;
        state.active_source_since = since;
        let _ = save_state(paths, &state);
    }
    state.active_source_since
}

pub fn get_t3_profiles(paths: &AppPaths) -> Vec<T3Profile> {
    if !paths.profiles_root.is_dir() {
        return Vec::new();
    }
    let Ok(entries) = fs::read_dir(&paths.profiles_root) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|x| x.path()))
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();

    let mut profiles = Vec::new();
    for entry in dirs {
        let acp_dir = entry.join("antigravity-acp");
        let settings_path = acp_dir.join("settings.json");
        if !settings_path.is_file() {
            continue;
        }
        let auth_type: Option<String> = fs::read_to_string(&settings_path)
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .and_then(|v| {
                v.get("auth")?
                    .as_object()?
                    .get("type")?
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(ToString::to_string)
            });

        if let Some(ref t) = auth_type {
            if t != "oauth-personal" {
                continue;
            }
        }

        let dir_name = entry
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("profile");
        let short_name: String = dir_name.chars().take(12).collect();
        profiles.push(T3Profile {
            name: short_name,
            token: acp_dir.join("antigravity-oauth-token"),
            acp_dir,
        });
    }
    profiles
}

fn prune_old_backups(backup_dir: &Path, prefix: &str, max_keep: usize) {
    let Ok(entries) = fs::read_dir(backup_dir) else {
        return;
    };
    let mut matching: Vec<(String, PathBuf)> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let p = e.path();
            if !p.is_file() {
                return None;
            }
            let name = p.file_name()?.to_str()?.to_string();
            if name.starts_with(prefix) {
                Some((name, p))
            } else {
                None
            }
        })
        .collect();

    matching.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, path) in matching.into_iter().skip(max_keep) {
        let _ = fs::remove_file(path);
    }
}

pub fn invoke_sync(paths: &AppPaths, quiet: bool) -> bool {
    let Some(src) = read_token_info(&paths.source_token) else {
        return false;
    };
    if src.refresh.is_empty() {
        return false;
    }

    sync_source_generation(paths, &src);
    let max_backups = load_config(paths).max_backups;

    let mut changed = false;
    for profile in get_t3_profiles(paths) {
        let dst = read_token_info(&profile.token);
        if let Some(ref d) = dst {
            if d.refresh == src.refresh && d.email == src.email {
                continue;
            }
        }

        if ensure_private_dir(&paths.backup_dir).is_err() {
            continue;
        }
        let stamp = Local::now().format("%Y%m%d-%H%M%S").to_string();

        if profile.token.is_file() {
            if let Ok(existing) = fs::read(&profile.token) {
                let backup_name = format!("{}-antigravity-oauth-token.{stamp}", profile.name);
                let backup_path = paths.backup_dir.join(backup_name);
                let _ = write_private_bytes_atomic(&backup_path, &existing);
                prune_old_backups(
                    &paths.backup_dir,
                    &format!("{}-antigravity-oauth-token.", profile.name),
                    max_backups,
                );
            }
        }

        let legacy_path = profile.acp_dir.join("acp_token.json");
        if legacy_path.is_file() {
            let legacy_backup = paths
                .backup_dir
                .join(format!("{}-acp_token.json.{stamp}", profile.name));
            let moved = if fs::rename(&legacy_path, &legacy_backup).is_ok() {
                let _ = set_private_file_perms(&legacy_backup);
                true
            } else if let Ok(bytes) = fs::read(&legacy_path) {
                if write_private_bytes_atomic(&legacy_backup, &bytes).is_ok() {
                    let _ = fs::remove_file(&legacy_path);
                    true
                } else {
                    false
                }
            } else {
                false
            };
            if moved {
                write_sync_log(
                    paths,
                    &format!("[{}] moved legacy acp_token.json to backups", profile.name),
                    quiet,
                );
            }
        }

        if write_private_bytes_atomic(&profile.token, &src.raw).is_ok() {
            let from_email = dst
                .as_ref()
                .map(|d| d.email.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("(none)");
            let to_email = if src.email.is_empty() {
                "(unknown)"
            } else {
                src.email.as_str()
            };
            write_sync_log(
                paths,
                &format!("[{}] {from_email} -> {to_email}", profile.name),
                quiet,
            );
            changed = true;
        }
    }

    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_updates_oauth_personal_skips_api_key_and_prunes_backups() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let t3_home = home.join(".t3");
        let base_dir = home.join(".config").join("agy-t3-sync");
        let tools_dir = home.join(".antigravity_tools");
        let paths = AppPaths::from_roots(home.clone(), t3_home.clone(), base_dir, tools_dir);

        // Source token
        fs::create_dir_all(paths.source_token.parent().unwrap()).unwrap();
        let src_json = r#"{"token":{"access_token":"a","refresh_token":"rt-new","token_type":"Bearer"},"auth_method":"consumer","id_token_email":"new@example.com"}"#;
        fs::write(&paths.source_token, src_json).unwrap();

        // oauth-personal profile with old token and legacy acp_token.json
        let p1 = paths.profiles_root.join("aaaa1111bbbb2222").join("antigravity-acp");
        fs::create_dir_all(&p1).unwrap();
        fs::write(p1.join("settings.json"), r#"{"auth":{"type":"oauth-personal"}}"#).unwrap();
        fs::write(
            p1.join("antigravity-oauth-token"),
            r#"{"token":{"refresh_token":"rt-old"},"id_token_email":"old@example.com"}"#,
        )
        .unwrap();
        fs::write(p1.join("acp_token.json"), r#"{"legacy":true}"#).unwrap();

        // Seed 4 pre-existing backups to test pruning down to max_backups=3
        ensure_private_dir(&paths.backup_dir).unwrap();
        for idx in 1..=4 {
            fs::write(
                paths
                    .backup_dir
                    .join(format!("aaaa1111bbbb-antigravity-oauth-token.2025010{idx}-000000")),
                b"{}",
            )
            .unwrap();
        }

        // gemini-api-key profile must be skipped
        let p2 = paths.profiles_root.join("cccc3333dddd4444").join("antigravity-acp");
        fs::create_dir_all(&p2).unwrap();
        fs::write(p2.join("settings.json"), r#"{"auth":{"type":"gemini-api-key"}}"#).unwrap();

        assert!(invoke_sync(&paths, true));
        assert_eq!(fs::read_to_string(p1.join("antigravity-oauth-token")).unwrap(), src_json);
        assert!(!p1.join("acp_token.json").exists());
        assert!(!p2.join("antigravity-oauth-token").exists());

        let token_backups: Vec<_> = fs::read_dir(&paths.backup_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("aaaa1111bbbb-antigravity-oauth-token.")
            })
            .collect();
        assert_eq!(token_backups.len(), 3);

        // Second sync with unchanged source is a no-op
        assert!(!invoke_sync(&paths, true));
    }
}

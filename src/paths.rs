use std::env;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct AppPaths {
    pub home_dir: PathBuf,
    pub source_token: PathBuf,
    pub t3_home: PathBuf,
    pub profiles_root: PathBuf,
    pub t3_state_db: PathBuf,
    pub base_dir: PathBuf,
    pub log_path: PathBuf,
    pub backup_dir: PathBuf,
    pub lock_path: PathBuf,
    pub config_path: PathBuf,
    pub state_path: PathBuf,
    pub accounts_index_path: PathBuf,
    pub accounts_dir: PathBuf,
    pub gui_config_path: PathBuf,
}

impl AppPaths {
    /// Detect paths from the real user home directory.
    ///
    /// Always resolves `~/.gemini/antigravity-cli/antigravity-oauth-token` via `home_dir`,
    /// never via `GEMINI_HOME` (which T3 Code overrides for child processes).
    pub fn detect() -> Self {
        let home_dir = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let t3_home = env::var_os("T3CODE_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir.join(".t3"));
        let base_dir = home_dir.join(".config").join("agy-t3-sync");
        let antigravity_tools_dir = home_dir.join(".antigravity_tools");
        Self::from_roots(home_dir, t3_home, base_dir, antigravity_tools_dir)
    }

    pub fn from_roots(
        home_dir: PathBuf,
        t3_home: PathBuf,
        base_dir: PathBuf,
        antigravity_tools_dir: PathBuf,
    ) -> Self {
        let source_token = home_dir
            .join(".gemini")
            .join("antigravity-cli")
            .join("antigravity-oauth-token");
        let profiles_root = t3_home
            .join("userdata")
            .join("providers")
            .join("antigravity");
        let t3_state_db = t3_home.join("userdata").join("statev2.sqlite");

        Self {
            home_dir,
            source_token,
            t3_home,
            profiles_root,
            t3_state_db,
            log_path: base_dir.join("sync.log"),
            backup_dir: base_dir.join("backups"),
            lock_path: base_dir.join("watcher.lock"),
            config_path: base_dir.join("config.json"),
            state_path: base_dir.join("rotate_state.json"),
            base_dir,
            accounts_index_path: antigravity_tools_dir.join("accounts.json"),
            accounts_dir: antigravity_tools_dir.join("accounts"),
            gui_config_path: antigravity_tools_dir.join("gui_config.json"),
        }
    }
}

pub fn executable_in_path(names: &[&str]) -> Option<PathBuf> {
    let path_var = env::var_os("PATH")?;
    for dir in env::split_paths(&path_var) {
        for name in names {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn find_binary(env_override: &str, names: &[&str], extra_paths: &[PathBuf]) -> Option<PathBuf> {
    if let Some(val) = env::var_os(env_override) {
        if !val.is_empty() {
            let p = PathBuf::from(val);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    if let Some(found) = executable_in_path(names) {
        return Some(found);
    }
    extra_paths.iter().find(|p| p.is_file()).cloned()
}

pub fn find_antigravity_tools_bin(paths: &AppPaths) -> Option<PathBuf> {
    let mut extras = Vec::new();
    #[cfg(windows)]
    {
        let names = ["antigravity-tools.exe", "antigravity-tools.cmd"];
        for env_key in ["LOCALAPPDATA", "ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(base) = env::var_os(env_key) {
                let b = PathBuf::from(base);
                extras.push(b.join("Programs").join("Antigravity Tools").join("antigravity-tools.exe"));
                extras.push(b.join("Antigravity Tools").join("antigravity-tools.exe"));
            }
        }
        return find_binary("AGY_TOOLS_BIN", &names, &extras);
    }
    #[cfg(target_os = "macos")]
    {
        extras.push(PathBuf::from(
            "/Applications/Antigravity Tools.app/Contents/MacOS/antigravity-tools",
        ));
        extras.push(PathBuf::from("/opt/homebrew/bin/antigravity-tools"));
        extras.push(PathBuf::from("/usr/local/bin/antigravity-tools"));
        extras.push(paths.home_dir.join(".local").join("bin").join("antigravity-tools"));
        return find_binary("AGY_TOOLS_BIN", &["antigravity-tools"], &extras);
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        extras.push(PathBuf::from("/usr/bin/antigravity-tools"));
        extras.push(PathBuf::from("/usr/local/bin/antigravity-tools"));
        extras.push(paths.home_dir.join(".local").join("bin").join("antigravity-tools"));
        find_binary("AGY_TOOLS_BIN", &["antigravity-tools"], &extras)
    }
}

pub fn find_antigravity_server_bin(paths: &AppPaths) -> Option<PathBuf> {
    let mut extras = Vec::new();
    #[cfg(windows)]
    {
        for env_key in ["LOCALAPPDATA", "ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(base) = env::var_os(env_key) {
                let b = PathBuf::from(base);
                extras.push(b.join("Programs").join("Antigravity Tools").join("antigravity-server.exe"));
                extras.push(b.join("Antigravity Tools").join("antigravity-server.exe"));
            }
        }
        return find_binary("AGY_SERVER_BIN", &["antigravity-server.exe"], &extras);
    }
    #[cfg(target_os = "macos")]
    {
        extras.push(PathBuf::from(
            "/Applications/Antigravity Tools.app/Contents/MacOS/antigravity-server",
        ));
        extras.push(PathBuf::from("/opt/homebrew/bin/antigravity-server"));
        extras.push(PathBuf::from("/usr/local/bin/antigravity-server"));
        extras.push(paths.home_dir.join(".local").join("bin").join("antigravity-server"));
        return find_binary("AGY_SERVER_BIN", &["antigravity-server"], &extras);
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        extras.push(PathBuf::from("/usr/bin/antigravity-server"));
        extras.push(PathBuf::from("/usr/local/bin/antigravity-server"));
        extras.push(paths.home_dir.join(".local").join("bin").join("antigravity-server"));
        find_binary("AGY_SERVER_BIN", &["antigravity-server"], &extras)
    }
}

pub fn canonical_or_raw(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

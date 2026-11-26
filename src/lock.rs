use crate::log::{ensure_private_dir, set_private_file_perms, write_private_bytes_atomic};
use crate::paths::AppPaths;
use crate::process::is_pid_alive;
use fs2::FileExt;
use std::fs::{self, File, OpenOptions};
use std::path::PathBuf;
use std::process;

pub struct WatcherLock {
    file: File,
    pid_path: PathBuf,
}

impl WatcherLock {
    pub fn acquire(paths: &AppPaths) -> Option<Self> {
        ensure_private_dir(&paths.base_dir).ok()?;

        let mut opts = OpenOptions::new();
        opts.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }

        let file = opts.open(&paths.lock_path).ok()?;
        let _ = set_private_file_perms(&paths.lock_path);

        if file.try_lock_exclusive().is_err() {
            return None;
        }

        // Store PID in a separate unlocked file so Windows mandatory locks never block readers.
        let _ = write_private_bytes_atomic(
            &paths.pid_path,
            format!("{}\n", process::id()).as_bytes(),
        );

        Some(Self {
            file,
            pid_path: paths.pid_path.clone(),
        })
    }
}

impl Drop for WatcherLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
        let _ = fs::remove_file(&self.pid_path);
    }
}

pub fn get_running_watcher_pid(paths: &AppPaths) -> Option<u32> {
    if !paths.lock_path.exists() {
        return None;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&paths.lock_path)
        .ok()?;

    if file.try_lock_exclusive().is_ok() {
        let _ = file.unlock();
        return None;
    }

    #[cfg(unix)]
    let raw = fs::read_to_string(&paths.pid_path)
        .ok()
        .or_else(|| fs::read_to_string(&paths.lock_path).ok())?;
    #[cfg(not(unix))]
    let raw = fs::read_to_string(&paths.pid_path).ok()?;

    let pid: u32 = raw.trim().parse().ok()?;
    if pid > 0 && is_pid_alive(pid) {
        Some(pid)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watcher_lock_prevents_double_acquire() {
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_roots(
            dir.path().to_path_buf(),
            dir.path().join(".t3"),
            dir.path().join("cfg"),
            dir.path().join("tools"),
        );

        let lock1 = WatcherLock::acquire(&paths);
        assert!(lock1.is_some());

        // Second non-blocking lock attempt inside same/child handle must fail.
        let lock2 = WatcherLock::acquire(&paths);
        assert!(lock2.is_none());

        let running_pid = get_running_watcher_pid(&paths);
        assert_eq!(running_pid, Some(process::id()));

        drop(lock1);
        assert!(get_running_watcher_pid(&paths).is_none());
    }
}

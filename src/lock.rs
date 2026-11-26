use crate::log::{ensure_private_dir, set_private_file_perms};
use crate::paths::AppPaths;
use crate::process::is_pid_alive;
use fs2::FileExt;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::process;

pub struct WatcherLock {
    file: File,
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

        let mut file = opts.open(&paths.lock_path).ok()?;
        let _ = set_private_file_perms(&paths.lock_path);

        if file.try_lock_exclusive().is_err() {
            return None;
        }

        let _ = file.set_len(0);
        let _ = file.seek(SeekFrom::Start(0));
        let _ = writeln!(file, "{}", process::id());
        let _ = file.flush();

        Some(Self { file })
    }
}

impl Drop for WatcherLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

pub fn get_running_watcher_pid(paths: &AppPaths) -> Option<u32> {
    if !paths.lock_path.exists() {
        return None;
    }
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&paths.lock_path)
        .ok()?;

    if file.try_lock_exclusive().is_ok() {
        let _ = file.unlock();
        return None;
    }

    let _ = file.seek(SeekFrom::Start(0));
    let mut raw = String::new();
    let _ = file.read_to_string(&mut raw);
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

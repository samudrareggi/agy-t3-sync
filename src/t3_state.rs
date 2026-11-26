use chrono::{DateTime, NaiveDateTime, Utc};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const TERMINAL_RUN_STATUSES: [&str; 5] = [
    "completed",
    "failed",
    "cancelled",
    "interrupted",
    "rolled_back",
];

const IDLE_PROVIDER_THREAD_STATUSES: [&str; 6] = [
    "idle",
    "stopped",
    "closed",
    "archived",
    "completed",
    "failed",
];

pub fn parse_iso_timestamp(val: &str) -> Option<f64> {
    let trimmed = val.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(trimmed) {
        return Some(dt.timestamp() as f64 + (dt.timestamp_subsec_nanos() as f64 / 1e9));
    }
    let normalized = if let Some(stripped) = trimmed.strip_suffix('Z') {
        format!("{stripped}+00:00")
    } else {
        trimmed.to_string()
    };
    if let Ok(dt) = DateTime::parse_from_rfc3339(&normalized) {
        return Some(dt.timestamp() as f64 + (dt.timestamp_subsec_nanos() as f64 / 1e9));
    }
    if let Ok(ndt) = NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%dT%H:%M:%S%.f")
        .or_else(|_| NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M:%S%.f"))
        .or_else(|_| NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%dT%H:%M:%S"))
        .or_else(|_| NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M:%S"))
    {
        let dt = DateTime::<Utc>::from_naive_utc_and_offset(ndt, Utc);
        return Some(dt.timestamp() as f64 + (dt.timestamp_subsec_nanos() as f64 / 1e9));
    }
    None
}

pub fn now_epoch_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Query T3 `statev2.sqlite` strictly read-only for busy, recently completed, or pending threads.
///
/// Returns `None` on any read or SQLite error so callers fail safe and never terminate processes.
pub fn get_busy_thread_ids(db_path: &Path, grace_seconds: u64) -> Option<HashSet<String>> {
    if !db_path.is_file() {
        return None;
    }

    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(db_path, flags).ok()?;
    let _ = conn.busy_timeout(Duration::from_secs(2));

    let now = now_epoch_secs();
    let grace = grace_seconds as f64;
    let mut busy = HashSet::new();

    {
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT thread_id FROM orchestration_v2_projection_runs \
                 WHERE status NOT IN (?, ?, ?, ?, ?)",
            )
            .ok()?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(TERMINAL_RUN_STATUSES), |row| {
                row.get::<_, Option<String>>(0)
            })
            .ok()?;
        for item in rows {
            let tid = item.ok()?;
            if let Some(id) = tid.filter(|s| !s.is_empty()) {
                busy.insert(id);
            }
        }
    }

    {
        let mut stmt = conn
            .prepare(
                "SELECT thread_id, status, updated_at, payload_json \
                 FROM orchestration_v2_projection_provider_threads \
                 WHERE provider_instance_id = 'antigravity' OR driver = 'antigravity'",
            )
            .ok()?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })
            .ok()?;

        for item in rows {
            let (tid_opt, status_opt, updated_opt, payload_opt) = item.ok()?;
            let Some(tid) = tid_opt.filter(|s| !s.is_empty()) else {
                continue;
            };

            let status = status_opt.unwrap_or_default().to_ascii_lowercase();
            if !IDLE_PROVIDER_THREAD_STATUSES.contains(&status.as_str()) {
                busy.insert(tid.clone());
            }

            if let Some(raw_payload) = payload_opt {
                if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&raw_payload) {
                    if let Some(Value::Array(tasks)) = map.get("pendingBackgroundTasks") {
                        if !tasks.is_empty() {
                            busy.insert(tid.clone());
                        }
                    }
                }
            }

            if grace_seconds > 0 {
                if let Some(updated_str) = updated_opt {
                    if let Some(ts) = parse_iso_timestamp(&updated_str) {
                        if (now - ts) < grace {
                            busy.insert(tid);
                        }
                    }
                }
            }
        }
    }

    {
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT thread_id FROM orchestration_v2_projection_runtime_requests \
                 WHERE resolved_at IS NULL OR status = 'pending'",
            )
            .ok()?;
        let rows = stmt
            .query_map([], |row| row.get::<_, Option<String>>(0))
            .ok()?;
        for item in rows {
            let tid = item.ok()?;
            if let Some(id) = tid.filter(|s| !s.is_empty()) {
                busy.insert(id);
            }
        }
    }

    if grace_seconds > 0 {
        let mut stmt = conn
            .prepare(
                "SELECT thread_id, MAX(completed_at) \
                 FROM orchestration_v2_projection_runs \
                 WHERE completed_at IS NOT NULL GROUP BY thread_id",
            )
            .ok()?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                ))
            })
            .ok()?;
        for item in rows {
            let (tid_opt, completed_opt) = item.ok()?;
            let Some(tid) = tid_opt.filter(|s| !s.is_empty()) else {
                continue;
            };
            if let Some(completed_str) = completed_opt {
                if let Some(ts) = parse_iso_timestamp(&completed_str) {
                    if (now - ts) < grace {
                        busy.insert(tid);
                    }
                }
            }
        }
    }

    Some(busy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::SecondsFormat;

    #[test]
    fn busy_threads_detects_running_pending_and_grace_window() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("statev2.sqlite");

        // Missing DB returns None (fail safe)
        assert!(get_busy_thread_ids(&db, 5).is_none());

        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE orchestration_v2_projection_runs (
                run_id TEXT, thread_id TEXT, provider_instance_id TEXT, status TEXT, completed_at TEXT
             );
             CREATE TABLE orchestration_v2_projection_provider_threads (
                thread_id TEXT, provider_instance_id TEXT, driver TEXT, status TEXT, updated_at TEXT, payload_json TEXT
             );
             CREATE TABLE orchestration_v2_projection_runtime_requests (
                runtime_request_id TEXT, thread_id TEXT, status TEXT, resolved_at TEXT
             );",
        )
        .unwrap();

        let old_ts = "2020-01-01T00:00:00Z";
        let recent_ts = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);

        // t-running: active run
        conn.execute(
            "INSERT INTO orchestration_v2_projection_runs VALUES ('r1', 't-running', 'antigravity', 'running', NULL)",
            [],
        )
        .unwrap();
        // t-bg: idle status but non-empty pendingBackgroundTasks
        conn.execute(
            "INSERT INTO orchestration_v2_projection_provider_threads VALUES ('t-bg', 'antigravity', 'antigravity', 'idle', ?, '{\"pendingBackgroundTasks\":[\"job1\"]}')",
            [old_ts],
        )
        .unwrap();
        // t-req: pending approval/question
        conn.execute(
            "INSERT INTO orchestration_v2_projection_runtime_requests VALUES ('req1', 't-req', 'pending', NULL)",
            [],
        )
        .unwrap();
        // t-grace: completed right now within grace window
        conn.execute(
            "INSERT INTO orchestration_v2_projection_runs VALUES ('r2', 't-grace', 'antigravity', 'completed', ?)",
            [&recent_ts],
        )
        .unwrap();
        // t-idle: completed long ago, idle, no pending tasks
        conn.execute(
            "INSERT INTO orchestration_v2_projection_runs VALUES ('r3', 't-idle', 'antigravity', 'completed', ?)",
            [old_ts],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO orchestration_v2_projection_provider_threads VALUES ('t-idle', 'antigravity', 'antigravity', 'idle', ?, '{\"pendingBackgroundTasks\":[]}')",
            [old_ts],
        )
        .unwrap();

        let busy = get_busy_thread_ids(&db, 10).unwrap();
        assert!(busy.contains("t-running"));
        assert!(busy.contains("t-bg"));
        assert!(busy.contains("t-req"));
        assert!(busy.contains("t-grace"));
        assert!(!busy.contains("t-idle"));
    }
}

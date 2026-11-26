use crate::paths::AppPaths;
use crate::t3_state::parse_iso_timestamp;
use crate::token::read_token_info;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct AccountRecord {
    pub id: String,
    pub email: String,
    pub tier: String,
    pub disabled: bool,
    pub banned: bool,
    pub validation_blocked: bool,
    pub last_used: i64,
    pub has_refresh_token: bool,
    pub group_pcts: HashMap<String, Option<i32>>,
    pub monitored_pct: Option<i32>,
    pub secondary_pct: Option<f64>,
    pub eligible: bool,
    pub order_index: usize,
}

#[derive(Clone, Debug)]
pub struct QuotaBucket {
    pub remaining_fraction: Option<f64>,
    pub reset_time: Option<String>,
}

pub fn get_server_auth_token(paths: &AppPaths) -> Option<String> {
    let raw = fs::read_to_string(&paths.gui_config_path).ok()?;
    let data: Value = serde_json::from_str(&raw).ok()?;
    let proxy = data.get("proxy")?.as_object()?;
    if let Some(pwd) = proxy.get("admin_password").and_then(|v| v.as_str()) {
        if !pwd.is_empty() {
            return Some(pwd.to_string());
        }
    }
    if let Some(key) = proxy.get("api_key").and_then(|v| v.as_str()) {
        if !key.is_empty() {
            return Some(key.to_string());
        }
    }
    None
}

fn url_encode_component(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for b in input.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(*b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn api_request(method: &str, url: &str, token: Option<&str>, timeout_secs: u64) -> u16 {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(3))
        .timeout_read(Duration::from_secs(timeout_secs))
        .timeout_write(Duration::from_secs(5))
        .build();

    let mut req = agent.request(method, url).set("Accept", "application/json");
    if let Some(tok) = token {
        req = req.set("Authorization", &format!("Bearer {tok}"));
    }

    let resp = if method.eq_ignore_ascii_case("POST") {
        req.send_bytes(&[])
    } else {
        req.call()
    };

    match resp {
        Ok(r) => r.status(),
        Err(ureq::Error::Status(code, _)) => code,
        Err(_) => 0,
    }
}

pub fn check_server_health(server_url: &str) -> bool {
    let url = format!("{}/api/health", server_url.trim_end_matches('/'));
    let status = api_request("GET", &url, None, 3);
    (200..300).contains(&status)
}

pub fn refresh_account_quota(paths: &AppPaths, server_url: &str, account_id: &str) -> bool {
    if account_id.is_empty() {
        return false;
    }
    let Some(token) = get_server_auth_token(paths) else {
        return false;
    };
    let encoded = url_encode_component(account_id);
    let url = format!(
        "{}/api/accounts/{encoded}/quota",
        server_url.trim_end_matches('/')
    );
    let status = api_request("GET", &url, Some(&token), 15);
    (200..300).contains(&status)
}

pub fn refresh_all_quotas(paths: &AppPaths, server_url: &str) -> bool {
    let Some(token) = get_server_auth_token(paths) else {
        return false;
    };
    let url = format!("{}/api/accounts/refresh", server_url.trim_end_matches('/'));
    let status = api_request("POST", &url, Some(&token), 25);
    (200..300).contains(&status)
}

fn json_number_as_i32(val: Option<&Value>) -> Option<i32> {
    let v = val?;
    if v.is_boolean() {
        return None;
    }
    v.as_i64()
        .map(|n| n as i32)
        .or_else(|| v.as_f64().map(|f| f as i32))
}

fn json_number_as_f64(val: Option<&Value>) -> Option<f64> {
    let v = val?;
    if v.is_boolean() {
        return None;
    }
    v.as_f64()
}

pub fn compute_group_pct(models: &[Value], group_name: &str) -> Option<i32> {
    let needle = group_name.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return None;
    }
    let mut min_pct: Option<i32> = None;
    for model in models {
        let Some(obj) = model.as_object() else {
            continue;
        };
        let name = obj
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !name.contains(&needle) {
            continue;
        }
        if let Some(pct) = json_number_as_i32(obj.get("percentage")) {
            min_pct = Some(min_pct.map_or(pct, |cur| cur.min(pct)));
        }
    }
    min_pct
}

pub fn load_accounts(
    paths: &AppPaths,
    monitored_groups: &[String],
) -> (Vec<AccountRecord>, Option<AccountRecord>, String) {
    let mut index_entries: Vec<Value> = Vec::new();
    let mut current_agy_id = String::new();
    let mut current_account_id = String::new();

    if let Ok(raw) = fs::read_to_string(&paths.accounts_index_path) {
        if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&raw) {
            if let Some(Value::Array(list)) = map.get("accounts") {
                index_entries = list.iter().filter(|v| v.is_object()).cloned().collect();
            }
            if let Some(s) = map.get("current_agy_account_id").and_then(|v| v.as_str()) {
                current_agy_id = s.to_string();
            }
            if let Some(s) = map.get("current_account_id").and_then(|v| v.as_str()) {
                current_account_id = s.to_string();
            }
        }
    }

    let mut groups_to_compute = vec!["gemini".to_string(), "claude".to_string()];
    for g in monitored_groups {
        let gl = g.to_ascii_lowercase();
        if !groups_to_compute.contains(&gl) {
            groups_to_compute.push(gl);
        }
    }

    let mut records = Vec::new();
    let mut seen_ids = HashSet::new();

    let build_record = |acc_id: &str, fallback: &Value, order_idx: usize| -> Option<AccountRecord> {
        let detail_path = paths.accounts_dir.join(format!("{acc_id}.json"));
        let detail: Value = fs::read_to_string(&detail_path)
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .filter(|v| v.is_object())
            .unwrap_or_else(|| fallback.clone());

        let email = detail
            .get("email")
            .and_then(|v| v.as_str())
            .or_else(|| fallback.get("email").and_then(|v| v.as_str()))
            .unwrap_or("")
            .to_string();
        if acc_id.is_empty() || email.is_empty() {
            return None;
        }

        let quota = detail.get("quota").and_then(|v| v.as_object());
        let models: &[Value] = quota
            .and_then(|q| q.get("models"))
            .and_then(|v| v.as_array())
            .map(|v| v.as_slice())
            .unwrap_or(&[]);

        let tier = quota
            .and_then(|q| q.get("subscription_tier"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("-")
            .to_string();

        let mut group_pcts = HashMap::new();
        for g in &groups_to_compute {
            group_pcts.insert(g.clone(), compute_group_pct(models, g));
        }

        let monitored_vals: Vec<i32> = monitored_groups
            .iter()
            .filter_map(|g| group_pcts.get(&g.to_ascii_lowercase()).copied().flatten())
            .collect();
        let monitored_pct = monitored_vals.into_iter().min();

        let all_pcts: Vec<i32> = models
            .iter()
            .filter_map(|m| json_number_as_i32(m.as_object()?.get("percentage")))
            .collect();
        let secondary_pct = if all_pcts.is_empty() {
            None
        } else {
            let sum: i64 = all_pcts.iter().map(|&n| i64::from(n)).sum();
            Some(sum as f64 / all_pcts.len() as f64)
        };

        let has_refresh_token = detail
            .get("token")
            .and_then(|v| v.as_object())
            .and_then(|t| t.get("refresh_token"))
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.is_empty());

        let disabled = detail.get("disabled").and_then(|v| v.as_bool()).unwrap_or(false);
        let banned = detail.get("banned").and_then(|v| v.as_bool()).unwrap_or(false);
        let validation_blocked = detail
            .get("validation_blocked")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let eligible = !disabled && !banned && !validation_blocked && has_refresh_token;

        let last_used = detail
            .get("last_used")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);

        Some(AccountRecord {
            id: acc_id.to_string(),
            email,
            tier,
            disabled,
            banned,
            validation_blocked,
            last_used,
            has_refresh_token,
            group_pcts,
            monitored_pct,
            secondary_pct,
            eligible,
            order_index: order_idx,
        })
    };

    for entry in &index_entries {
        let acc_id = entry
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if !acc_id.is_empty() && !seen_ids.contains(acc_id) {
            if let Some(rec) = build_record(acc_id, entry, records.len()) {
                seen_ids.insert(acc_id.to_string());
                records.push(rec);
            }
        }
    }

    if let Ok(entries) = fs::read_dir(&paths.accounts_dir) {
        let mut json_files: Vec<_> = entries
            .filter_map(|e| e.ok().map(|x| x.path()))
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
            .collect();
        json_files.sort();

        for path in json_files {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                if !seen_ids.contains(stem) {
                    let fallback = serde_json::json!({ "id": stem });
                    if let Some(rec) = build_record(stem, &fallback, records.len()) {
                        seen_ids.insert(stem.to_string());
                        records.push(rec);
                    }
                }
            }
        }
    }

    let source_email = read_token_info(&paths.source_token)
        .map(|t| t.email)
        .unwrap_or_default();

    let mut active: Option<AccountRecord> = None;
    if !source_email.is_empty() {
        let lower_src = source_email.to_ascii_lowercase();
        active = records
            .iter()
            .find(|r| r.email.to_ascii_lowercase() == lower_src)
            .cloned();
    }
    if active.is_none() && !current_agy_id.is_empty() {
        active = records.iter().find(|r| r.id == current_agy_id).cloned();
    }
    if active.is_none() && !current_account_id.is_empty() {
        active = records.iter().find(|r| r.id == current_account_id).cloned();
    }

    (records, active, source_email)
}

pub fn load_account_quota_buckets(
    paths: &AppPaths,
    account_id: &str,
) -> (HashMap<String, QuotaBucket>, i64) {
    let detail_path = paths.accounts_dir.join(format!("{account_id}.json"));
    let Ok(raw) = fs::read_to_string(&detail_path) else {
        return (HashMap::new(), 0);
    };
    let Ok(Value::Object(data)) = serde_json::from_str::<Value>(&raw) else {
        return (HashMap::new(), 0);
    };
    let Some(quota) = data.get("quota").and_then(|v| v.as_object()) else {
        return (HashMap::new(), 0);
    };

    let last_updated = quota
        .get("last_updated")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    let mut buckets_map = HashMap::new();
    let Some(groups) = quota.get("quota_groups").and_then(|v| v.as_array()) else {
        return (buckets_map, last_updated);
    };

    for grp in groups {
        let Some(grp_obj) = grp.as_object() else {
            continue;
        };
        let group_name = grp_obj
            .get("display_name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let Some(buckets) = grp_obj.get("buckets").and_then(|v| v.as_array()) else {
            continue;
        };

        for bucket in buckets {
            let Some(b_obj) = bucket.as_object() else {
                continue;
            };
            let bid = b_obj
                .get("bucket_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            let window = b_obj
                .get("window")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_ascii_lowercase();

            let key = if matches!(
                bid.as_str(),
                "gemini-5h" | "gemini-weekly" | "3p-5h" | "3p-weekly"
            ) {
                bid
            } else if group_name.contains("gemini") && matches!(window.as_str(), "5h" | "weekly") {
                format!("gemini-{window}")
            } else if (group_name.contains("claude") || group_name.contains("gpt"))
                && matches!(window.as_str(), "5h" | "weekly")
            {
                format!("3p-{window}")
            } else {
                continue;
            };

            let remaining_fraction = json_number_as_f64(b_obj.get("remaining_fraction"));
            let reset_time = b_obj
                .get("reset_time")
                .and_then(|v| v.as_str())
                .map(ToString::to_string);

            buckets_map.insert(
                key,
                QuotaBucket {
                    remaining_fraction,
                    reset_time,
                },
            );
        }
    }

    (buckets_map, last_updated)
}

pub fn format_fraction_pct(frac: Option<f64>) -> String {
    let Some(f) = frac else {
        return "n/a".to_string();
    };
    let pct = f * 100.0;
    if pct > 0.0 && pct < 1.0 {
        format!("{pct:.1}%")
    } else {
        format!("{}%", pct.round() as i64)
    }
}

pub fn format_reset_countdown(iso_str: Option<&str>, now: f64) -> String {
    let Some(ts) = iso_str.and_then(parse_iso_timestamp) else {
        return "-".to_string();
    };
    let diff = ts - now;
    if diff <= 0.0 {
        return "now".to_string();
    }
    if diff < 3600.0 {
        let mins = ((diff / 60.0).floor() as i64).max(1);
        return format!("{mins}m");
    }
    if diff < 86400.0 {
        let hours = (diff / 3600.0).floor() as i64;
        let mins = ((diff % 3600.0) / 60.0).floor() as i64;
        return format!("{hours}h{mins:02}m");
    }
    let days = (diff / 86400.0).floor() as i64;
    let hours = ((diff % 86400.0) / 3600.0).floor() as i64;
    format!("{days}d{hours}h")
}

pub fn format_bucket_cell(bucket: Option<&QuotaBucket>, now: f64) -> String {
    let Some(b) = bucket else {
        return "n/a".to_string();
    };
    let Some(frac) = b.remaining_fraction else {
        return "n/a".to_string();
    };
    let pct_str = format_fraction_pct(Some(frac));
    let reset_str = format_reset_countdown(b.reset_time.as_deref(), now);
    format!("{pct_str:>5} ({reset_str})")
}

pub fn format_account_status(acc: &AccountRecord) -> &'static str {
    if acc.banned {
        "banned"
    } else if acc.disabled {
        "disabled"
    } else if acc.validation_blocked {
        "verify"
    } else if !acc.has_refresh_token {
        "no-token"
    } else {
        "ready"
    }
}

pub fn select_candidate<'a>(
    accounts: &'a [AccountRecord],
    current: Option<&AccountRecord>,
    threshold: i32,
    reason: &str,
) -> Option<&'a AccountRecord> {
    let others: Vec<&AccountRecord> = accounts
        .iter()
        .filter(|a| a.eligible && current.is_none_or(|c| a.id != c.id))
        .collect();
    if others.is_empty() {
        return None;
    }

    if matches!(reason, "manual-next" | "schedule") {
        let ring_start = current
            .map(|c| (c.order_index + 1) % accounts.len().max(1))
            .unwrap_or(0);
        let ring_others: Vec<&AccountRecord> = accounts[ring_start..]
            .iter()
            .chain(accounts[..ring_start].iter())
            .filter(|a| a.eligible && current.is_none_or(|c| a.id != c.id))
            .collect();

        if let Some(found) = ring_others
            .iter()
            .copied()
            .find(|a| a.monitored_pct.is_some_and(|p| p > threshold))
        {
            return Some(found);
        }
        if let Some(found) = ring_others
            .iter()
            .copied()
            .find(|a| a.monitored_pct.is_some_and(|p| p > 0))
        {
            return Some(found);
        }
        return ring_others.into_iter().max_by(|a, b| {
            let ap = a.monitored_pct.unwrap_or(-1);
            let bp = b.monitored_pct.unwrap_or(-1);
            ap.cmp(&bp).then_with(|| {
                let as2 = a.secondary_pct.unwrap_or(-1.0);
                let bs2 = b.secondary_pct.unwrap_or(-1.0);
                as2.total_cmp(&bs2)
            })
        });
    }

    let best_cmp = |a: &&AccountRecord, b: &&AccountRecord| {
        let ap = a.monitored_pct.unwrap_or(-1);
        let bp = b.monitored_pct.unwrap_or(-1);
        bp.cmp(&ap)
            .then_with(|| {
                let as2 = a.secondary_pct.unwrap_or(-1.0);
                let bs2 = b.secondary_pct.unwrap_or(-1.0);
                bs2.total_cmp(&as2)
            })
            .then_with(|| a.last_used.cmp(&b.last_used))
    };

    let mut above: Vec<&AccountRecord> = others
        .iter()
        .copied()
        .filter(|a| a.monitored_pct.is_some_and(|p| p > threshold))
        .collect();
    if !above.is_empty() {
        above.sort_by(best_cmp);
        return above.into_iter().next();
    }

    let cur_pct = current
        .filter(|c| c.eligible)
        .and_then(|c| c.monitored_pct)
        .unwrap_or(-1);

    let mut better: Vec<&AccountRecord> = others
        .iter()
        .copied()
        .filter(|a| a.monitored_pct.unwrap_or(-1) > cur_pct)
        .collect();
    if !better.is_empty() {
        better.sort_by(best_cmp);
        return better.into_iter().next();
    }

    if current.is_none_or(|c| !c.eligible) {
        let mut sorted = others;
        sorted.sort_by(best_cmp);
        return sorted.into_iter().next();
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_account(id: &str, pct: i32, eligible: bool, order: usize) -> AccountRecord {
        AccountRecord {
            id: id.into(),
            email: format!("{id}@example.com"),
            tier: "PRO".into(),
            disabled: !eligible,
            banned: false,
            validation_blocked: false,
            last_used: order as i64,
            has_refresh_token: true,
            group_pcts: HashMap::new(),
            monitored_pct: Some(pct),
            secondary_pct: Some(f64::from(pct)),
            eligible,
            order_index: order,
        }
    }

    #[test]
    fn candidate_selection_picks_highest_or_round_robin() {
        let accounts = vec![
            mk_account("a", 5, true, 0),
            mk_account("b", 95, false, 1), // ineligible
            mk_account("c", 45, true, 2),
            mk_account("d", 80, true, 3),
        ];
        let cur = &accounts[0];

        // Quota mode picks highest eligible > threshold ("d" at 80%)
        assert_eq!(select_candidate(&accounts, Some(cur), 10, "quota").unwrap().id, "d");
        // Schedule / round-robin mode skips ineligible "b" and picks next healthy "c"
        assert_eq!(
            select_candidate(&accounts, Some(cur), 10, "schedule")
                .unwrap()
                .id,
            "c"
        );
    }

    #[test]
    fn bucket_formatting_handles_small_fractions_and_countdowns() {
        let b = QuotaBucket {
            remaining_fraction: Some(0.002),
            reset_time: Some("2026-11-26T05:00:00Z".into()),
        };
        let now = parse_iso_timestamp("2026-11-26T02:30:00Z").unwrap();
        assert_eq!(format_bucket_cell(Some(&b), now), " 0.2% (2h30m)");
        assert_eq!(format_bucket_cell(None, now), "n/a");
    }
}

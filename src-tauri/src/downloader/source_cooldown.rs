//! 下载源冷却表。
//!
//! 这里只记录确实表现出限流、慢速或卡死的源。冷却 key 使用 URL 的
//! scheme + host + port，避免把不同 CDN 或镜像源混在一起。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct SourceCooldownSnapshot {
    pub key: String,
    pub remaining: Duration,
    pub reason: String,
}

#[derive(Clone, Debug)]
struct SourceCooldownEntry {
    until: Instant,
    reason: String,
}

#[derive(Debug, Default)]
pub struct SourceCooldowns {
    entries: Mutex<HashMap<String, SourceCooldownEntry>>,
    // 入口地址可能会重定向到实际 CDN，这里只把入口映射到真实冷却源。
    aliases: Mutex<HashMap<String, String>>,
}

/// 正式运行时共享同一份源冷却状态，避免不同下载阶段反复撞击已限流的源。
pub fn process_source_cooldowns() -> Arc<SourceCooldowns> {
    static COOLDOWNS: OnceLock<Arc<SourceCooldowns>> = OnceLock::new();
    COOLDOWNS
        .get_or_init(|| Arc::new(SourceCooldowns::default()))
        .clone()
}

impl SourceCooldowns {
    pub fn remaining_for_url(&self, url: &str) -> Option<SourceCooldownSnapshot> {
        let key = source_key(url);
        let alias_target = self
            .aliases
            .lock()
            .ok()
            .and_then(|aliases| aliases.get(&key).cloned());
        let lookup_key = alias_target.clone().unwrap_or_else(|| key.clone());
        let mut entries = self.entries.lock().ok()?;
        let entry = match entries.get(&lookup_key) {
            Some(entry) => entry,
            None => {
                if alias_target.is_some() {
                    self.remove_alias(&key);
                }
                return None;
            }
        };
        let now = Instant::now();
        if entry.until <= now {
            entries.remove(&lookup_key);
            if alias_target.is_some() {
                self.remove_alias(&key);
            }
            return None;
        }

        Some(SourceCooldownSnapshot {
            key: lookup_key,
            remaining: entry.until.saturating_duration_since(now),
            reason: entry.reason.clone(),
        })
    }

    pub fn mark_url(
        &self,
        url: &str,
        reason: impl Into<String>,
        duration: Duration,
    ) -> SourceCooldownSnapshot {
        let key = source_key(url);
        let reason = reason.into();
        let until = Instant::now() + duration;
        if let Ok(mut entries) = self.entries.lock() {
            entries
                .entry(key.clone())
                .and_modify(|entry| {
                    if entry.until < until {
                        entry.until = until;
                    }
                    entry.reason = reason.clone();
                })
                .or_insert_with(|| SourceCooldownEntry {
                    until,
                    reason: reason.clone(),
                });
        }

        SourceCooldownSnapshot {
            key,
            remaining: duration,
            reason,
        }
    }

    pub fn mark_alias(&self, alias_url: &str, target_url: &str) {
        let alias_key = source_key(alias_url);
        let target_key = source_key(target_url);
        if alias_key == target_key {
            return;
        }
        if let Ok(mut aliases) = self.aliases.lock() {
            aliases.insert(alias_key, target_key);
        }
    }

    fn remove_alias(&self, alias_key: &str) {
        if let Ok(mut aliases) = self.aliases.lock() {
            aliases.remove(alias_key);
        }
    }
}

pub fn source_key(url: &str) -> String {
    match url::Url::parse(url.trim()) {
        Ok(parsed) => {
            let scheme = parsed.scheme();
            let host = parsed.host_str().unwrap_or("").to_ascii_lowercase();
            match parsed.port() {
                Some(port) => format!("{scheme}://{host}:{port}"),
                None => format!("{scheme}://{host}"),
            }
        }
        Err(_) => url.trim().to_ascii_lowercase(),
    }
}

pub fn is_source_pressure_error(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    error.contains("下载过慢")
        || error.contains("下载卡住")
        || error.contains("请求超时")
        || error.contains("超时")
        || lower.contains("too slow")
        || lower.contains("stalled")
        || lower.contains("no progress")
        || lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("deadline")
        || lower.contains("connection reset")
        || lower.contains("connection closed")
        || has_http_status(&lower, 408)
        || has_http_status(&lower, 429)
        || has_http_status(&lower, 503)
        || has_http_status(&lower, 522)
}

fn has_http_status(error: &str, expected: u16) -> bool {
    for marker in ["http status ", "http "] {
        let mut remaining = error;
        while let Some(offset) = remaining.find(marker) {
            let value = &remaining[offset + marker.len()..];
            let digits = value
                .chars()
                .take_while(|character| character.is_ascii_digit())
                .collect::<String>();
            if digits.parse::<u16>().ok() == Some(expected) {
                return true;
            }
            remaining = value.get(digits.len()..).unwrap_or_default();
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_cooldowns_share_one_instance() {
        let first = process_source_cooldowns();
        let second = process_source_cooldowns();

        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn pressure_status_requires_http_status_context() {
        assert!(is_source_pressure_error(
            "http status 429 Too Many Requests"
        ));
        assert!(is_source_pressure_error(
            "mirror HTTP 429 Too Many Requests"
        ));
        assert!(!is_source_pressure_error(
            "sha1 mismatch: expected abc429def, got 001122"
        ));
        assert!(!is_source_pressure_error(
            "https://example.invalid/files/429/archive.jar"
        ));
    }
}

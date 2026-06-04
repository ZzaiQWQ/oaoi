//! 下载源冷却表。
//!
//! 这里只记录确实表现出限流、慢速或卡死的源。冷却 key 使用 URL 的
//! scheme + host + port，避免把不同 CDN 或镜像源混在一起。

use std::collections::HashMap;
use std::sync::Mutex;
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
    error.contains("429")
        || error.contains("下载过慢")
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
        || lower.contains("http status 408")
        || lower.contains("http status 429")
        || lower.contains("http status 503")
        || lower.contains("http status 522")
}

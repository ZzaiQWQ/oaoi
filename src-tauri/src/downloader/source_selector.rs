//! 候选源启动前规划。
//!
//! 正式下载前不主动请求所有源，避免导入大量文件时被预探测拖慢。这里只根据冷却表
//! 把明确冷却中的源后移，其他源保持业务层给出的原始顺序。
use crate::downloader::options::DownloadEngineOptions;
use crate::downloader::request::{DownloadCandidate, DownloadRequest};
use crate::downloader::source_cooldown::SourceCooldowns;
use reqwest::Method;

pub fn plan_candidates(
    request: &DownloadRequest,
    candidates: Vec<DownloadCandidate>,
    cooldowns: &SourceCooldowns,
    options: &DownloadEngineOptions,
) -> Vec<DownloadCandidate> {
    if !options.source_cooldown_reorder_enabled
        || request.method != Method::GET
        || candidates.len() < 2
    {
        return candidates;
    }

    let mut normal = Vec::new();
    let mut cooling = Vec::new();
    for candidate in candidates {
        if cooldowns.remaining_for_url(&candidate.url).is_some() {
            cooling.push(candidate);
        } else {
            normal.push(candidate);
        }
    }

    normal.extend(cooling);
    normal
}

//! HTTP 探测和请求构造。
//!
//! Range 探测用于判断地址能不能走分片下载。拒绝 Range 的服务器仍然可以
//! 降级为普通流式下载。

use crate::downloader::async_http;
use crate::downloader::request::DownloadRequest;
use reqwest::header::{HeaderMap, HeaderName, CONTENT_LENGTH, CONTENT_RANGE, ETAG, LAST_MODIFIED};
use reqwest::Response;
use reqwest::{Method, StatusCode};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct RangeProbe {
    pub source_url: String,
    pub url: String,
    pub total: Option<u64>,
    pub accepts_range: bool,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContentRange {
    pub start: Option<u64>,
    pub end: Option<u64>,
    pub total: Option<u64>,
}

pub async fn probe_candidate(
    http: &reqwest::Client,
    request: &DownloadRequest,
    url: &str,
    cancel: &AtomicBool,
    probe_timeout: Duration,
    probe_bytes: u64,
) -> Result<RangeProbe, String> {
    if request.method != Method::GET {
        let response = async_http::send_request(
            http,
            request,
            url,
            None,
            probe_timeout,
            cancel,
            None,
            "probe",
        )
        .await?;
        let final_url = response.url().to_string();
        let total = response.content_length();
        return Ok(RangeProbe {
            source_url: url.to_string(),
            url: final_url,
            total,
            accepts_range: false,
            etag: header_string(response.headers(), &ETAG),
            last_modified: header_string(response.headers(), &LAST_MODIFIED),
        });
    }

    let probe_end = probe_bytes.max(1).saturating_sub(1);
    for range in [(0_u64, probe_end), (1_u64, 1_u64)] {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".to_string());
        }
        let response = async_http::send_request(
            http,
            request,
            url,
            Some(range),
            probe_timeout,
            cancel,
            None,
            "range probe",
        )
        .await?;
        let probe = parse_probe_response(url, response)?;
        if probe.accepts_range || probe.total.is_some() {
            return Ok(probe);
        }
    }

    Err("range probe did not return usable metadata".to_string())
}

pub fn resolve_total(
    probed_total: Option<u64>,
    expected_size: Option<u64>,
) -> Result<Option<u64>, String> {
    match (probed_total, expected_size) {
        (Some(probed), Some(expected)) if probed != expected => Err(format!(
            "size mismatch before download: expected {expected}, source reports {probed}"
        )),
        (Some(probed), _) => Ok(Some(probed)),
        (None, Some(expected)) => Ok(Some(expected)),
        (None, None) => Ok(None),
    }
}

fn parse_probe_response(source_url: &str, response: Response) -> Result<RangeProbe, String> {
    let status = response.status();
    let final_url = response.url().to_string();
    let headers = response.headers().clone();
    let etag = header_string(&headers, &ETAG);
    let last_modified = header_string(&headers, &LAST_MODIFIED);

    if status == StatusCode::PARTIAL_CONTENT {
        let total = headers
            .get(CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_content_range_total);
        return Ok(RangeProbe {
            source_url: source_url.to_string(),
            url: final_url,
            total,
            accepts_range: total.is_some(),
            etag,
            last_modified,
        });
    }

    if status == StatusCode::RANGE_NOT_SATISFIABLE {
        let total = headers
            .get(CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_content_range_total);
        return Ok(RangeProbe {
            source_url: source_url.to_string(),
            url: final_url,
            total,
            accepts_range: total.is_some(),
            etag,
            last_modified,
        });
    }

    if status.is_success() {
        let total = headers
            .get(CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        return Ok(RangeProbe {
            source_url: source_url.to_string(),
            url: final_url,
            total,
            accepts_range: false,
            etag,
            last_modified,
        });
    }

    Err(format!("http status {status}"))
}

fn header_string(headers: &HeaderMap, name: &HeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_string())
}

pub fn parse_content_range(value: &str) -> Option<ContentRange> {
    let value = value.trim();
    let (unit, rest) = value.split_once(' ')?;
    if !unit.eq_ignore_ascii_case("bytes") {
        return None;
    }

    let (range, total) = rest.rsplit_once('/')?;
    let total = parse_content_range_total_value(total.trim())?;
    let range = range.trim();
    if range == "*" {
        return Some(ContentRange {
            start: None,
            end: None,
            total,
        });
    }

    let (start, end) = range.split_once('-')?;
    let start = start.trim().parse::<u64>().ok()?;
    let end = end.trim().parse::<u64>().ok()?;
    if start > end {
        return None;
    }

    Some(ContentRange {
        start: Some(start),
        end: Some(end),
        total,
    })
}

fn parse_content_range_total(value: &str) -> Option<u64> {
    parse_content_range(value)?.total
}

fn parse_content_range_total_value(value: &str) -> Option<Option<u64>> {
    if value == "*" {
        Some(None)
    } else {
        value.parse::<u64>().ok().map(Some)
    }
}

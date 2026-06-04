//! 带空闲超时的异步 HTTP 读写辅助。
//!
//! blocking reqwest 只能设置连接超时，没有可靠的单次 read 空闲超时。这里用异步 chunk
//! 读取包一层 timeout，避免连接卡住后下载线程一直等。
use crate::downloader::request::DownloadRequest;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT_ENCODING, RANGE};
use reqwest::{Client, Method, RequestBuilder, Response};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub fn build_client(connect_timeout: Duration) -> Result<Client, String> {
    Client::builder()
        .connect_timeout(connect_timeout)
        .build()
        .map_err(|e| format!("build async download http client failed: {}", e))
}

pub async fn send_request(
    http: &Client,
    request: &DownloadRequest,
    url: &str,
    range: Option<(u64, u64)>,
    idle_timeout: Duration,
    cancel: &AtomicBool,
    abort: Option<&AtomicBool>,
    label: &str,
) -> Result<Response, String> {
    if stop_requested(cancel, abort) {
        return Err("cancelled".to_string());
    }
    let future = build_request(http, request, url, range)?.send();
    let response = wait_with_idle_timeout(future, idle_timeout, cancel, abort, label).await?;
    response.map_err(|e| format!("{label} request failed: {}", e))
}

pub async fn read_chunk(
    response: &mut Response,
    idle_timeout: Duration,
    cancel: &AtomicBool,
    label: &str,
) -> Result<Option<Vec<u8>>, String> {
    read_chunk_with_abort(response, idle_timeout, cancel, None, label).await
}

pub async fn read_chunk_with_abort(
    response: &mut Response,
    idle_timeout: Duration,
    cancel: &AtomicBool,
    abort: Option<&AtomicBool>,
    label: &str,
) -> Result<Option<Vec<u8>>, String> {
    if stop_requested(cancel, abort) {
        return Err("cancelled".to_string());
    }
    let chunk = wait_with_idle_timeout(response.chunk(), idle_timeout, cancel, abort, label)
        .await?
        .map_err(|e| format!("{label} read failed: {}", e))?;
    Ok(chunk.map(|bytes| bytes.to_vec()))
}

fn build_request(
    http: &Client,
    request: &DownloadRequest,
    url: &str,
    range: Option<(u64, u64)>,
) -> Result<RequestBuilder, String> {
    let method = if range.is_some() {
        Method::GET
    } else {
        request.method.clone()
    };
    let mut builder = http.request(method, url);
    let headers = build_headers(&request.headers)?;
    if !headers.is_empty() {
        builder = builder.headers(headers);
    }
    if let Some((start, end)) = range {
        builder = builder
            .header(RANGE, format!("bytes={start}-{end}"))
            .header(ACCEPT_ENCODING, "identity");
    } else if request.method == Method::GET {
        builder = builder.header(ACCEPT_ENCODING, "identity");
    } else if let Some(body) = &request.body {
        builder = builder.body(body.clone());
    }
    Ok(builder)
}

fn build_headers(headers: &[(String, String)]) -> Result<HeaderMap, String> {
    let mut out = HeaderMap::new();
    for (name, value) in headers {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|e| format!("invalid header name {name}: {e}"))?;
        let value =
            HeaderValue::from_str(value).map_err(|e| format!("invalid header value: {e}"))?;
        out.insert(name, value);
    }
    Ok(out)
}

async fn wait_with_idle_timeout<F, T>(
    future: F,
    idle_timeout: Duration,
    cancel: &AtomicBool,
    abort: Option<&AtomicBool>,
    label: &str,
) -> Result<T, String>
where
    F: std::future::Future<Output = T>,
{
    tokio::pin!(future);
    let started = Instant::now();
    loop {
        if stop_requested(cancel, abort) {
            return Err("cancelled".to_string());
        }

        let tick = if idle_timeout.is_zero() {
            Duration::from_millis(250)
        } else {
            let remaining = idle_timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Err(format!(
                    "{label} timed out after {}s idle",
                    idle_timeout.as_secs()
                ));
            }
            remaining.min(Duration::from_millis(250))
        };

        tokio::select! {
            result = &mut future => return Ok(result),
            _ = tokio::time::sleep(tick) => {}
        }
    }
}

fn stop_requested(cancel: &AtomicBool, abort: Option<&AtomicBool>) -> bool {
    cancel.load(Ordering::Relaxed)
        || abort
            .map(|abort| abort.load(Ordering::Relaxed))
            .unwrap_or(false)
}

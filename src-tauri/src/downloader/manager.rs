//! 独立下载器的队列管理器。
//!
//! 多个文件可以同时下载，但所有工作线程共享同一个全局连接池。
//! 某个文件完成后，队列里的下一个文件会马上开始。

use crate::downloader::event::{DownloadEvent, DownloadOutcome, DownloadResult, EventHandler};
use crate::downloader::file_task::{FileTask, FileTaskAttempt};
use crate::downloader::options::DownloadEngineOptions;
use crate::downloader::pool::ConnectionPool;
use crate::downloader::request::DownloadRequest;
use crate::downloader::source_cooldown::SourceCooldowns;
use crate::downloader::throttle::GlobalThrottle;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, Weak};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct DownloadManager {
    http: reqwest::Client,
    runtime: Arc<tokio::runtime::Runtime>,
    options: DownloadEngineOptions,
    pool: Arc<ConnectionPool>,
    throttle: Arc<GlobalThrottle>,
    cooldowns: Arc<SourceCooldowns>,
    cancelled: Arc<AtomicBool>,
    cancel_tokens: Arc<Mutex<Vec<Weak<AtomicBool>>>>,
    active_destinations: Arc<Mutex<HashSet<String>>>,
}

#[derive(Debug)]
struct QueuedRequest {
    index: usize,
    request: DownloadRequest,
    // 冷却中的任务先留在队列里，到点后再被 worker 捞起。
    available_at: Instant,
    defer_count: usize,
    dest_guard: DestinationGuard,
}

#[derive(Debug)]
struct DestinationGuard {
    key: String,
    active: Arc<Mutex<HashSet<String>>>,
}

impl Drop for DestinationGuard {
    fn drop(&mut self) {
        if let Ok(mut active) = self.active.lock() {
            active.remove(&self.key);
        }
    }
}

impl DownloadManager {
    pub fn with_options(options: DownloadEngineOptions) -> Result<Self, String> {
        let http = crate::downloader::async_http::build_client(options.connect_timeout)?;
        let runtime = build_download_runtime()?;
        Ok(Self::new_with_runtime(http, runtime, options))
    }

    pub fn with_options_and_pool(
        options: DownloadEngineOptions,
        pool: Arc<ConnectionPool>,
    ) -> Result<Self, String> {
        let http = crate::downloader::async_http::build_client(options.connect_timeout)?;
        let runtime = build_download_runtime()?;
        Ok(Self::new_with_runtime_and_pool(
            http, runtime, options, pool,
        ))
    }

    pub fn new(http: reqwest::Client, options: DownloadEngineOptions) -> Result<Self, String> {
        let runtime = build_download_runtime()?;
        Ok(Self::new_with_runtime(http, runtime, options))
    }

    pub fn new_with_runtime(
        http: reqwest::Client,
        runtime: tokio::runtime::Runtime,
        options: DownloadEngineOptions,
    ) -> Self {
        let max_global_connections = options.max_global_connections.max(1);
        Self::new_with_runtime_and_pool(
            http,
            runtime,
            options,
            Arc::new(ConnectionPool::new(max_global_connections)),
        )
    }

    pub fn new_with_runtime_and_pool(
        http: reqwest::Client,
        runtime: tokio::runtime::Runtime,
        options: DownloadEngineOptions,
        pool: Arc<ConnectionPool>,
    ) -> Self {
        let throttle = Arc::new(GlobalThrottle::new(options.global_speed_limit));
        Self {
            http,
            runtime: Arc::new(runtime),
            pool,
            throttle,
            cooldowns: Arc::new(SourceCooldowns::default()),
            cancelled: Arc::new(AtomicBool::new(false)),
            options,
            cancel_tokens: Arc::new(Mutex::new(Vec::new())),
            active_destinations: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    pub fn cancel_all(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
        if let Ok(mut tokens) = self.cancel_tokens.lock() {
            tokens.retain(|token| {
                if let Some(token) = token.upgrade() {
                    token.store(true, Ordering::Relaxed);
                    true
                } else {
                    false
                }
            });
        }
        self.pool.wake_all();
    }

    pub fn set_global_speed_limit(&self, bytes_per_sec: Option<u64>) {
        self.throttle.set_limit(bytes_per_sec);
    }

    pub fn active_connections(&self) -> usize {
        self.pool.active()
    }

    pub fn max_global_connections(&self) -> usize {
        self.pool.limit()
    }

    pub fn download_one<F>(
        &self,
        request: DownloadRequest,
        on_event: F,
    ) -> Result<DownloadResult, String>
    where
        F: Fn(DownloadEvent) + Send + Sync + 'static,
    {
        let handler: EventHandler = Arc::new(on_event);
        let _dest_guard = match self.try_acquire_destination(&request.dest) {
            Ok(guard) => guard,
            Err(error) => {
                handler(DownloadEvent::FileFailed {
                    request_id: request.id.clone(),
                    dest: request.dest.clone(),
                    error: error.clone(),
                });
                return Err(error);
            }
        };
        let cancel = self.new_cancel_token();
        let mut request = request;
        let mut defer_count = 0_usize;
        loop {
            match self.run_file_attempt(
                request,
                0,
                1,
                handler.clone(),
                cancel.clone(),
                defer_count,
                defer_count == 0,
            ) {
                FileTaskAttempt::Finished(result) => return Ok(result),
                FileTaskAttempt::Failed { error, .. } => return Err(error),
                FileTaskAttempt::Deferred {
                    request: deferred,
                    wait,
                    reason,
                } => {
                    if defer_count >= self.options.candidate_retry_attempts {
                        let error = format!("download deferred too many times: {reason}");
                        handler(DownloadEvent::FileFailed {
                            request_id: deferred.id.clone(),
                            dest: deferred.dest.clone(),
                            error: error.clone(),
                        });
                        return Err(error);
                    }
                    defer_count += 1;
                    if let Err(error) = sleep_with_cancel(wait, &cancel) {
                        handler(DownloadEvent::FileFailed {
                            request_id: deferred.id.clone(),
                            dest: deferred.dest.clone(),
                            error: error.clone(),
                        });
                        return Err(error);
                    }
                    request = deferred;
                }
            }
        }
    }

    pub fn download_many<F>(
        &self,
        requests: Vec<DownloadRequest>,
        on_event: F,
    ) -> Vec<DownloadOutcome>
    where
        F: Fn(DownloadEvent) + Send + Sync + 'static,
    {
        let total = requests.len();
        if total == 0 {
            return Vec::new();
        }

        let handler: EventHandler = Arc::new(on_event);
        let cancel = self.new_cancel_token();
        let request_meta = requests
            .iter()
            .map(|request| (request.id.clone(), request.dest.clone()))
            .collect::<Vec<_>>();
        let mut outcomes: Vec<Option<DownloadOutcome>> = vec![None; total];
        let mut seen_destinations = HashMap::new();
        let mut queued = VecDeque::new();
        for (index, request) in requests.into_iter().enumerate() {
            handler(DownloadEvent::FileQueued {
                request_id: request.id.clone(),
                file_index: index,
                file_total: total,
                dest: request.dest.clone(),
            });

            let dest_key = destination_key(&request.dest);
            if let Some(first_index) = seen_destinations.get(&dest_key).copied() {
                // 同一目标文件不能并发写，否则 .part/.dl 会互相覆盖。
                let error = format!(
                    "duplicate destination path with request #{}: {}",
                    first_index + 1,
                    request.dest.display()
                );
                handler(DownloadEvent::FileFailed {
                    request_id: request.id.clone(),
                    dest: request.dest.clone(),
                    error: error.clone(),
                });
                outcomes[index] = Some(DownloadOutcome::Failed {
                    request_id: request.id,
                    dest: request.dest,
                    error,
                });
                continue;
            }

            let dest_guard = match self.try_acquire_destination(&request.dest) {
                Ok(guard) => guard,
                Err(error) => {
                    handler(DownloadEvent::FileFailed {
                        request_id: request.id.clone(),
                        dest: request.dest.clone(),
                        error: error.clone(),
                    });
                    outcomes[index] = Some(DownloadOutcome::Failed {
                        request_id: request.id,
                        dest: request.dest,
                        error,
                    });
                    continue;
                }
            };

            seen_destinations.insert(dest_key, index);
            queued.push_back(QueuedRequest {
                index,
                request,
                available_at: Instant::now(),
                defer_count: 0,
                dest_guard,
            });
        }

        if queued.is_empty() {
            return finalize_outcomes(
                outcomes,
                &request_meta,
                &handler,
                cancel.load(Ordering::Relaxed),
            );
        }

        let worker_count = self.options.max_active_files.max(1).min(queued.len());
        let queue = Arc::new(Mutex::new(queued));
        let (tx, rx) = mpsc::channel();

        for _ in 0..worker_count {
            let manager = self.clone();
            let queue = queue.clone();
            let tx = tx.clone();
            let handler = handler.clone();
            let cancel = cancel.clone();

            thread::spawn(move || loop {
                let Some(next) = next_ready_request(&queue, &cancel) else {
                    break;
                };
                let QueuedRequest {
                    index,
                    request,
                    defer_count,
                    dest_guard,
                    ..
                } = next;

                match manager.run_file_attempt(
                    request,
                    index,
                    total,
                    handler.clone(),
                    cancel.clone(),
                    defer_count,
                    defer_count == 0,
                ) {
                    FileTaskAttempt::Finished(result) => {
                        let _ = tx.send((index, DownloadOutcome::Finished(result)));
                    }
                    FileTaskAttempt::Failed {
                        request_id,
                        dest,
                        error,
                    } => {
                        let _ = tx.send((
                            index,
                            DownloadOutcome::Failed {
                                request_id,
                                dest,
                                error,
                            },
                        ));
                    }
                    FileTaskAttempt::Deferred {
                        request,
                        wait,
                        reason,
                    } => {
                        if defer_count >= manager.options.candidate_retry_attempts {
                            let error = format!("download deferred too many times: {reason}");
                            handler(DownloadEvent::FileFailed {
                                request_id: request.id.clone(),
                                dest: request.dest.clone(),
                                error: error.clone(),
                            });
                            let _ = tx.send((
                                index,
                                DownloadOutcome::Failed {
                                    request_id: request.id,
                                    dest: request.dest,
                                    error,
                                },
                            ));
                        } else {
                            let mut queue = queue.lock().unwrap();
                            queue.push_back(QueuedRequest {
                                index,
                                request,
                                available_at: Instant::now() + wait,
                                defer_count: defer_count + 1,
                                dest_guard,
                            });
                        }
                    }
                }
            });
        }
        drop(tx);

        for (index, outcome) in rx {
            outcomes[index] = Some(outcome);
        }

        finalize_outcomes(
            outcomes,
            &request_meta,
            &handler,
            cancel.load(Ordering::Relaxed),
        )
    }

    fn run_file(
        &self,
        request: DownloadRequest,
        file_index: usize,
        file_total: usize,
        handler: EventHandler,
    ) -> DownloadOutcome {
        let _dest_guard = match self.try_acquire_destination(&request.dest) {
            Ok(guard) => guard,
            Err(error) => {
                handler(DownloadEvent::FileFailed {
                    request_id: request.id.clone(),
                    dest: request.dest.clone(),
                    error: error.clone(),
                });
                return DownloadOutcome::Failed {
                    request_id: request.id,
                    dest: request.dest,
                    error,
                };
            }
        };
        let cancel = self.new_cancel_token();
        match self.run_file_attempt(
            request,
            file_index,
            file_total,
            handler.clone(),
            cancel,
            0,
            true,
        ) {
            FileTaskAttempt::Finished(result) => DownloadOutcome::Finished(result),
            FileTaskAttempt::Failed {
                request_id,
                dest,
                error,
            } => DownloadOutcome::Failed {
                request_id,
                dest,
                error,
            },
            FileTaskAttempt::Deferred {
                request, reason, ..
            } => {
                let error = format!("download deferred without scheduler: {reason}");
                handler(DownloadEvent::FileFailed {
                    request_id: request.id.clone(),
                    dest: request.dest.clone(),
                    error: error.clone(),
                });
                DownloadOutcome::Failed {
                    request_id: request.id,
                    dest: request.dest,
                    error,
                }
            }
        }
    }

    fn run_file_attempt(
        &self,
        request: DownloadRequest,
        file_index: usize,
        file_total: usize,
        handler: EventHandler,
        cancel: Arc<AtomicBool>,
        retry_attempt_offset: usize,
        emit_started: bool,
    ) -> FileTaskAttempt {
        let task = FileTask::new(
            self.http.clone(),
            self.runtime.clone(),
            self.options.clone(),
            self.pool.clone(),
            self.throttle.clone(),
            self.cooldowns.clone(),
            cancel,
            handler,
            file_index,
            file_total,
            retry_attempt_offset,
        );
        task.run_attempt(request, emit_started)
    }

    fn new_cancel_token(&self) -> Arc<AtomicBool> {
        let token = Arc::new(AtomicBool::new(self.cancelled.load(Ordering::Relaxed)));
        if let Ok(mut tokens) = self.cancel_tokens.lock() {
            tokens.retain(|token| token.upgrade().is_some());
            tokens.push(Arc::downgrade(&token));
        }
        token
    }

    fn try_acquire_destination(&self, dest: &Path) -> Result<DestinationGuard, String> {
        let key = destination_key(dest);
        let mut active = self
            .active_destinations
            .lock()
            .map_err(|_| "destination lock poisoned".to_string())?;
        if active.contains(&key) {
            return Err(format!(
                "destination already downloading: {}",
                dest.display()
            ));
        }
        active.insert(key.clone());
        Ok(DestinationGuard {
            key,
            active: self.active_destinations.clone(),
        })
    }
}

fn finalize_outcomes(
    outcomes: Vec<Option<DownloadOutcome>>,
    request_meta: &[(String, PathBuf)],
    handler: &EventHandler,
    cancelled: bool,
) -> Vec<DownloadOutcome> {
    outcomes
        .into_iter()
        .enumerate()
        .map(|(index, outcome)| {
            outcome.unwrap_or_else(|| {
                let (request_id, dest) = request_meta
                    .get(index)
                    .cloned()
                    .unwrap_or_else(|| (String::new(), Default::default()));
                let error = if cancelled {
                    "download cancelled".to_string()
                } else {
                    "download worker ended without result".to_string()
                };
                handler(DownloadEvent::FileFailed {
                    request_id: request_id.clone(),
                    dest: dest.clone(),
                    error: error.clone(),
                });
                DownloadOutcome::Failed {
                    request_id,
                    dest,
                    error,
                }
            })
        })
        .collect()
}

fn destination_key(dest: &Path) -> String {
    let absolute = if dest.is_absolute() {
        dest.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|dir| dir.join(dest))
            .unwrap_or_else(|_| dest.to_path_buf())
    };
    let normalized = normalize_path_lexically(&absolute);
    let key = normalized.to_string_lossy().replace('/', "\\");
    #[cfg(windows)]
    {
        key.to_ascii_lowercase()
    }
    #[cfg(not(windows))]
    {
        key
    }
}

fn normalize_path_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push(component.as_os_str());
                }
            }
            Component::Normal(part) => out.push(part),
            Component::Prefix(_) | Component::RootDir => out.push(component.as_os_str()),
        }
    }
    out
}

fn build_download_runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .thread_name("oaoi-downloader-io")
        .build()
        .map_err(|e| format!("build download runtime failed: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::downloader::request::DownloadCandidate;
    use std::fs;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use tiny_http::{Header, Response, Server, StatusCode};

    #[test]
    fn destination_key_normalizes_dot_segments() {
        let base = std::env::current_dir().unwrap().join("target");
        let direct = base.join("oaoi-download-test.bin");
        let dotted = base
            .join(".")
            .join("nested")
            .join("..")
            .join("oaoi-download-test.bin");

        assert_eq!(destination_key(&direct), destination_key(&dotted));
    }

    #[test]
    fn cancel_all_cancels_future_tokens() {
        let manager = DownloadManager::with_options(DownloadEngineOptions::default()).unwrap();
        let active = manager.new_cancel_token();

        manager.cancel_all();

        assert!(active.load(Ordering::Relaxed));
        assert!(manager.new_cancel_token().load(Ordering::Relaxed));
    }

    #[test]
    fn destination_guard_blocks_parallel_same_destination() {
        let manager = DownloadManager::with_options(DownloadEngineOptions::default()).unwrap();
        let dest = std::env::current_dir()
            .unwrap()
            .join("target")
            .join("oaoi-active-destination-test.bin");
        let guard = manager.try_acquire_destination(&dest).unwrap();

        assert!(manager.try_acquire_destination(&dest).is_err());
        drop(guard);
        assert!(manager.try_acquire_destination(&dest).is_ok());
    }

    #[test]
    fn download_one_uses_ranged_local_http() {
        let body = b"abcdefghijklmnopqrstuvwxyz".to_vec();
        let (url, handle) = spawn_ranged_server(body.clone(), 2);
        let dest = unique_test_dest("ranged");
        let mut options = DownloadEngineOptions::default();
        options.max_global_connections = 1;
        options.max_connections_per_file = 1;
        let manager = DownloadManager::with_options(options).unwrap();
        let request = DownloadRequest::new(
            "ranged-http-test",
            vec![DownloadCandidate::new(url)],
            dest.clone(),
        )
        .with_expected_size(body.len() as u64);

        let result = manager.download_one(request, |_| {}).unwrap();

        assert!(result.ranged);
        assert_eq!(fs::read(&dest).unwrap(), body);
        let _ = fs::remove_file(dest);
        handle.join().unwrap();
    }

    #[test]
    fn download_one_rejects_wrong_content_range_local_http() {
        let body = b"wrong content range should not be accepted".to_vec();
        let (url, handle) = spawn_wrong_content_range_server(body.clone());
        let dest = unique_test_dest("wrong-content-range");
        let part = crate::downloader::fs::partial_path(&dest);
        let mut options = DownloadEngineOptions::default();
        options.max_global_connections = 1;
        options.max_connections_per_file = 1;
        options.segment_retries = 0;
        options.candidate_retry_attempts = 0;
        let manager = DownloadManager::with_options(options).unwrap();
        let request = DownloadRequest::new(
            "wrong-content-range-test",
            vec![DownloadCandidate::new(url)],
            dest.clone(),
        )
        .with_expected_size(body.len() as u64);

        let error = manager.download_one(request, |_| {}).unwrap_err();

        assert!(error.contains("Content-Range start mismatch"));
        assert!(!dest.exists());
        let _ = fs::remove_file(part);
        handle.join().unwrap();
    }

    #[test]
    fn download_one_rejects_short_content_range_with_long_body_local_http() {
        let body = b"short content range must not accept a longer body".to_vec();
        let (url, handle) = spawn_short_content_range_server(body.clone());
        let dest = unique_test_dest("short-content-range");
        let part = crate::downloader::fs::partial_path(&dest);
        let mut options = DownloadEngineOptions::default();
        options.max_global_connections = 1;
        options.max_connections_per_file = 1;
        options.segment_retries = 0;
        options.candidate_retry_attempts = 0;
        let manager = DownloadManager::with_options(options).unwrap();
        let request = DownloadRequest::new(
            "short-content-range-test",
            vec![DownloadCandidate::new(url)],
            dest.clone(),
        )
        .with_expected_size(body.len() as u64);

        let error = manager.download_one(request, |_| {}).unwrap_err();

        assert!(error.contains("Content-Range end mismatch"));
        assert!(!dest.exists());
        let _ = fs::remove_file(part);
        handle.join().unwrap();
    }

    #[test]
    fn download_one_keeps_real_segment_error_when_other_segments_cancel() {
        let body = (0..128).collect::<Vec<u8>>();
        let (url, handle) = spawn_abort_preserves_error_server(body.clone());
        let dest = unique_test_dest("multi-segment-error");
        let part = crate::downloader::fs::partial_path(&dest);
        let resume = crate::downloader::fs::resume_path(&dest);
        let mut options = DownloadEngineOptions::default();
        options.max_global_connections = 2;
        options.max_connections_per_file = 2;
        options.min_split_size = 1;
        options.slow_start_interval = Duration::ZERO;
        options.manager_tick = Duration::from_millis(5);
        options.segment_retries = 0;
        options.candidate_retry_attempts = 0;
        let manager = DownloadManager::with_options(options).unwrap();
        let request = DownloadRequest::new(
            "multi-segment-error-test",
            vec![DownloadCandidate::new(url)],
            dest.clone(),
        )
        .with_expected_size(body.len() as u64);

        let error = manager.download_one(request, |_| {}).unwrap_err();

        assert!(error.contains("Content-Range start mismatch"));
        assert!(!error.contains("cancelled"));
        assert!(!dest.exists());
        let _ = fs::remove_file(part);
        let _ = fs::remove_file(resume);
        handle.join().unwrap();
    }

    #[test]
    fn download_one_falls_back_to_streaming_local_http() {
        let body = b"streaming fallback body".to_vec();
        let (url, handle) = spawn_streaming_server(body.clone(), 2);
        let dest = unique_test_dest("streaming");
        let mut options = DownloadEngineOptions::default();
        options.max_global_connections = 1;
        options.max_connections_per_file = 1;
        let manager = DownloadManager::with_options(options).unwrap();
        let request = DownloadRequest::new(
            "streaming-http-test",
            vec![DownloadCandidate::new(url)],
            dest.clone(),
        )
        .with_expected_size(body.len() as u64);

        let result = manager.download_one(request, |_| {}).unwrap();

        assert!(!result.ranged);
        assert_eq!(fs::read(&dest).unwrap(), body);
        let _ = fs::remove_file(dest);
        handle.join().unwrap();
    }

    #[test]
    fn download_streaming_removes_stale_resume_file() {
        let body = b"streaming clears stale resume".to_vec();
        let (url, handle) = spawn_streaming_server(body.clone(), 2);
        let dest = unique_test_dest("streaming-stale-resume");
        let resume = crate::downloader::fs::resume_path(&dest);
        fs::write(&resume, b"stale resume").unwrap();
        let mut options = DownloadEngineOptions::default();
        options.max_global_connections = 1;
        options.max_connections_per_file = 1;
        let manager = DownloadManager::with_options(options).unwrap();
        let request = DownloadRequest::new(
            "streaming-stale-resume-test",
            vec![DownloadCandidate::new(url)],
            dest.clone(),
        )
        .with_expected_size(body.len() as u64)
        .without_resume();

        let result = manager.download_one(request, |_| {}).unwrap();

        assert!(!result.ranged);
        assert_eq!(fs::read(&dest).unwrap(), body);
        assert!(!resume.exists());
        let _ = fs::remove_file(dest);
        handle.join().unwrap();
    }

    #[test]
    fn download_one_restarts_when_resume_cannot_continue() {
        let body = b"restart when resume is unavailable".to_vec();
        let (url, handle) = spawn_streaming_server(body.clone(), 3);
        let dest = unique_test_dest("restart-no-resume");
        let part = crate::downloader::fs::partial_path(&dest);
        fs::write(&part, b"old partial bytes").unwrap();
        let mut options = DownloadEngineOptions::default();
        options.max_global_connections = 1;
        options.max_connections_per_file = 1;
        let manager = DownloadManager::with_options(options).unwrap();
        let request = DownloadRequest::new(
            "restart-no-resume-test",
            vec![DownloadCandidate::new(url)],
            dest.clone(),
        )
        .with_expected_size(body.len() as u64);

        let result = manager.download_one(request, |_| {}).unwrap();

        assert!(!result.ranged);
        assert_eq!(fs::read(&dest).unwrap(), body);
        assert!(!part.exists());
        let _ = fs::remove_file(dest);
        handle.join().unwrap();
    }

    #[test]
    fn download_one_keeps_resume_when_probe_status_fails() {
        let (url, handle) = spawn_status_server(404, 1);
        let dest = unique_test_dest("resume-http-status");
        let part = crate::downloader::fs::partial_path(&dest);
        fs::write(&part, b"old partial bytes").unwrap();
        let manager = DownloadManager::with_options(DownloadEngineOptions::default()).unwrap();
        let request = DownloadRequest::new(
            "resume-http-status-test",
            vec![DownloadCandidate::new(url)],
            dest.clone(),
        )
        .with_expected_size(100);

        let error = manager.download_one(request, |_| {}).unwrap_err();

        assert!(error.contains("http status 404"));
        assert!(part.exists());
        let _ = fs::remove_file(part);
        let _ = fs::remove_file(dest);
        handle.join().unwrap();
    }

    #[test]
    fn download_many_finishes_with_global_connection_limit() {
        let body_a = b"first batch body".to_vec();
        let body_b = b"second batch body".to_vec();
        let (url_a, handle_a) = spawn_streaming_server(body_a.clone(), 2);
        let (url_b, handle_b) = spawn_streaming_server(body_b.clone(), 2);
        let dest_a = unique_test_dest("many-a");
        let dest_b = unique_test_dest("many-b");
        let mut options = DownloadEngineOptions::default();
        options.max_global_connections = 1;
        options.max_active_files = 2;
        let manager = DownloadManager::with_options(options).unwrap();
        let requests = vec![
            DownloadRequest::new(
                "many-a",
                vec![DownloadCandidate::new(url_a)],
                dest_a.clone(),
            )
            .with_expected_size(body_a.len() as u64),
            DownloadRequest::new(
                "many-b",
                vec![DownloadCandidate::new(url_b)],
                dest_b.clone(),
            )
            .with_expected_size(body_b.len() as u64),
        ];

        let outcomes = manager.download_many(requests, |_| {});

        assert_eq!(outcomes.len(), 2);
        assert!(matches!(outcomes[0], DownloadOutcome::Finished(_)));
        assert!(matches!(outcomes[1], DownloadOutcome::Finished(_)));
        assert_eq!(fs::read(&dest_a).unwrap(), body_a);
        assert_eq!(fs::read(&dest_b).unwrap(), body_b);
        let _ = fs::remove_file(dest_a);
        let _ = fs::remove_file(dest_b);
        handle_a.join().unwrap();
        handle_b.join().unwrap();
    }

    fn spawn_ranged_server(body: Vec<u8>, requests: usize) -> (String, thread::JoinHandle<()>) {
        let server = Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}", server.server_addr());
        let handle = thread::spawn(move || {
            for _ in 0..requests {
                let request = server.recv().unwrap();
                let range = range_header(&request);
                if let Some((start, end)) = range.and_then(|range| parse_range(&range, body.len()))
                {
                    let chunk = body[start..=end].to_vec();
                    let response = Response::from_data(chunk)
                        .with_status_code(StatusCode(206))
                        .with_header(header(
                            "Content-Range",
                            &format!("bytes {start}-{end}/{}", body.len()),
                        ))
                        .with_header(header("Accept-Ranges", "bytes"));
                    request.respond(response).unwrap();
                } else {
                    request
                        .respond(
                            Response::from_data(body.clone())
                                .with_header(header("Content-Length", &body.len().to_string())),
                        )
                        .unwrap();
                }
            }
        });
        (url, handle)
    }

    fn spawn_wrong_content_range_server(body: Vec<u8>) -> (String, thread::JoinHandle<()>) {
        let server = Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}", server.server_addr());
        let handle = thread::spawn(move || {
            for request_index in 0..2 {
                let request = server.recv().unwrap();
                let range = range_header(&request);
                if request_index == 0 {
                    if let Some((start, end)) =
                        range.and_then(|range| parse_range(&range, body.len()))
                    {
                        let chunk = body[start..=end].to_vec();
                        request
                            .respond(
                                Response::from_data(chunk)
                                    .with_status_code(StatusCode(206))
                                    .with_header(header(
                                        "Content-Range",
                                        &format!("bytes {start}-{end}/{}", body.len()),
                                    ))
                                    .with_header(header("Accept-Ranges", "bytes")),
                            )
                            .unwrap();
                    } else {
                        request.respond(Response::empty(StatusCode(400))).unwrap();
                    }
                } else {
                    request
                        .respond(
                            Response::from_data(body.clone())
                                .with_status_code(StatusCode(206))
                                .with_header(header(
                                    "Content-Range",
                                    &format!("bytes 1-{}/{}", body.len(), body.len() + 1),
                                ))
                                .with_header(header("Accept-Ranges", "bytes")),
                        )
                        .unwrap();
                }
            }
        });
        (url, handle)
    }

    fn spawn_short_content_range_server(body: Vec<u8>) -> (String, thread::JoinHandle<()>) {
        let server = Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}", server.server_addr());
        let handle = thread::spawn(move || {
            for request_index in 0..2 {
                let request = server.recv().unwrap();
                let range = range_header(&request);
                if request_index == 0 {
                    if let Some((start, end)) =
                        range.and_then(|range| parse_range(&range, body.len()))
                    {
                        let chunk = body[start..=end].to_vec();
                        request
                            .respond(
                                Response::from_data(chunk)
                                    .with_status_code(StatusCode(206))
                                    .with_header(header(
                                        "Content-Range",
                                        &format!("bytes {start}-{end}/{}", body.len()),
                                    ))
                                    .with_header(header("Accept-Ranges", "bytes")),
                            )
                            .unwrap();
                    } else {
                        request.respond(Response::empty(StatusCode(400))).unwrap();
                    }
                } else {
                    request
                        .respond(
                            Response::from_data(body.clone())
                                .with_status_code(StatusCode(206))
                                .with_header(header(
                                    "Content-Range",
                                    &format!("bytes 0-{}/{}", body.len() / 2, body.len()),
                                ))
                                .with_header(header("Accept-Ranges", "bytes")),
                        )
                        .unwrap();
                }
            }
        });
        (url, handle)
    }

    fn spawn_abort_preserves_error_server(body: Vec<u8>) -> (String, thread::JoinHandle<()>) {
        let server = Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}", server.server_addr());
        let handle = thread::spawn(move || {
            let mut workers = Vec::new();
            for request_index in 0..3 {
                let request = server.recv().unwrap();
                let body = body.clone();
                workers.push(thread::spawn(move || {
                    let range = range_header(&request);
                    if request_index == 0 {
                        respond_range_or_full(request, &body, range, false, false);
                        return;
                    }

                    let Some((start, _)) = range
                        .as_deref()
                        .and_then(|range| parse_range(range, body.len()))
                    else {
                        respond_range_or_full(request, &body, range, false, false);
                        return;
                    };

                    if start == 0 {
                        thread::sleep(Duration::from_millis(500));
                        respond_range_or_full(request, &body, range, false, false);
                    } else {
                        respond_range_or_full(request, &body, range, true, false);
                    }
                }));
            }

            for worker in workers {
                let _ = worker.join();
            }
        });
        (url, handle)
    }

    fn spawn_streaming_server(body: Vec<u8>, requests: usize) -> (String, thread::JoinHandle<()>) {
        let server = Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}", server.server_addr());
        let handle = thread::spawn(move || {
            for _ in 0..requests {
                let request = server.recv().unwrap();
                request
                    .respond(
                        Response::from_data(body.clone())
                            .with_header(header("Content-Length", &body.len().to_string())),
                    )
                    .unwrap();
            }
        });
        (url, handle)
    }

    fn spawn_status_server(status: u16, requests: usize) -> (String, thread::JoinHandle<()>) {
        let server = Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}", server.server_addr());
        let handle = thread::spawn(move || {
            for _ in 0..requests {
                let request = server.recv().unwrap();
                request
                    .respond(Response::empty(StatusCode(status)))
                    .unwrap();
            }
        });
        (url, handle)
    }

    fn range_header(request: &tiny_http::Request) -> Option<String> {
        request
            .headers()
            .iter()
            .find(|header| header.field.equiv("Range"))
            .map(|header| header.value.as_str().to_string())
    }

    fn parse_range(value: &str, len: usize) -> Option<(usize, usize)> {
        let range = value.strip_prefix("bytes=")?;
        let (start, end) = range.split_once('-')?;
        let start = start.parse::<usize>().ok()?;
        let end = if end.is_empty() {
            len.saturating_sub(1)
        } else {
            end.parse::<usize>().ok()?.min(len.saturating_sub(1))
        };
        if start <= end && end < len {
            Some((start, end))
        } else {
            None
        }
    }

    fn respond_range_or_full(
        request: tiny_http::Request,
        body: &[u8],
        range: Option<String>,
        shift_start: bool,
        short_end: bool,
    ) {
        if let Some((start, end)) = range.and_then(|range| parse_range(&range, body.len())) {
            let chunk = body[start..=end].to_vec();
            let header_start = if shift_start { start + 1 } else { start };
            let header_end = if short_end {
                start + (end - start) / 2
            } else {
                end
            };
            let _ = request.respond(
                Response::from_data(chunk)
                    .with_status_code(StatusCode(206))
                    .with_header(header(
                        "Content-Range",
                        &format!("bytes {header_start}-{header_end}/{}", body.len()),
                    ))
                    .with_header(header("Accept-Ranges", "bytes")),
            );
        } else {
            let _ = request.respond(
                Response::from_data(body.to_vec())
                    .with_header(header("Content-Length", &body.len().to_string())),
            );
        }
    }

    fn header(name: &str, value: &str) -> Header {
        Header::from_bytes(name.as_bytes(), value.as_bytes()).unwrap()
    }

    fn unique_test_dest(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|time| time.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("oaoi-{label}-download-test-{nonce}.bin"))
    }
}

fn next_ready_request(
    queue: &Arc<Mutex<VecDeque<QueuedRequest>>>,
    cancel: &AtomicBool,
) -> Option<QueuedRequest> {
    loop {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }

        let wait = {
            let mut queue = queue.lock().unwrap();
            if queue.is_empty() {
                return None;
            }

            let now = Instant::now();
            if let Some(index) = queue.iter().position(|item| item.available_at <= now) {
                return queue.remove(index);
            }

            queue
                .iter()
                .map(|item| item.available_at.saturating_duration_since(now))
                .min()
                .unwrap_or_else(|| Duration::from_millis(250))
        };

        thread::sleep(wait.min(Duration::from_millis(250)));
    }
}

fn sleep_with_cancel(duration: Duration, cancel: &AtomicBool) -> Result<(), String> {
    let started = Instant::now();
    while started.elapsed() < duration {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".to_string());
        }
        let remaining = duration.saturating_sub(started.elapsed());
        thread::sleep(remaining.min(Duration::from_millis(250)));
    }
    Ok(())
}

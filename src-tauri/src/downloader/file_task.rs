//! 单文件下载执行器。
//!
//! 候选地址兜底、Range 探测、动态扩展分片、流式降级、断点保存和最终校验
//! 都在这里统一协调。

use crate::downloader::async_http;
use crate::downloader::event::{
    DownloadEvent, DownloadOutcome, DownloadProgress, DownloadResult, EventHandler, SegmentProgress,
};
use crate::downloader::fs as download_fs;
use crate::downloader::health::{CandidateHealth, CandidateHealthAction, CandidateHealthConfig};
use crate::downloader::options::DownloadEngineOptions;
use crate::downloader::pool::{ConnectionPermit, ConnectionPool};
use crate::downloader::probe::{parse_content_range, probe_candidate, resolve_total, RangeProbe};
use crate::downloader::request::{DownloadProtocol, DownloadRequest};
use crate::downloader::resume::{load_resume_state, save_resume_state, ResumeIdentity};
use crate::downloader::segment::{
    active_segments, all_segments_done, current_downloaded, first_segment_error, fresh_segments,
    max_connections_for_size, next_waiting_segment, snapshot_segments, split_largest_segment,
    update_max_connections_seen, FileRuntimeState, Segment,
};
use crate::downloader::source_cooldown::{is_source_pressure_error, SourceCooldowns};
use crate::downloader::source_selector;
use crate::downloader::throttle::GlobalThrottle;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_RANGE, ETAG, LAST_MODIFIED};
use reqwest::{Method, StatusCode};
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug)]
struct CandidateAttemptError {
    message: String,
    cooldown_url: String,
    kind: CandidateAttemptErrorKind,
}

struct StreamingPartCleanup {
    path: PathBuf,
    armed: bool,
}

impl StreamingPartCleanup {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for StreamingPartCleanup {
    fn drop(&mut self) {
        if self.armed {
            // 只清理本轮已经成功创建的流式临时文件，不碰尚未由本轮接管的旧断点。
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CandidateAttemptErrorKind {
    Ordinary,
    ResumeUnavailable,
}

#[derive(Clone, Debug)]
enum RangeIdentityValidator {
    StrongEtag(String),
    LastModified(String),
}

impl RangeIdentityValidator {
    fn value(&self) -> &str {
        match self {
            Self::StrongEtag(value) | Self::LastModified(value) => value,
        }
    }

    fn validate_response(&self, headers: &HeaderMap) -> Result<(), String> {
        let (name, expected) = match self {
            Self::StrongEtag(expected) => (ETAG, expected),
            Self::LastModified(expected) => (LAST_MODIFIED, expected),
        };
        let Some(actual) = headers.get(name) else {
            return Ok(());
        };
        let actual = actual
            .to_str()
            .map_err(|e| format!("invalid range identity header: {e}"))?;
        if actual != expected {
            return Err(format!(
                "range identity changed: expected {expected}, got {actual}"
            ));
        }
        Ok(())
    }
}

impl CandidateAttemptError {
    fn new(message: impl Into<String>, cooldown_url: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            cooldown_url: cooldown_url.into(),
            kind: CandidateAttemptErrorKind::Ordinary,
        }
    }

    fn resume_unavailable(message: impl Into<String>, cooldown_url: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            cooldown_url: cooldown_url.into(),
            kind: CandidateAttemptErrorKind::ResumeUnavailable,
        }
    }
}

#[derive(Debug)]
pub enum FileTaskAttempt {
    Finished(DownloadResult),
    Failed {
        request_id: String,
        dest: PathBuf,
        error: String,
    },
    Deferred {
        request: DownloadRequest,
        wait: Duration,
        reason: String,
    },
}

#[derive(Debug)]
enum FileTaskResult {
    Finished(DownloadResult),
    Deferred { wait: Duration, reason: String },
}

pub struct FileTask {
    http: reqwest::Client,
    runtime: Arc<tokio::runtime::Runtime>,
    options: DownloadEngineOptions,
    pool: Arc<ConnectionPool>,
    throttle: Arc<GlobalThrottle>,
    cooldowns: Arc<SourceCooldowns>,
    cancel: Arc<AtomicBool>,
    on_event: EventHandler,
    file_index: usize,
    file_total: usize,
    retry_attempt_offset: usize,
}

impl FileTask {
    pub fn new(
        http: reqwest::Client,
        runtime: Arc<tokio::runtime::Runtime>,
        options: DownloadEngineOptions,
        pool: Arc<ConnectionPool>,
        throttle: Arc<GlobalThrottle>,
        cooldowns: Arc<SourceCooldowns>,
        cancel: Arc<AtomicBool>,
        on_event: EventHandler,
        file_index: usize,
        file_total: usize,
        retry_attempt_offset: usize,
    ) -> Self {
        Self {
            http,
            runtime,
            options,
            pool,
            throttle,
            cooldowns,
            cancel,
            on_event,
            file_index,
            file_total,
            retry_attempt_offset,
        }
    }

    pub fn run(&self, request: DownloadRequest) -> DownloadOutcome {
        match self.run_attempt(request, true) {
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
                (self.on_event)(DownloadEvent::FileFailed {
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

    pub fn run_attempt(&self, request: DownloadRequest, emit_started: bool) -> FileTaskAttempt {
        if emit_started {
            (self.on_event)(DownloadEvent::FileStarted {
                request_id: request.id.clone(),
                file_index: self.file_index,
                file_total: self.file_total,
                dest: request.dest.clone(),
            });
        }

        match self.try_run(&request) {
            Ok(FileTaskResult::Finished(result)) => {
                (self.on_event)(DownloadEvent::FileFinished(result.clone()));
                FileTaskAttempt::Finished(result)
            }
            Ok(FileTaskResult::Deferred { wait, reason }) => FileTaskAttempt::Deferred {
                request,
                wait,
                reason,
            },
            Err(error) => {
                (self.on_event)(DownloadEvent::FileFailed {
                    request_id: request.id.clone(),
                    dest: request.dest.clone(),
                    error: error.clone(),
                });
                FileTaskAttempt::Failed {
                    request_id: request.id,
                    dest: request.dest,
                    error,
                }
            }
        }
    }

    fn try_run(&self, request: &DownloadRequest) -> Result<FileTaskResult, String> {
        validate_request(request)?;
        // 流式兜底文件不可续传，新任务开始时直接清理上次异常退出的残留。
        let _ = fs::remove_file(download_fs::streaming_fallback_path(&request.dest));

        if download_fs::existing_file_ok(
            &request.dest,
            request.expected_size,
            request.expected_sha1.as_deref(),
        ) {
            let bytes = fs::metadata(&request.dest).map(|m| m.len()).unwrap_or(0);
            return Ok(FileTaskResult::Finished(DownloadResult {
                request_id: request.id.clone(),
                dest: request.dest.clone(),
                url: String::new(),
                bytes,
                sha1: request.expected_sha1.clone(),
                ranged: false,
                max_file_connections_seen: 0,
            }));
        }

        let candidates = source_selector::plan_candidates(
            request,
            dedupe_candidates(&request.candidates),
            &self.cooldowns,
            &self.options,
        );
        if candidates.is_empty() {
            return Err("no usable download candidates".to_string());
        }
        let mut failures = Vec::new();
        let mut defer_wait = None;
        let mut defer_reasons = Vec::new();
        let mut streaming_fallbacks = Vec::new();

        for (candidate_index, candidate) in candidates.iter().enumerate() {
            let has_next_candidate = candidate_index + 1 < candidates.len();
            let mut attempt = self.retry_attempt_offset;

            'candidate_attempt: {
                if self.cancel.load(Ordering::Relaxed) {
                    return Err("cancelled".to_string());
                }

                if let Some(cooldown) = self.cooldowns.remaining_for_url(&candidate.url) {
                    let reason = format!(
                        "source cooling down: {} for {}s ({})",
                        cooldown.key,
                        cooldown.remaining.as_secs(),
                        cooldown.reason
                    );
                    self.emit_candidate_skipped(
                        request,
                        candidate_index,
                        candidates.len(),
                        &candidate.url,
                        &reason,
                    );
                    remember_defer(
                        &mut defer_wait,
                        &mut defer_reasons,
                        cooldown.remaining,
                        reason.clone(),
                    );
                    failures.push(format!("{}: {}", candidate.url, reason));
                    break 'candidate_attempt;
                }

                (self.on_event)(DownloadEvent::CandidateStarted {
                    request_id: request.id.clone(),
                    candidate_index,
                    candidate_total: candidates.len(),
                    url: candidate.url.clone(),
                });

                match self.try_candidate(
                    request,
                    &candidate.url,
                    candidate_index,
                    candidates.len(),
                    has_next_candidate,
                ) {
                    Ok(result) => return Ok(FileTaskResult::Finished(result)),
                    Err(error) => {
                        let error_kind = error.kind;
                        let error_message = error.message;
                        let mut pressure_wait = None;
                        if is_source_pressure_error(&error_message) {
                            let cooldown = self.cooldowns.mark_url(
                                &error.cooldown_url,
                                error_message.clone(),
                                self.options.source_cooldown_duration,
                            );
                            self.emit_source_cooling_down(
                                request,
                                &error.cooldown_url,
                                &cooldown.key,
                                cooldown.remaining,
                                &cooldown.reason,
                            );
                            self.cooldowns
                                .mark_alias(&candidate.url, &error.cooldown_url);
                            pressure_wait = Some(cooldown.remaining);
                        }

                        if self.should_retry_candidate(&error_message, attempt) {
                            attempt += 1;
                            self.emit_candidate_retrying(
                                request,
                                candidate_index,
                                candidates.len(),
                                &candidate.url,
                                attempt,
                                &error_message,
                            );
                            remember_defer(
                                &mut defer_wait,
                                &mut defer_reasons,
                                pressure_wait.unwrap_or(self.options.candidate_retry_delay),
                                error_message.clone(),
                            );
                        }

                        self.emit_candidate_failed(
                            request,
                            candidate_index,
                            candidates.len(),
                            &candidate.url,
                            &error_message,
                        );
                        if let Some(wait) = pressure_wait {
                            // 临时压力交给调度器延后重试，当前 worker 可以去跑别的文件。
                            remember_defer(
                                &mut defer_wait,
                                &mut defer_reasons,
                                wait,
                                error_message.clone(),
                            );
                        }
                        if error_kind == CandidateAttemptErrorKind::ResumeUnavailable {
                            if !streaming_fallbacks
                                .iter()
                                .any(|(index, _, _)| *index == candidate_index)
                            {
                                streaming_fallbacks.push((
                                    candidate_index,
                                    error.cooldown_url.clone(),
                                    error_message.clone(),
                                ));
                            }
                        }
                        failures.push(format!("{}: {}", candidate.url, error_message));
                        break 'candidate_attempt;
                    }
                }
            }
        }

        if let Some(wait) = defer_wait {
            return Ok(FileTaskResult::Deferred {
                wait,
                reason: summarize_defer_reasons(&defer_reasons, &failures),
            });
        }

        if !streaming_fallbacks.is_empty() {
            // 有可续传数据时使用独立临时文件；禁用续传时直接从零流式重试。
            return self.try_streaming_fallbacks(request, &candidates, &streaming_fallbacks);
        }

        Err(format!("all candidates failed: {}", failures.join(" | ")))
    }

    fn try_streaming_fallbacks(
        &self,
        request: &DownloadRequest,
        candidates: &[crate::downloader::request::DownloadCandidate],
        fallbacks: &[(usize, String, String)],
    ) -> Result<FileTaskResult, String> {
        let mut failures = Vec::new();
        let mut defer_wait = None;
        let mut defer_reasons = Vec::new();
        let preserve_resume_artifacts = request.allow_resume && has_resume_artifacts(&request.dest);
        for (fallback_index, (candidate_index, url, range_error)) in fallbacks.iter().enumerate() {
            if self.cancel.load(Ordering::Relaxed) {
                return Err("cancelled".to_string());
            }

            (self.on_event)(DownloadEvent::CandidateStarted {
                request_id: request.id.clone(),
                candidate_index: *candidate_index,
                candidate_total: candidates.len(),
                url: url.clone(),
            });
            let has_next_candidate = fallback_index + 1 < fallbacks.len();
            match self.download_streaming(
                request,
                url,
                request.expected_size,
                false,
                *candidate_index,
                candidates.len(),
                has_next_candidate,
                preserve_resume_artifacts,
            ) {
                Ok(result) => return Ok(FileTaskResult::Finished(result)),
                Err(stream_error) if stream_error == "cancelled" => return Err(stream_error),
                Err(stream_error) => {
                    let error = format!(
                        "ranged download failed: {}; streaming fallback failed: {}",
                        range_error, stream_error
                    );
                    self.emit_candidate_failed(
                        request,
                        *candidate_index,
                        candidates.len(),
                        url,
                        &error,
                    );
                    if is_source_pressure_error(&stream_error) {
                        let cooldown = self.cooldowns.mark_url(
                            url,
                            stream_error.clone(),
                            self.options.source_cooldown_duration,
                        );
                        self.emit_source_cooling_down(
                            request,
                            url,
                            &cooldown.key,
                            cooldown.remaining,
                            &cooldown.reason,
                        );
                        if let Some(candidate) = candidates.get(*candidate_index) {
                            self.cooldowns.mark_alias(&candidate.url, url);
                        }
                        remember_defer(
                            &mut defer_wait,
                            &mut defer_reasons,
                            cooldown.remaining,
                            error.clone(),
                        );
                    }
                    failures.push(format!("{}: {}", url, error));
                }
            }
        }

        if let Some(wait) = defer_wait {
            return Ok(FileTaskResult::Deferred {
                wait,
                reason: summarize_defer_reasons(&defer_reasons, &failures),
            });
        }

        Err(format!(
            "all streaming fallbacks failed: {}",
            failures.join(" | ")
        ))
    }

    fn try_candidate(
        &self,
        request: &DownloadRequest,
        url: &str,
        candidate_index: usize,
        candidate_total: usize,
        has_next_candidate: bool,
    ) -> Result<DownloadResult, CandidateAttemptError> {
        let wants_resume = request.allow_resume && has_resume_artifacts(&request.dest);
        if request.method != Method::GET {
            if wants_resume {
                return Err(CandidateAttemptError::resume_unavailable(
                    "resume unavailable on this candidate: method does not support range",
                    url,
                ));
            }
            return self
                .download_streaming(
                    request,
                    url,
                    request.expected_size,
                    false,
                    candidate_index,
                    candidate_total,
                    has_next_candidate,
                    false,
                )
                .map_err(|error| CandidateAttemptError::new(error, url));
        }

        let probe_result = {
            let _permit = self
                .pool
                .acquire(&self.cancel)
                .map_err(|error| CandidateAttemptError::new(error, url))?;
            self.runtime.block_on(probe_candidate(
                &self.http,
                request,
                url,
                &self.cancel,
                self.options.source_probe_timeout,
                self.options.source_probe_bytes,
            ))
        };
        let probe = match probe_result {
            Ok(probe) => probe,
            Err(probe_error) => {
                if wants_resume {
                    let message = format!(
                        "resume unavailable on this candidate: range probe failed: {}",
                        probe_error
                    );
                    return if is_resume_probe_unavailable(&probe_error) {
                        Err(CandidateAttemptError::resume_unavailable(message, url))
                    } else {
                        Err(CandidateAttemptError::new(message, url))
                    };
                }
                return self
                    .download_streaming(
                        request,
                        url,
                        request.expected_size,
                        false,
                        candidate_index,
                        candidate_total,
                        has_next_candidate,
                        false,
                    )
                    .map_err(|stream_error| {
                        CandidateAttemptError::new(
                            format!(
                                "range probe failed: {}; streaming fallback failed: {}",
                                probe_error, stream_error
                            ),
                            url,
                        )
                    });
            }
        };
        let total = resolve_total(probe.total, request.expected_size)
            .map_err(|error| CandidateAttemptError::new(error, &probe.url))?;

        if request.method == Method::GET && probe.accepts_range && total.is_some() {
            let total = total.unwrap();
            let ranged_result = self.download_ranged_resume_only(
                request,
                &probe,
                total,
                candidate_index,
                candidate_total,
                has_next_candidate,
            );
            match ranged_result {
                Ok(result) => Ok(result),
                Err(error) if error == "cancelled" => {
                    Err(CandidateAttemptError::new(error, &probe.url))
                }
                Err(error) => {
                    // 分片过程留下的断点先交给后续候选和下一轮重试，不能在当前源立即清掉。
                    Err(CandidateAttemptError::resume_unavailable(error, &probe.url))
                }
            }
        } else if wants_resume {
            Err(CandidateAttemptError::resume_unavailable(
                "resume unavailable on this candidate: source does not support range",
                &probe.url,
            ))
        } else {
            self.download_streaming(
                request,
                &probe.url,
                total,
                probe.accepts_range,
                candidate_index,
                candidate_total,
                has_next_candidate,
                false,
            )
            .map_err(|error| CandidateAttemptError::new(error, &probe.url))
        }
    }

    fn download_ranged_resume_only(
        &self,
        request: &DownloadRequest,
        probe: &RangeProbe,
        total: u64,
        candidate_index: usize,
        candidate_total: usize,
        has_next_candidate: bool,
    ) -> Result<DownloadResult, String> {
        self.download_ranged(
            request,
            probe,
            total,
            candidate_index,
            candidate_total,
            has_next_candidate,
            true,
        )
    }

    fn download_ranged(
        &self,
        request: &DownloadRequest,
        probe: &RangeProbe,
        total: u64,
        candidate_index: usize,
        candidate_total: usize,
        has_next_candidate: bool,
        resume_existing: bool,
    ) -> Result<DownloadResult, String> {
        download_fs::prepare_parent(&request.dest)?;
        let part = download_fs::partial_path(&request.dest);
        let resume = download_fs::resume_path(&request.dest);
        let identity = ResumeIdentity {
            source_url: probe.source_url.clone(),
            final_url: probe.url.clone(),
            total,
            expected_sha1: request.expected_sha1.clone(),
            etag: probe.etag.clone(),
            last_modified: probe.last_modified.clone(),
            allow_cross_source_resume: request.expected_sha1.is_some(),
        };
        let range_validator = range_identity_validator(probe);
        let segments = if request.allow_resume && resume_existing {
            load_resume_state(&resume, &part, &identity).unwrap_or_else(|| {
                let _ = fs::remove_file(&resume);
                fresh_segments(total)
            })
        } else {
            let _ = fs::remove_file(&part);
            let _ = fs::remove_file(&resume);
            fresh_segments(total)
        };
        download_fs::ensure_part_file(&part, total)?;

        let abort = Arc::new(AtomicBool::new(false));
        let durability_lock = Arc::new(Mutex::new(()));
        let state = Arc::new(FileRuntimeState::new(request.id.clone(), total, segments));
        let mut handles = Vec::new();
        let mut target_connections = 1_usize;
        let max_for_file = max_connections_for_size(
            total,
            self.options.min_split_size,
            self.options
                .max_connections_per_file
                .min(self.pool.limit())
                .max(1),
        );
        let mut last_expand = Instant::now();
        let mut last_progress = Instant::now();
        let mut last_resume_flush = Instant::now();
        let mut last_bytes = current_downloaded(&state);
        let mut health = CandidateHealth::new(Instant::now());
        let health_config = self.candidate_health_config();

        loop {
            if self.cancel.load(Ordering::Relaxed) {
                abort.store(true, Ordering::Relaxed);
                join_handles(handles);
                self.save_resume_checkpoint(
                    request,
                    &part,
                    &resume,
                    &state,
                    &identity,
                    &durability_lock,
                );
                return Err("cancelled".to_string());
            }

            if let Some(error) = first_segment_error(&state) {
                abort.store(true, Ordering::Relaxed);
                join_handles(handles);
                self.save_resume_checkpoint(
                    request,
                    &part,
                    &resume,
                    &state,
                    &identity,
                    &durability_lock,
                );
                return Err(error);
            }

            if all_segments_done(&state) {
                break;
            }

            if last_expand.elapsed() >= self.options.slow_start_interval
                && active_segments(&state) >= target_connections
                && target_connections < max_for_file
            {
                target_connections = (target_connections * 2).min(max_for_file);
                last_expand = Instant::now();
            }

            self.spawn_waiting_segments(
                &mut handles,
                request,
                &probe.url,
                &part,
                &state,
                &abort,
                &durability_lock,
                range_validator.as_ref(),
                target_connections,
            );

            while active_segments(&state) < target_connections {
                let Some(permit) = self.pool.try_acquire() else {
                    break;
                };
                let Some(segment) = split_largest_segment(&state, self.options.min_split_size)
                else {
                    drop(permit);
                    break;
                };
                self.spawn_segment_thread(
                    &mut handles,
                    request.clone(),
                    probe.url.clone(),
                    part.clone(),
                    segment,
                    state.clone(),
                    abort.clone(),
                    durability_lock.clone(),
                    range_validator.clone(),
                    permit,
                );
            }

            if last_resume_flush.elapsed() >= self.options.resume_flush_tick {
                self.save_resume_checkpoint(
                    request,
                    &part,
                    &resume,
                    &state,
                    &identity,
                    &durability_lock,
                );
                last_resume_flush = Instant::now();
            }

            if last_progress.elapsed() >= self.options.progress_tick {
                let elapsed_time = last_progress.elapsed();
                let downloaded = current_downloaded(&state).min(total);
                let elapsed = elapsed_time.as_secs_f64().max(0.001);
                let delta = downloaded.saturating_sub(last_bytes);
                let speed = ((downloaded.saturating_sub(last_bytes)) as f64 / elapsed) as u64;
                let segments = snapshot_segments(&state, elapsed_time)
                    .into_iter()
                    .map(segment_progress)
                    .collect();
                let file_connections = active_segments(&state);
                let waiting_for_global_connection =
                    file_connections == 0 && self.pool.active() >= self.pool.limit();
                last_bytes = downloaded;
                last_progress = Instant::now();
                self.emit_progress(
                    request,
                    candidate_index,
                    candidate_total,
                    downloaded,
                    Some(total),
                    speed,
                    file_connections,
                    true,
                    segments,
                );
                if waiting_for_global_connection {
                    health.reset_waiting_time();
                } else if let Some(action) = health.observe(
                    &health_config,
                    delta,
                    speed,
                    downloaded >= total,
                    has_next_candidate,
                ) {
                    abort.store(true, Ordering::Relaxed);
                    join_handles(handles);
                    self.save_resume_checkpoint(
                        request,
                        &part,
                        &resume,
                        &state,
                        &identity,
                        &durability_lock,
                    );
                    return Err(candidate_health_error(action));
                }
            }

            thread::sleep(self.options.manager_tick);
        }

        join_handles(handles);
        if let Some(error) = first_segment_error(&state) {
            return Err(error);
        }

        download_fs::finalize_part_file(
            &part,
            &request.dest,
            Some(total),
            request.expected_sha1.as_deref(),
        )?;
        let _ = fs::remove_file(&resume);

        Ok(DownloadResult {
            request_id: request.id.clone(),
            dest: request.dest.clone(),
            url: probe.url.clone(),
            bytes: total,
            sha1: request.expected_sha1.clone(),
            ranged: true,
            max_file_connections_seen: state.max_connections_seen.load(Ordering::Relaxed),
        })
    }

    fn download_streaming(
        &self,
        request: &DownloadRequest,
        url: &str,
        total: Option<u64>,
        ranged: bool,
        candidate_index: usize,
        candidate_total: usize,
        has_next_candidate: bool,
        preserve_resume_artifacts: bool,
    ) -> Result<DownloadResult, String> {
        let _permit = self.pool.acquire(&self.cancel)?;
        download_fs::prepare_parent(&request.dest)?;
        let part = if preserve_resume_artifacts {
            download_fs::streaming_fallback_path(&request.dest)
        } else {
            download_fs::partial_path(&request.dest)
        };
        let resume = download_fs::resume_path(&request.dest);
        let _ = fs::remove_file(&part);
        if !preserve_resume_artifacts {
            let _ = fs::remove_file(&resume);
        }
        let idle_timeout = effective_idle_timeout(
            self.options.read_timeout,
            self.options.candidate_no_progress_timeout,
        );

        self.runtime.block_on(async {
            let mut response = async_http::send_request(
                &self.http,
                request,
                url,
                None,
                idle_timeout,
                &self.cancel,
                None,
                "stream",
            )
            .await?;
            if !response.status().is_success() {
                return Err(format!("http status {}", response.status()));
            }

            let final_url = response.url().to_string();
            let total = resolve_total(response.content_length(), total)?;
            let mut file =
                File::create(&part).map_err(|e| format!("create part file failed: {}", e))?;
            let mut part_cleanup = StreamingPartCleanup::new(part.clone());
            let mut downloaded = 0_u64;
            let mut last_progress = Instant::now();
            let mut last_bytes = 0_u64;
            let mut health = CandidateHealth::new(Instant::now());
            let health_config = self.candidate_health_config();
            let write_chunk_size = self.options.buffer_size.max(1);

            loop {
                if self.cancel.load(Ordering::Relaxed) {
                    let _ = fs::remove_file(&part);
                    return Err("cancelled".to_string());
                }

                let Some(chunk) =
                    async_http::read_chunk(&mut response, idle_timeout, &self.cancel, "stream")
                        .await?
                else {
                    break;
                };

                for piece in chunk.chunks(write_chunk_size) {
                    self.throttle.wait_for(piece.len(), &self.cancel)?;
                    file.write_all(piece)
                        .map_err(|e| format!("write failed: {}", e))?;
                    downloaded += piece.len() as u64;
                }

                if last_progress.elapsed() >= self.options.progress_tick {
                    let elapsed_time = last_progress.elapsed();
                    let elapsed = elapsed_time.as_secs_f64().max(0.001);
                    let delta = downloaded.saturating_sub(last_bytes);
                    let speed = ((downloaded.saturating_sub(last_bytes)) as f64 / elapsed) as u64;
                    last_bytes = downloaded;
                    last_progress = Instant::now();
                    self.emit_progress(
                        request,
                        candidate_index,
                        candidate_total,
                        downloaded,
                        total,
                        speed,
                        1,
                        ranged,
                        Vec::new(),
                    );
                    if let Some(action) = health.observe(
                        &health_config,
                        delta,
                        speed,
                        total.map(|total| downloaded >= total).unwrap_or(false),
                        has_next_candidate,
                    ) {
                        let _ = fs::remove_file(&part);
                        return Err(candidate_health_error(action));
                    }
                }
            }

            download_fs::finalize_part_file(
                &part,
                &request.dest,
                total,
                request.expected_sha1.as_deref(),
            )?;
            part_cleanup.disarm();
            if preserve_resume_artifacts {
                self.discard_resume_artifacts(&request.dest);
            }

            Ok(DownloadResult {
                request_id: request.id.clone(),
                dest: request.dest.clone(),
                url: final_url,
                bytes: downloaded,
                sha1: request.expected_sha1.clone(),
                ranged,
                max_file_connections_seen: 1,
            })
        })
    }

    fn spawn_waiting_segments(
        &self,
        handles: &mut Vec<thread::JoinHandle<()>>,
        request: &DownloadRequest,
        url: &str,
        part: &Path,
        state: &Arc<FileRuntimeState>,
        abort: &Arc<AtomicBool>,
        durability_lock: &Arc<Mutex<()>>,
        range_validator: Option<&RangeIdentityValidator>,
        target_connections: usize,
    ) {
        loop {
            if active_segments(state) >= target_connections {
                return;
            }
            let Some(segment) = next_waiting_segment(state) else {
                return;
            };
            let Some(permit) = self.pool.try_acquire() else {
                segment.mark_stopped();
                return;
            };
            self.spawn_segment_thread(
                handles,
                request.clone(),
                url.to_string(),
                part.to_path_buf(),
                segment,
                state.clone(),
                abort.clone(),
                durability_lock.clone(),
                range_validator.cloned(),
                permit,
            );
        }
    }

    fn spawn_segment_thread(
        &self,
        handles: &mut Vec<thread::JoinHandle<()>>,
        request: DownloadRequest,
        url: String,
        part: PathBuf,
        segment: Arc<Segment>,
        state: Arc<FileRuntimeState>,
        abort: Arc<AtomicBool>,
        durability_lock: Arc<Mutex<()>>,
        range_validator: Option<RangeIdentityValidator>,
        permit: ConnectionPermit,
    ) {
        update_max_connections_seen(&state);
        let http = self.http.clone();
        let runtime = self.runtime.clone();
        let throttle = self.throttle.clone();
        let cancel = self.cancel.clone();
        let options = self.options.clone();
        handles.push(thread::spawn(move || {
            let _permit = permit;
            let result = run_segment_worker(
                http,
                runtime,
                request,
                url,
                part,
                segment.clone(),
                state.clone(),
                abort.clone(),
                cancel.clone(),
                throttle,
                durability_lock,
                range_validator,
                options,
            );
            match result {
                Ok(()) => segment.mark_stopped(),
                Err(error) => {
                    if error == "cancelled"
                        && (abort.load(Ordering::Relaxed) || cancel.load(Ordering::Relaxed))
                    {
                        segment.mark_stopped();
                    } else {
                        segment.fail(error);
                        abort.store(true, Ordering::Relaxed);
                    }
                }
            }
        }));
    }

    fn emit_progress(
        &self,
        request: &DownloadRequest,
        candidate_index: usize,
        candidate_total: usize,
        downloaded: u64,
        total: Option<u64>,
        speed_bytes_per_sec: u64,
        file_connections: usize,
        ranged: bool,
        segments: Vec<SegmentProgress>,
    ) {
        (self.on_event)(DownloadEvent::Progress(DownloadProgress {
            request_id: request.id.clone(),
            file_index: self.file_index,
            file_total: self.file_total,
            candidate_index,
            candidate_total,
            downloaded,
            total,
            speed_bytes_per_sec,
            file_connections,
            global_connections: self.pool.active(),
            global_connection_limit: self.pool.limit(),
            ranged,
            segments,
        }));
    }

    fn emit_candidate_failed(
        &self,
        request: &DownloadRequest,
        candidate_index: usize,
        candidate_total: usize,
        url: &str,
        error: &str,
    ) {
        (self.on_event)(DownloadEvent::CandidateFailed {
            request_id: request.id.clone(),
            candidate_index,
            candidate_total,
            url: url.to_string(),
            error: error.to_string(),
        });
    }

    fn emit_candidate_skipped(
        &self,
        request: &DownloadRequest,
        candidate_index: usize,
        candidate_total: usize,
        url: &str,
        reason: &str,
    ) {
        (self.on_event)(DownloadEvent::CandidateSkipped {
            request_id: request.id.clone(),
            candidate_index,
            candidate_total,
            url: url.to_string(),
            reason: reason.to_string(),
        });
    }

    fn emit_source_cooling_down(
        &self,
        request: &DownloadRequest,
        url: &str,
        source_key: &str,
        remaining: Duration,
        reason: &str,
    ) {
        (self.on_event)(DownloadEvent::SourceCoolingDown {
            request_id: request.id.clone(),
            source_key: source_key.to_string(),
            url: url.to_string(),
            wait_seconds: remaining.as_secs(),
            reason: reason.to_string(),
        });
    }

    fn emit_candidate_retrying(
        &self,
        request: &DownloadRequest,
        candidate_index: usize,
        candidate_total: usize,
        url: &str,
        attempt: usize,
        reason: &str,
    ) {
        (self.on_event)(DownloadEvent::CandidateRetrying {
            request_id: request.id.clone(),
            candidate_index,
            candidate_total,
            url: url.to_string(),
            attempt,
            wait_seconds: if attempt == 0 {
                0
            } else {
                self.options.candidate_retry_delay.as_secs()
            },
            reason: reason.to_string(),
        });
    }

    fn should_retry_candidate(&self, error: &str, attempt: usize) -> bool {
        if attempt >= self.options.candidate_retry_attempts {
            return false;
        }

        let error = error.to_ascii_lowercase();
        if error.contains("sha1 check failed")
            || error.contains("size check failed")
            || error.contains("size mismatch")
            || error.contains("invalid header")
            || error.contains("empty destination")
            || error.contains("http status 400")
            || error.contains("http status 401")
            || error.contains("http status 403")
            || error.contains("http status 404")
        {
            return false;
        }

        error.contains("candidate failed")
            || error.contains("stalled")
            || error.contains("too slow")
            || error.contains("timed out")
            || error.contains("timeout")
            || error.contains("connection")
            || error.contains("request failed")
            || error.contains("read failed")
            || error.contains("ended early")
            || error.contains("retry exhausted")
            || error.contains("http status 408")
            || error.contains("http status 429")
            || error.contains("http status 500")
            || error.contains("http status 502")
            || error.contains("http status 503")
            || error.contains("http status 504")
            || error.contains("http status 522")
    }

    fn sleep_before_candidate_retry(&self) -> Result<(), String> {
        sleep_with_cancel(self.options.candidate_retry_delay, &self.cancel)
    }

    fn candidate_health_config(&self) -> CandidateHealthConfig {
        let low_speed_limit = match self.throttle.limit() {
            Some(limit) if limit <= self.options.candidate_low_speed_limit => 0,
            _ => self.options.candidate_low_speed_limit,
        };
        CandidateHealthConfig {
            no_progress_timeout: self.options.candidate_no_progress_timeout,
            low_speed_limit,
            low_speed_window: self.options.candidate_low_speed_window,
        }
    }

    fn discard_resume_artifacts(&self, dest: &Path) {
        // 只有所有源都不能接断点时才清理，避免提前丢掉可续传的字节。
        let _ = fs::remove_file(download_fs::partial_path(dest));
        let _ = fs::remove_file(download_fs::resume_path(dest));
    }

    fn save_resume_checkpoint(
        &self,
        request: &DownloadRequest,
        part: &Path,
        resume: &Path,
        state: &Arc<FileRuntimeState>,
        identity: &ResumeIdentity,
        durability_lock: &Arc<Mutex<()>>,
    ) {
        // 断点保存失败不直接中断下载，但必须通知上层。
        let result = persist_resume_checkpoint(part, resume, state, identity, durability_lock);
        if let Err(error) = result {
            (self.on_event)(DownloadEvent::ResumeSaveFailed {
                request_id: request.id.clone(),
                dest: request.dest.clone(),
                error,
            });
        }
    }
}

fn persist_resume_checkpoint(
    part: &Path,
    resume: &Path,
    state: &Arc<FileRuntimeState>,
    identity: &ResumeIdentity,
    durability_lock: &Mutex<()>,
) -> Result<(), String> {
    let _guard = durability_lock
        .lock()
        .map_err(|_| "resume durability lock poisoned".to_string())?;
    // 游标只能在同一把锁内推进，先同步数据再发布对应的续传元数据。
    download_fs::sync_file_data(part)?;
    save_resume_state(resume, state, identity)
}

fn run_segment_worker(
    http: reqwest::Client,
    runtime: Arc<tokio::runtime::Runtime>,
    request: DownloadRequest,
    url: String,
    part: PathBuf,
    segment: Arc<Segment>,
    state: Arc<FileRuntimeState>,
    abort: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
    throttle: Arc<GlobalThrottle>,
    durability_lock: Arc<Mutex<()>>,
    range_validator: Option<RangeIdentityValidator>,
    options: DownloadEngineOptions,
) -> Result<(), String> {
    for attempt in 0..=options.segment_retries {
        if abort.load(Ordering::Relaxed) || cancel.load(Ordering::Relaxed) {
            return Err("cancelled".to_string());
        }
        if segment.remaining() == 0 {
            segment.done.store(true, Ordering::Relaxed);
            return Ok(());
        }

        match download_segment_once(
            &http,
            &runtime,
            &request,
            &url,
            &part,
            &segment,
            state.total,
            &abort,
            &cancel,
            &throttle,
            &durability_lock,
            range_validator.as_ref(),
            options.buffer_size,
            options.read_timeout,
        ) {
            Ok(()) => return Ok(()),
            Err(error) if attempt < options.segment_retries => {
                if error.contains("range not supported") {
                    return Err(error);
                }
                thread::sleep(std::time::Duration::from_millis(250 * (attempt as u64 + 1)));
            }
            Err(error) => return Err(error),
        }
    }

    Err("segment retry exhausted".to_string())
}

fn download_segment_once(
    http: &reqwest::Client,
    runtime: &tokio::runtime::Runtime,
    request: &DownloadRequest,
    url: &str,
    part: &Path,
    segment: &Segment,
    total: u64,
    abort: &AtomicBool,
    cancel: &AtomicBool,
    throttle: &GlobalThrottle,
    durability_lock: &Mutex<()>,
    range_validator: Option<&RangeIdentityValidator>,
    buffer_size: usize,
    read_timeout: Duration,
) -> Result<(), String> {
    let start = segment.cursor.load(Ordering::Relaxed);
    let end = segment.end.load(Ordering::Relaxed);
    if start > end {
        segment.done.store(true, Ordering::Relaxed);
        return Ok(());
    }
    let write_chunk_size = buffer_size.max(1);

    runtime.block_on(async {
        let label = format!("segment {}", segment.id);
        let mut response = async_http::send_range_request(
            http,
            request,
            url,
            (start, end),
            range_validator.map(RangeIdentityValidator::value),
            read_timeout,
            cancel,
            Some(abort),
            &label,
        )
        .await?;

        if response.status() != StatusCode::PARTIAL_CONTENT {
            return Err(format!("range not supported: status {}", response.status()));
        }
        validate_segment_content_range(response.headers().get(CONTENT_RANGE), start, end, total)
            .map_err(|error| format!("range not supported: {}", error))?;
        if let Some(range_validator) = range_validator {
            range_validator.validate_response(response.headers())?;
        }

        let file = OpenOptions::new()
            .write(true)
            .open(part)
            .map_err(|e| format!("open part file failed: {}", e))?;

        loop {
            if abort.load(Ordering::Relaxed) || cancel.load(Ordering::Relaxed) {
                return Err("cancelled".to_string());
            }

            let cursor = segment.cursor.load(Ordering::Relaxed);
            let current_end = segment.end.load(Ordering::Relaxed);
            if cursor > current_end {
                segment.done.store(true, Ordering::Relaxed);
                return Ok(());
            }

            let Some(chunk) = async_http::read_chunk_with_abort(
                &mut response,
                read_timeout,
                cancel,
                Some(abort),
                &label,
            )
            .await?
            else {
                break;
            };

            for piece in chunk.chunks(write_chunk_size) {
                if abort.load(Ordering::Relaxed) || cancel.load(Ordering::Relaxed) {
                    return Err("cancelled".to_string());
                }

                let cursor = segment.cursor.load(Ordering::Relaxed);
                let current_end = segment.end.load(Ordering::Relaxed);
                if cursor > current_end {
                    segment.done.store(true, Ordering::Relaxed);
                    return Ok(());
                }

                let allowed = (current_end - cursor + 1).min(piece.len() as u64) as usize;
                if allowed > 0 {
                    throttle.wait_for(allowed, cancel)?;
                    let _durability_guard = durability_lock
                        .lock()
                        .map_err(|_| "resume durability lock poisoned".to_string())?;
                    download_fs::write_all_at(&file, &piece[..allowed], cursor)
                        .map_err(|e| format!("segment {} write failed: {}", segment.id, e))?;
                    segment.record_write(allowed as u64);
                    segment.cursor.fetch_add(allowed as u64, Ordering::Relaxed);
                }

                if allowed < piece.len() {
                    segment.done.store(true, Ordering::Relaxed);
                    return Ok(());
                }
            }
        }

        if segment.remaining() == 0 {
            segment.done.store(true, Ordering::Relaxed);
            Ok(())
        } else {
            Err(format!(
                "segment {} ended early at byte {}",
                segment.id,
                segment.cursor.load(Ordering::Relaxed)
            ))
        }
    })
}

fn validate_segment_content_range(
    header: Option<&HeaderValue>,
    requested_start: u64,
    requested_end: u64,
    expected_total: u64,
) -> Result<(), String> {
    let value = header
        .ok_or_else(|| "missing Content-Range header".to_string())?
        .to_str()
        .map_err(|e| format!("invalid Content-Range header: {}", e))?;
    let range = parse_content_range(value)
        .ok_or_else(|| format!("invalid Content-Range header: {value}"))?;
    let start = range
        .start
        .ok_or_else(|| format!("Content-Range has no start byte: {value}"))?;
    let end = range
        .end
        .ok_or_else(|| format!("Content-Range has no end byte: {value}"))?;

    if start != requested_start {
        return Err(format!(
            "Content-Range start mismatch: requested {requested_start}, got {start}"
        ));
    }
    if end != requested_end {
        return Err(format!(
            "Content-Range end mismatch: requested {requested_end}, got {end}"
        ));
    }
    if let Some(total) = range.total {
        if total != expected_total {
            return Err(format!(
                "Content-Range total mismatch: expected {expected_total}, got {total}"
            ));
        }
    }

    Ok(())
}

fn candidate_health_error(action: CandidateHealthAction) -> String {
    match action {
        CandidateHealthAction::SwitchCandidate { reason } => {
            format!("switch candidate: {}", reason)
        }
        CandidateHealthAction::FailCandidate { reason } => {
            format!("candidate failed: {}", reason)
        }
    }
}

fn is_resume_probe_unavailable(error: &str) -> bool {
    error.contains("range probe did not return usable metadata")
}

fn range_identity_validator(probe: &RangeProbe) -> Option<RangeIdentityValidator> {
    if let Some(etag) = probe.etag.as_deref().filter(|value| is_strong_etag(value)) {
        return Some(RangeIdentityValidator::StrongEtag(etag.to_string()));
    }
    probe
        .last_modified
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(|value| RangeIdentityValidator::LastModified(value.to_string()))
}

fn is_strong_etag(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty()
        && !value
            .get(..2)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("w/"))
}

fn has_resume_artifacts(dest: &Path) -> bool {
    download_fs::partial_path(dest).exists() || download_fs::resume_path(dest).exists()
}

fn remember_defer(
    wait_slot: &mut Option<Duration>,
    reasons: &mut Vec<String>,
    wait: Duration,
    reason: String,
) {
    let wait = wait.max(Duration::from_millis(250));
    *wait_slot = Some(wait_slot.map_or(wait, |current| current.min(wait)));
    if !reasons.iter().any(|item| item == &reason) {
        reasons.push(reason);
    }
}

fn summarize_defer_reasons(reasons: &[String], failures: &[String]) -> String {
    if !reasons.is_empty() {
        return reasons.join(" | ");
    }
    failures.join(" | ")
}

fn segment_progress(
    snapshot: crate::downloader::segment::SegmentRuntimeSnapshot,
) -> SegmentProgress {
    SegmentProgress {
        id: snapshot.id,
        start: snapshot.start,
        cursor: snapshot.cursor,
        end: snapshot.end,
        remaining: snapshot.remaining,
        speed_bytes_per_sec: snapshot.speed_bytes_per_sec,
        running: snapshot.running,
    }
}

fn validate_request(request: &DownloadRequest) -> Result<(), String> {
    if request.protocol != DownloadProtocol::Http {
        return Err("only http protocol is implemented in this standalone draft".to_string());
    }
    if request.candidates.is_empty() {
        return Err("no download candidates".to_string());
    }
    if request.dest.as_os_str().is_empty() {
        return Err("empty destination path".to_string());
    }
    if request.dest.is_dir() {
        return Err(format!(
            "destination path is a directory: {}",
            request.dest.display()
        ));
    }
    Ok(())
}

fn dedupe_candidates(
    candidates: &[crate::downloader::request::DownloadCandidate],
) -> Vec<crate::downloader::request::DownloadCandidate> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for candidate in candidates {
        let url = candidate.url.trim();
        if !url.is_empty() && seen.insert(url.to_string()) {
            out.push(crate::downloader::request::DownloadCandidate {
                url: url.to_string(),
                label: candidate.label.clone(),
            });
        }
    }
    out
}

fn join_handles(handles: Vec<thread::JoinHandle<()>>) {
    for handle in handles {
        let _ = handle.join();
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

fn effective_idle_timeout(read_timeout: Duration, no_progress_timeout: Duration) -> Duration {
    if no_progress_timeout.is_zero() {
        return read_timeout;
    }
    if read_timeout.is_zero() {
        return no_progress_timeout;
    }
    read_timeout.min(no_progress_timeout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn resume_metadata_is_not_published_before_part_sync() {
        let root = std::env::temp_dir().join(format!(
            "oaoi-resume-order-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let part = root.join("missing.oaoi.part");
        let resume = root.join("file.oaoi.dl");
        let state = Arc::new(FileRuntimeState::new(
            "resume-order".to_string(),
            10,
            fresh_segments(10),
        ));
        let identity = ResumeIdentity {
            source_url: "https://example.com/file".to_string(),
            final_url: "https://example.com/file".to_string(),
            total: 10,
            expected_sha1: Some("0123456789012345678901234567890123456789".to_string()),
            etag: None,
            last_modified: None,
            allow_cross_source_resume: true,
        };

        assert!(
            persist_resume_checkpoint(&part, &resume, &state, &identity, &Mutex::new(())).is_err()
        );
        assert!(!resume.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn range_validator_uses_last_modified_when_etag_is_weak() {
        let probe = RangeProbe {
            source_url: "https://example.com/file".to_string(),
            url: "https://example.com/file".to_string(),
            total: Some(10),
            accepts_range: true,
            etag: Some("W/\"weak\"".to_string()),
            last_modified: Some("Wed, 21 Oct 2015 07:28:00 GMT".to_string()),
        };

        assert!(matches!(
            range_identity_validator(&probe),
            Some(RangeIdentityValidator::LastModified(_))
        ));
    }
}

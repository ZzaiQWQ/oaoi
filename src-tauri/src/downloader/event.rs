//! 下载器对外抛出的生命周期事件和进度事件。
//!
//! 后面上层调用方可以订阅这些事件；下载内核本身不依赖具体业务入口。

use std::path::PathBuf;
use std::sync::Arc;

pub type EventHandler = Arc<dyn Fn(DownloadEvent) + Send + Sync + 'static>;

#[derive(Clone, Debug)]
pub struct SegmentProgress {
    pub id: usize,
    pub start: u64,
    pub cursor: u64,
    pub end: u64,
    pub remaining: u64,
    pub speed_bytes_per_sec: u64,
    pub running: bool,
}

#[derive(Clone, Debug)]
pub struct DownloadProgress {
    pub request_id: String,
    pub file_index: usize,
    pub file_total: usize,
    pub candidate_index: usize,
    pub candidate_total: usize,
    pub downloaded: u64,
    pub total: Option<u64>,
    pub speed_bytes_per_sec: u64,
    pub file_connections: usize,
    pub global_connections: usize,
    pub global_connection_limit: usize,
    pub ranged: bool,
    pub segments: Vec<SegmentProgress>,
}

#[derive(Clone, Debug)]
pub struct DownloadResult {
    pub request_id: String,
    pub dest: PathBuf,
    pub url: String,
    pub bytes: u64,
    pub sha1: Option<String>,
    pub ranged: bool,
    pub max_file_connections_seen: usize,
}

#[derive(Clone, Debug)]
pub enum DownloadOutcome {
    Finished(DownloadResult),
    Failed {
        request_id: String,
        dest: PathBuf,
        error: String,
    },
}

#[derive(Clone, Debug)]
pub enum DownloadEvent {
    FileQueued {
        request_id: String,
        file_index: usize,
        file_total: usize,
        dest: PathBuf,
    },
    FileStarted {
        request_id: String,
        file_index: usize,
        file_total: usize,
        dest: PathBuf,
    },
    CandidateStarted {
        request_id: String,
        candidate_index: usize,
        candidate_total: usize,
        url: String,
    },
    CandidateSkipped {
        request_id: String,
        candidate_index: usize,
        candidate_total: usize,
        url: String,
        reason: String,
    },
    CandidateFailed {
        request_id: String,
        candidate_index: usize,
        candidate_total: usize,
        url: String,
        error: String,
    },
    SourceCoolingDown {
        request_id: String,
        source_key: String,
        url: String,
        wait_seconds: u64,
        reason: String,
    },
    CandidateRetrying {
        request_id: String,
        candidate_index: usize,
        candidate_total: usize,
        url: String,
        attempt: usize,
        wait_seconds: u64,
        reason: String,
    },
    ResumeSaveFailed {
        request_id: String,
        dest: PathBuf,
        error: String,
    },
    Progress(DownloadProgress),
    FileFinished(DownloadResult),
    FileFailed {
        request_id: String,
        dest: PathBuf,
        error: String,
    },
}

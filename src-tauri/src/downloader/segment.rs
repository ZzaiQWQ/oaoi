//! 分片状态和动态切片逻辑。
//!
//! 一个分片对应一段字节范围。运行中的分片还能继续被拆开，让快连接去接
//! 剩余最多的那一段后半部分。

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct SegmentRuntimeSnapshot {
    pub id: usize,
    pub start: u64,
    pub cursor: u64,
    pub end: u64,
    pub remaining: u64,
    pub speed_bytes_per_sec: u64,
    pub running: bool,
}

#[derive(Debug)]
pub struct FileRuntimeState {
    pub request_id: String,
    pub total: u64,
    pub segments: Mutex<Vec<Arc<Segment>>>,
    pub next_segment_id: AtomicUsize,
    pub max_connections_seen: AtomicUsize,
}

impl FileRuntimeState {
    pub fn new(request_id: String, total: u64, segments: Vec<Arc<Segment>>) -> Self {
        Self {
            request_id,
            total,
            segments: Mutex::new(segments),
            next_segment_id: AtomicUsize::new(10_000),
            max_connections_seen: AtomicUsize::new(0),
        }
    }

    pub fn fresh(request_id: String, total: u64) -> Self {
        Self::new(request_id, total, fresh_segments(total))
    }
}

#[derive(Debug)]
pub struct Segment {
    pub id: usize,
    pub start: u64,
    pub cursor: AtomicU64,
    pub end: AtomicU64,
    pub running: AtomicBool,
    pub done: AtomicBool,
    pub failed: AtomicBool,
    pub error: Mutex<Option<String>>,
    pub window_bytes: AtomicU64,
    pub last_speed_bytes_per_sec: AtomicU64,
}

impl Segment {
    pub fn new(id: usize, start: u64, cursor: u64, end: u64) -> Self {
        let done = cursor > end;
        Self {
            id,
            start,
            cursor: AtomicU64::new(cursor),
            end: AtomicU64::new(end),
            running: AtomicBool::new(false),
            done: AtomicBool::new(done),
            failed: AtomicBool::new(false),
            error: Mutex::new(None),
            window_bytes: AtomicU64::new(0),
            last_speed_bytes_per_sec: AtomicU64::new(0),
        }
    }

    pub fn downloaded(&self) -> u64 {
        let cursor = self.cursor.load(Ordering::Relaxed);
        let end = self.end.load(Ordering::Relaxed);
        let capped_cursor = cursor.min(end.saturating_add(1));
        capped_cursor.saturating_sub(self.start)
    }

    pub fn remaining(&self) -> u64 {
        if self.done.load(Ordering::Relaxed) || self.failed.load(Ordering::Relaxed) {
            return 0;
        }
        let cursor = self.cursor.load(Ordering::Relaxed);
        let end = self.end.load(Ordering::Relaxed);
        if cursor > end {
            0
        } else {
            end - cursor + 1
        }
    }

    pub fn try_mark_running(&self) -> bool {
        self.running
            .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    }

    pub fn mark_stopped(&self) {
        self.running.store(false, Ordering::Relaxed);
    }

    pub fn fail(&self, error: String) {
        self.failed.store(true, Ordering::Relaxed);
        self.running.store(false, Ordering::Relaxed);
        if let Ok(mut slot) = self.error.lock() {
            *slot = Some(error);
        }
    }

    pub fn record_write(&self, bytes: u64) {
        self.window_bytes.fetch_add(bytes, Ordering::Relaxed);
    }
}

pub fn fresh_segments(total: u64) -> Vec<Arc<Segment>> {
    if total == 0 {
        Vec::new()
    } else {
        vec![Arc::new(Segment::new(0, 0, 0, total - 1))]
    }
}

pub fn max_connections_for_size(total: u64, min_split_size: u64, limit: usize) -> usize {
    if total == 0 {
        return 1;
    }
    let min_split_size = min_split_size.max(1);
    let by_size =
        (total.saturating_add(min_split_size.saturating_sub(1)) / min_split_size).max(1) as usize;
    by_size.min(limit.max(1)).max(1)
}

pub fn next_waiting_segment(state: &Arc<FileRuntimeState>) -> Option<Arc<Segment>> {
    let segments = state.segments.lock().ok()?;
    for segment in segments.iter() {
        if segment.done.load(Ordering::Relaxed)
            || segment.failed.load(Ordering::Relaxed)
            || segment.remaining() == 0
        {
            continue;
        }
        if segment.try_mark_running() {
            return Some(segment.clone());
        }
    }
    None
}

pub fn split_largest_segment(
    state: &Arc<FileRuntimeState>,
    min_split_size: u64,
) -> Option<Arc<Segment>> {
    let min_split_size = min_split_size.max(1);
    let mut segments = state.segments.lock().ok()?;
    let source = segments
        .iter()
        .filter(|segment| {
            !segment.done.load(Ordering::Relaxed)
                && !segment.failed.load(Ordering::Relaxed)
                && segment.remaining() >= min_split_size.saturating_mul(2)
        })
        .max_by_key(|segment| {
            let running_score = if segment.running.load(Ordering::Relaxed) {
                1_u8
            } else {
                0_u8
            };
            let speed = segment.last_speed_bytes_per_sec.load(Ordering::Relaxed);
            let slow_score = u64::MAX.saturating_sub(speed);
            (running_score, slow_score, segment.remaining())
        })?
        .clone();

    let cursor = source.cursor.load(Ordering::Relaxed);
    let old_end = source.end.load(Ordering::Relaxed);
    if cursor >= old_end {
        return None;
    }

    let remaining = old_end - cursor + 1;
    let split_start = cursor + remaining / 2;
    if split_start == 0 || split_start > old_end {
        return None;
    }

    source.end.store(split_start - 1, Ordering::Relaxed);
    let id = state.next_segment_id.fetch_add(1, Ordering::Relaxed);
    let segment = Arc::new(Segment::new(id, split_start, split_start, old_end));
    segment.running.store(true, Ordering::Relaxed);
    segments.push(segment.clone());
    Some(segment)
}

pub fn active_segments(state: &Arc<FileRuntimeState>) -> usize {
    state
        .segments
        .lock()
        .map(|segments| {
            segments
                .iter()
                .filter(|segment| {
                    segment.running.load(Ordering::Relaxed)
                        && !segment.done.load(Ordering::Relaxed)
                        && !segment.failed.load(Ordering::Relaxed)
                })
                .count()
        })
        .unwrap_or(0)
}

pub fn all_segments_done(state: &Arc<FileRuntimeState>) -> bool {
    state
        .segments
        .lock()
        .map(|segments| {
            segments
                .iter()
                .all(|segment| segment.done.load(Ordering::Relaxed) || segment.remaining() == 0)
        })
        .unwrap_or(false)
}

pub fn current_downloaded(state: &Arc<FileRuntimeState>) -> u64 {
    state
        .segments
        .lock()
        .map(|segments| segments.iter().map(|segment| segment.downloaded()).sum())
        .unwrap_or(0)
}

pub fn first_segment_error(state: &Arc<FileRuntimeState>) -> Option<String> {
    let segments = state.segments.lock().ok()?;
    let mut cancelled = None;
    for segment in segments.iter() {
        if !segment.failed.load(Ordering::Relaxed) {
            continue;
        }
        let error = segment
            .error
            .lock()
            .ok()
            .and_then(|slot| slot.clone())
            .unwrap_or_else(|| format!("segment {} failed", segment.id));
        if error == "cancelled" {
            cancelled.get_or_insert(error);
        } else {
            return Some(error);
        }
    }
    cancelled
}

pub fn update_max_connections_seen(state: &Arc<FileRuntimeState>) {
    let current = active_segments(state);
    let mut old = state.max_connections_seen.load(Ordering::Relaxed);
    while current > old {
        match state.max_connections_seen.compare_exchange(
            old,
            current,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(next) => old = next,
        }
    }
}

pub fn snapshot_segments(
    state: &Arc<FileRuntimeState>,
    elapsed: Duration,
) -> Vec<SegmentRuntimeSnapshot> {
    let elapsed_secs = elapsed.as_secs_f64().max(0.001);
    state
        .segments
        .lock()
        .map(|segments| {
            segments
                .iter()
                .map(|segment| {
                    let raw_cursor = segment.cursor.load(Ordering::Relaxed);
                    let end = segment.end.load(Ordering::Relaxed);
                    let cursor = raw_cursor.min(end.saturating_add(1));
                    let bytes = segment.window_bytes.swap(0, Ordering::Relaxed);
                    let speed = (bytes as f64 / elapsed_secs) as u64;
                    segment
                        .last_speed_bytes_per_sec
                        .store(speed, Ordering::Relaxed);
                    SegmentRuntimeSnapshot {
                        id: segment.id,
                        start: segment.start,
                        cursor,
                        end,
                        remaining: segment.remaining(),
                        speed_bytes_per_sec: speed,
                        running: segment.running.load(Ordering::Relaxed),
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_segment_error_prefers_real_error_over_cancelled() {
        let cancelled = Arc::new(Segment::new(0, 0, 0, 4));
        cancelled.fail("cancelled".to_string());
        let failed = Arc::new(Segment::new(1, 5, 5, 9));
        failed.fail("Content-Range end mismatch".to_string());
        let state = Arc::new(FileRuntimeState::new(
            "error-priority".to_string(),
            10,
            vec![cancelled, failed],
        ));

        assert_eq!(
            first_segment_error(&state).as_deref(),
            Some("Content-Range end mismatch")
        );
    }
}

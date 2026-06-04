//! 断点续传状态持久化。
//!
//! 临时文件保存实际字节，断点文件保存每个分片的游标。重新下载时，
//! 根据这些游标恢复还没完成的字节范围。

use crate::downloader::segment::{FileRuntimeState, Segment};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ResumeState {
    version: u32,
    total: u64,
    expected_sha1: Option<String>,
    source_url: String,
    final_url: String,
    etag: Option<String>,
    last_modified: Option<String>,
    segments: Vec<ResumeSegment>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ResumeSegment {
    start: u64,
    cursor: u64,
    end: u64,
}

#[derive(Clone, Debug)]
pub struct ResumeIdentity {
    pub source_url: String,
    pub final_url: String,
    pub total: u64,
    pub expected_sha1: Option<String>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub allow_cross_source_resume: bool,
}

pub fn save_resume_state(
    resume_path: &Path,
    state: &Arc<FileRuntimeState>,
    identity: &ResumeIdentity,
) -> Result<(), String> {
    let segments = state
        .segments
        .lock()
        .map_err(|_| "resume state lock poisoned".to_string())?;
    let resume_segments = segments
        .iter()
        .map(|segment| ResumeSegment {
            start: segment.start,
            cursor: segment.cursor.load(Ordering::Relaxed),
            end: segment.end.load(Ordering::Relaxed),
        })
        .collect();
    let state = ResumeState {
        version: 2,
        total: state.total,
        expected_sha1: identity.expected_sha1.clone(),
        source_url: identity.source_url.clone(),
        final_url: identity.final_url.clone(),
        etag: identity.etag.clone(),
        last_modified: identity.last_modified.clone(),
        segments: resume_segments,
    };
    let bytes =
        serde_json::to_vec(&state).map_err(|e| format!("serialize resume failed: {}", e))?;
    let tmp = resume_path.with_extension("oaoi.dl.tmp");
    fs::write(&tmp, bytes).map_err(|e| format!("write resume failed: {}", e))?;
    replace_resume_file(&tmp, resume_path)
}

pub fn load_resume_state(
    resume_path: &Path,
    part_path: &Path,
    identity: &ResumeIdentity,
) -> Option<Vec<Arc<Segment>>> {
    let part_len = fs::metadata(part_path).ok()?.len();
    if part_len != identity.total {
        return None;
    }

    let data = fs::read(resume_path).ok()?;
    let state: ResumeState = serde_json::from_slice(&data).ok()?;
    if state.version != 2 || state.segments.is_empty() || !resume_identity_matches(&state, identity)
    {
        return None;
    }

    restore_segments(state.segments, identity.total)
}

fn resume_identity_matches(state: &ResumeState, identity: &ResumeIdentity) -> bool {
    if state.total != identity.total || state.expected_sha1 != identity.expected_sha1 {
        return false;
    }

    if identity.expected_sha1.is_some() {
        return true;
    }

    if state.source_url == identity.source_url
        && state.final_url == identity.final_url
        && state.etag == identity.etag
        && state.last_modified == identity.last_modified
    {
        return true;
    }

    identity.allow_cross_source_resume
}

fn restore_segments(mut saved: Vec<ResumeSegment>, total: u64) -> Option<Vec<Arc<Segment>>> {
    saved.sort_by_key(|segment| segment.start);

    let mut expected_start = 0_u64;
    let mut out = Vec::new();
    for (id, segment) in saved.into_iter().enumerate() {
        if segment.start != expected_start || segment.start > segment.end || segment.end >= total {
            return None;
        }

        let next_start = segment.end.checked_add(1)?;
        if segment.cursor < segment.start || segment.cursor > next_start {
            return None;
        }

        out.push(Arc::new(Segment::new(
            id,
            segment.start,
            segment.cursor,
            segment.end,
        )));
        expected_start = next_start;
    }

    if expected_start == total {
        Some(out)
    } else {
        None
    }
}

fn replace_resume_file(tmp: &Path, resume_path: &Path) -> Result<(), String> {
    let backup = if resume_path.exists() {
        let backup = resume_backup_path(resume_path);
        fs::rename(resume_path, &backup).map_err(|e| format!("backup resume failed: {}", e))?;
        Some(backup)
    } else {
        None
    };

    match fs::rename(tmp, resume_path) {
        Ok(()) => {
            if let Some(backup) = backup {
                let _ = fs::remove_file(backup);
            }
            Ok(())
        }
        Err(error) => {
            if let Some(backup) = backup {
                let _ = fs::rename(&backup, resume_path);
            }
            let _ = fs::remove_file(tmp);
            Err(format!("replace resume failed: {}", error))
        }
    }
}

fn resume_backup_path(resume_path: &Path) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|time| time.as_millis())
        .unwrap_or(0);
    let name = resume_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("download.oaoi.dl");

    for index in 0..1000 {
        let mut out = resume_path.to_path_buf();
        out.set_file_name(format!("{name}.replace.{stamp}.{index}.bak"));
        if !out.exists() {
            return out;
        }
    }

    let mut out = resume_path.to_path_buf();
    out.set_file_name(format!("{name}.replace.{stamp}.bak"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::downloader::segment::{fresh_segments, FileRuntimeState};

    #[test]
    fn save_resume_state_replaces_existing_file() {
        let dir = std::env::temp_dir().join(format!(
            "oaoi-resume-replace-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|time| time.as_millis())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&dir).unwrap();
        let resume = dir.join("file.bin.oaoi.dl");
        let identity = ResumeIdentity {
            source_url: "https://example.com/file.bin".to_string(),
            final_url: "https://example.com/file.bin".to_string(),
            total: 10,
            expected_sha1: None,
            etag: None,
            last_modified: None,
            allow_cross_source_resume: false,
        };

        let first = Arc::new(FileRuntimeState::new(
            "first".to_string(),
            10,
            fresh_segments(10),
        ));
        save_resume_state(&resume, &first, &identity).unwrap();

        let second_segments = fresh_segments(10);
        second_segments[0].cursor.store(5, Ordering::Relaxed);
        let second = Arc::new(FileRuntimeState::new(
            "second".to_string(),
            10,
            second_segments,
        ));
        save_resume_state(&resume, &second, &identity).unwrap();

        let data = fs::read(&resume).unwrap();
        let state: ResumeState = serde_json::from_slice(&data).unwrap();
        assert_eq!(state.segments[0].cursor, 5);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn load_resume_state_rejects_gapped_or_overlapped_segments() {
        let dir = std::env::temp_dir().join(format!(
            "oaoi-resume-shape-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|time| time.as_millis())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&dir).unwrap();
        let resume = dir.join("file.bin.oaoi.dl");
        let part = dir.join("file.bin.oaoi.part");
        fs::File::create(&part).unwrap().set_len(10).unwrap();
        let identity = ResumeIdentity {
            source_url: "https://example.com/file.bin".to_string(),
            final_url: "https://example.com/file.bin".to_string(),
            total: 10,
            expected_sha1: None,
            etag: None,
            last_modified: None,
            allow_cross_source_resume: false,
        };

        write_resume_segments(
            &resume,
            vec![
                ResumeSegment {
                    start: 0,
                    cursor: 5,
                    end: 4,
                },
                ResumeSegment {
                    start: 6,
                    cursor: 6,
                    end: 9,
                },
            ],
        );
        assert!(load_resume_state(&resume, &part, &identity).is_none());

        write_resume_segments(
            &resume,
            vec![
                ResumeSegment {
                    start: 0,
                    cursor: 6,
                    end: 5,
                },
                ResumeSegment {
                    start: 5,
                    cursor: 5,
                    end: 9,
                },
            ],
        );
        assert!(load_resume_state(&resume, &part, &identity).is_none());
        let _ = fs::remove_dir_all(dir);
    }

    fn write_resume_segments(resume: &Path, segments: Vec<ResumeSegment>) {
        let state = ResumeState {
            version: 2,
            total: 10,
            expected_sha1: None,
            source_url: "https://example.com/file.bin".to_string(),
            final_url: "https://example.com/file.bin".to_string(),
            etag: None,
            last_modified: None,
            segments,
        };
        fs::write(resume, serde_json::to_vec(&state).unwrap()).unwrap();
    }
}

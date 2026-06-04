//! 文件系统辅助逻辑，负责临时文件、断点文件、最终落盘和哈希校验。

use std::fs::{self as stdfs, File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn prepare_parent(dest: &Path) -> Result<(), String> {
    if let Some(parent) = dest.parent() {
        stdfs::create_dir_all(parent).map_err(|e| format!("create parent failed: {}", e))?;
    }
    Ok(())
}

pub fn ensure_part_file(part: &Path, total: u64) -> Result<(), String> {
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(part)
        .map_err(|e| format!("open part file failed: {}", e))?;
    let current_len = file
        .metadata()
        .map_err(|e| format!("read part metadata failed: {}", e))?
        .len();
    if current_len != total {
        file.set_len(total)
            .map_err(|e| format!("preallocate part file failed: {}", e))?;
    }
    Ok(())
}

pub fn partial_path(dest: &Path) -> PathBuf {
    with_suffix(dest, ".oaoi.part")
}

pub fn resume_path(dest: &Path) -> PathBuf {
    with_suffix(dest, ".oaoi.dl")
}

fn with_suffix(dest: &Path, suffix: &str) -> PathBuf {
    let mut out = dest.to_path_buf();
    let name = dest
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("download");
    out.set_file_name(format!("{name}{suffix}"));
    out
}

pub fn finalize_part_file(
    part: &Path,
    dest: &Path,
    expected_size: Option<u64>,
    expected_sha1: Option<&str>,
) -> Result<(), String> {
    if let Some(expected_size) = expected_size {
        let actual_size = stdfs::metadata(part)
            .map_err(|e| format!("read part metadata failed: {}", e))?
            .len();
        if actual_size != expected_size {
            let _ = stdfs::remove_file(part);
            return Err(format!(
                "size check failed: expected {expected_size}, got {actual_size}"
            ));
        }
    }

    if let Some(expected_sha1) = expected_sha1 {
        let actual_sha1 = hash_file_sha1(part)?;
        if !actual_sha1.eq_ignore_ascii_case(expected_sha1) {
            let _ = stdfs::remove_file(part);
            return Err(format!(
                "sha1 check failed: expected {expected_sha1}, got {actual_sha1}"
            ));
        }
    }

    replace_part_file(part, dest)
}

pub fn existing_file_ok(
    dest: &Path,
    expected_size: Option<u64>,
    expected_sha1: Option<&str>,
) -> bool {
    if !dest.exists() {
        return false;
    }
    if let Some(expected_size) = expected_size {
        if stdfs::metadata(dest).map(|m| m.len()).ok() != Some(expected_size) {
            return false;
        }
    }
    match expected_sha1 {
        Some(expected_sha1) => hash_file_sha1(dest)
            .map(|actual| actual.eq_ignore_ascii_case(expected_sha1))
            .unwrap_or(false),
        None => expected_size.is_some(),
    }
}

fn replace_part_file(part: &Path, dest: &Path) -> Result<(), String> {
    let backup = if dest.exists() {
        let backup = replacement_backup_path(dest);
        stdfs::rename(dest, &backup).map_err(|e| format!("backup old file failed: {}", e))?;
        Some(backup)
    } else {
        None
    };

    match stdfs::rename(part, dest) {
        Ok(()) => {
            if let Some(backup) = backup {
                let _ = stdfs::remove_file(backup);
            }
            Ok(())
        }
        Err(error) => {
            if let Some(backup) = backup {
                let _ = stdfs::rename(&backup, dest);
            }
            Err(format!("move part file failed: {}", error))
        }
    }
}

fn replacement_backup_path(dest: &Path) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|time| time.as_millis())
        .unwrap_or(0);
    let name = dest
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("download");

    for index in 0..1000 {
        let mut out = dest.to_path_buf();
        out.set_file_name(format!("{name}.oaoi.replace.{stamp}.{index}.bak"));
        if !out.exists() {
            return out;
        }
    }

    let mut out = dest.to_path_buf();
    out.set_file_name(format!("{name}.oaoi.replace.{stamp}.bak"));
    out
}

pub fn hash_file_sha1(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| format!("open for sha1 failed: {}", e))?;
    let mut sha1 = sha1_smol::Sha1::new();
    let mut buffer = vec![0_u8; 128 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|e| format!("read for sha1 failed: {}", e))?;
        if read == 0 {
            break;
        }
        sha1.update(&buffer[..read]);
    }
    Ok(sha1.digest().to_string())
}

#[cfg(windows)]
pub fn write_all_at(file: &File, mut buffer: &[u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buffer.is_empty() {
        let written = file.seek_write(buffer, offset)?;
        if written == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "failed to write whole buffer",
            ));
        }
        offset += written as u64;
        buffer = &buffer[written..];
    }
    Ok(())
}

#[cfg(unix)]
pub fn write_all_at(file: &File, buffer: &[u8], offset: u64) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.write_all_at(buffer, offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finalize_removes_part_on_size_mismatch() {
        let dir = std::env::temp_dir().join(format!(
            "oaoi-finalize-size-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|time| time.as_millis())
                .unwrap_or(0)
        ));
        stdfs::create_dir_all(&dir).unwrap();
        let part = dir.join("file.bin.oaoi.part");
        let dest = dir.join("file.bin");
        stdfs::write(&part, b"bad").unwrap();

        let error = finalize_part_file(&part, &dest, Some(10), None).unwrap_err();

        assert!(error.contains("size check failed"));
        assert!(!part.exists());
        let _ = stdfs::remove_dir_all(dir);
    }
}

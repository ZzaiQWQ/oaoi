use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

const SOURCE_DIR: &str = "launcher-data";
const SOURCE_SUBDIR: &str = "modpack-sources";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceEntry {
    pub source: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_id: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha1: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_name: Option<String>,
}

#[derive(Default, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceIndex {
    #[serde(default)]
    pub files: HashMap<String, SourceEntry>,
}

pub fn load_source_index(game_root: &Path, instance_name: &str) -> SourceIndex {
    let path = source_index_path(game_root, instance_name);
    let Ok(data) = std::fs::read_to_string(path) else {
        return SourceIndex::default();
    };
    serde_json::from_str(&data).unwrap_or_default()
}

pub fn save_source_entry(
    game_root: &Path,
    instance_name: &str,
    entry: SourceEntry,
) -> Result<(), String> {
    let mut index = load_source_index(game_root, instance_name);
    let key = normalize_rel_path(&entry.path);
    if let (Some(project_id), Some(file_id)) = (entry.project_id, entry.file_id) {
        index.files.retain(|path, existing| {
            path == &key
                || existing.source != entry.source
                || existing.project_id != Some(project_id)
                || existing.file_id != Some(file_id)
        });
    }
    index.files.insert(key, entry);
    save_source_index(game_root, instance_name, &index)
}

pub fn delete_source_index(game_root: &Path, instance_name: &str) -> Result<(), String> {
    let path = source_index_path(game_root, instance_name);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

fn save_source_index(
    game_root: &Path,
    instance_name: &str,
    index: &SourceIndex,
) -> Result<(), String> {
    let dir = game_root.join(SOURCE_DIR).join(SOURCE_SUBDIR);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("{}.json", safe_index_name(instance_name)));
    let data = serde_json::to_string_pretty(index).map_err(|e| e.to_string())?;
    std::fs::write(path, data).map_err(|e| e.to_string())
}

fn source_index_path(game_root: &Path, instance_name: &str) -> std::path::PathBuf {
    game_root
        .join(SOURCE_DIR)
        .join(SOURCE_SUBDIR)
        .join(format!("{}.json", safe_index_name(instance_name)))
}

pub fn normalize_rel_path(value: &str) -> String {
    value.replace('\\', "/")
}

fn safe_index_name(value: &str) -> String {
    value
        .trim()
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect()
}

pub fn sha1_from_curseforge_hashes(value: &serde_json::Value) -> Option<String> {
    value.as_array().and_then(|hashes| {
        hashes.iter().find_map(|item| {
            let algo = item["algo"].as_u64().or_else(|| item["algorithm"].as_u64());
            if algo == Some(1) {
                item["value"].as_str().map(|v| v.to_lowercase())
            } else {
                None
            }
        })
    })
}

//! 通用下载器模块树。
//!
//! 负责任务调度、分片、断点续传、候选源切换、源冷却、限速和取消。
//! 业务层构造下载请求后，通过下载管理器统一执行。
#![allow(dead_code)]

pub mod async_http;
pub mod event;
pub mod file_task;
pub mod fs;
pub mod health;
pub mod manager;
pub mod options;
pub mod pool;
pub mod probe;
pub mod request;
pub mod resume;
pub mod segment;
pub mod source_cooldown;
pub mod source_selector;
pub mod throttle;

pub use manager::DownloadManager;
pub use options::DownloadEngineOptions;
pub use request::{DownloadCandidate, DownloadRequest};

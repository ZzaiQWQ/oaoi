//! 全局连接池。
//!
//! 每个文件流、每个文件分片都必须先从这里拿连接许可，所以整个下载器
//! 会被同一个全局连接上限约束住。

use crate::downloader::options::DEFAULT_MAX_GLOBAL_CONNECTIONS;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

static PROCESS_POOL: OnceLock<Arc<ConnectionPool>> = OnceLock::new();

#[derive(Debug)]
pub struct ConnectionPool {
    max: AtomicUsize,
    active: Mutex<usize>,
    wake: Condvar,
}

impl ConnectionPool {
    pub fn new(max: usize) -> Self {
        Self {
            max: AtomicUsize::new(max.max(1)),
            active: Mutex::new(0),
            wake: Condvar::new(),
        }
    }

    pub fn limit(&self) -> usize {
        self.max.load(Ordering::Relaxed)
    }

    pub fn set_limit(&self, max: usize) {
        self.max.store(max.max(1), Ordering::Relaxed);
        self.wake.notify_all();
    }

    pub fn active(&self) -> usize {
        *self.active.lock().unwrap()
    }

    pub fn wake_all(&self) {
        self.wake.notify_all();
    }

    pub fn acquire(self: &Arc<Self>, cancel: &Arc<AtomicBool>) -> Result<ConnectionPermit, String> {
        let mut active = self.active.lock().unwrap();
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".to_string());
            }
            if *active < self.limit() {
                *active += 1;
                return Ok(ConnectionPermit { pool: self.clone() });
            }
            let (next, _) = self
                .wake
                .wait_timeout(active, Duration::from_millis(200))
                .unwrap();
            active = next;
        }
    }

    pub fn try_acquire(self: &Arc<Self>) -> Option<ConnectionPermit> {
        let mut active = self.active.lock().ok()?;
        if *active >= self.limit() {
            return None;
        }
        *active += 1;
        Some(ConnectionPermit { pool: self.clone() })
    }

    fn release(&self) {
        let mut active = self.active.lock().unwrap();
        *active = active.saturating_sub(1);
        self.wake.notify_one();
    }
}

/// 所有下载任务共享同一个进程级连接池，阶段参数不能改写进程总上限。
pub fn process_connection_pool(_manager_limit: usize) -> Arc<ConnectionPool> {
    pool_from_cell(&PROCESS_POOL, DEFAULT_MAX_GLOBAL_CONNECTIONS)
}

/// 只有全局下载器配置入口可以动态调整进程总连接数。
pub fn configure_process_connection_pool(max: usize) -> Arc<ConnectionPool> {
    let pool = pool_from_cell(&PROCESS_POOL, DEFAULT_MAX_GLOBAL_CONNECTIONS);
    pool.set_limit(max);
    pool
}

pub fn wake_process_connection_pool() {
    if let Some(pool) = PROCESS_POOL.get() {
        pool.wake_all();
    }
}

fn pool_from_cell(cell: &OnceLock<Arc<ConnectionPool>>, max: usize) -> Arc<ConnectionPool> {
    cell.get_or_init(|| Arc::new(ConnectionPool::new(max)))
        .clone()
}

#[derive(Debug)]
pub struct ConnectionPermit {
    pool: Arc<ConnectionPool>,
}

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        self.pool.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manager_limit_hint_does_not_overwrite_process_limit() {
        let cell = OnceLock::new();
        let first = pool_from_cell(&cell, 64);
        first.set_limit(48);
        let second = pool_from_cell(&cell, 16);

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(first.limit(), 48);
    }

    #[test]
    fn process_limit_remains_dynamically_configurable() {
        let cell = OnceLock::new();
        let pool = pool_from_cell(&cell, 64);

        pool.set_limit(24);
        assert_eq!(pool.limit(), 24);
        pool.set_limit(72);
        assert_eq!(pool.limit(), 72);
    }
}

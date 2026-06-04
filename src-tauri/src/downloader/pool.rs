//! 全局连接池。
//!
//! 每个文件流、每个文件分片都必须先从这里拿连接许可，所以整个下载器
//! 会被同一个全局连接上限约束住。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

#[derive(Debug)]
pub struct ConnectionPool {
    max: usize,
    active: Mutex<usize>,
    wake: Condvar,
}

impl ConnectionPool {
    pub fn new(max: usize) -> Self {
        Self {
            max: max.max(1),
            active: Mutex::new(0),
            wake: Condvar::new(),
        }
    }

    pub fn limit(&self) -> usize {
        self.max
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
            if *active < self.max {
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
        if *active >= self.max {
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

#[derive(Debug)]
pub struct ConnectionPermit {
    pool: Arc<ConnectionPool>,
}

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        self.pool.release();
    }
}

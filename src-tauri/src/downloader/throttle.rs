//! 整个下载引擎共享的限速器。
//!
//! 限速值可以运行时修改，后面业务层要做限速时不需要改下载线程。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct GlobalThrottle {
    bytes_per_sec: AtomicU64,
    state: Mutex<ThrottleState>,
}

#[derive(Debug)]
struct ThrottleState {
    window_start: Instant,
    used: u64,
}

impl GlobalThrottle {
    pub fn new(limit: Option<u64>) -> Self {
        Self {
            bytes_per_sec: AtomicU64::new(limit.unwrap_or(0)),
            state: Mutex::new(ThrottleState {
                window_start: Instant::now(),
                used: 0,
            }),
        }
    }

    pub fn set_limit(&self, limit: Option<u64>) {
        self.bytes_per_sec
            .store(limit.unwrap_or(0), Ordering::Relaxed);
    }

    pub fn limit(&self) -> Option<u64> {
        match self.bytes_per_sec.load(Ordering::Relaxed) {
            0 => None,
            value => Some(value),
        }
    }

    pub fn wait_for(&self, bytes: usize, cancel: &AtomicBool) -> Result<(), String> {
        let limit = self.bytes_per_sec.load(Ordering::Relaxed);
        if limit == 0 || bytes == 0 {
            return Ok(());
        }

        // 单次 read 可能大于限速值，所以按桶额度分批扣。
        let mut remaining = bytes as u64;
        while remaining > 0 {
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".to_string());
            }

            let mut state = self.state.lock().unwrap();
            let elapsed = state.window_start.elapsed();
            if elapsed >= Duration::from_secs(1) {
                state.window_start = Instant::now();
                state.used = 0;
            }

            let chunk = remaining.min(limit);
            if state.used + chunk <= limit {
                state.used += chunk;
                remaining -= chunk;
                continue;
            }

            let sleep_for = Duration::from_secs(1).saturating_sub(elapsed);
            drop(state);
            thread::sleep(sleep_for.min(Duration::from_millis(100)));
        }

        Ok(())
    }
}

//! 候选源健康判断。
//!
//! 这个文件只负责判断“当前源是不是卡死或太慢”。是否真的能切换到下一个源，
//! 由调用方根据候选源数量决定；单源下载不会被当成多源兜底处理。

use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct CandidateHealthConfig {
    pub no_progress_timeout: Duration,
    pub low_speed_limit: u64,
    pub low_speed_window: Duration,
}

#[derive(Clone, Debug)]
pub enum CandidateHealthAction {
    SwitchCandidate { reason: String },
    FailCandidate { reason: String },
}

#[derive(Debug)]
pub struct CandidateHealth {
    last_progress_at: Instant,
    low_speed_since: Option<Instant>,
}

impl CandidateHealth {
    pub fn new(now: Instant) -> Self {
        Self {
            last_progress_at: now,
            low_speed_since: None,
        }
    }

    pub fn reset_waiting_time(&mut self) {
        // 等全局连接池不代表当前源没进度，不能计入卡死判断。
        self.last_progress_at = Instant::now();
        self.low_speed_since = None;
    }

    pub fn observe(
        &mut self,
        config: &CandidateHealthConfig,
        downloaded_delta: u64,
        speed_bytes_per_sec: u64,
        complete: bool,
        has_next_candidate: bool,
    ) -> Option<CandidateHealthAction> {
        if complete {
            self.low_speed_since = None;
            return None;
        }

        let now = Instant::now();
        if downloaded_delta > 0 {
            self.last_progress_at = now;
        }

        if !config.no_progress_timeout.is_zero()
            && self.last_progress_at.elapsed() >= config.no_progress_timeout
        {
            return Some(self.action(
                has_next_candidate,
                format!(
                    "candidate stalled: no progress for {}s",
                    config.no_progress_timeout.as_secs()
                ),
            ));
        }

        if config.low_speed_limit == 0 || config.low_speed_window.is_zero() {
            self.low_speed_since = None;
            return None;
        }

        if downloaded_delta > 0 && speed_bytes_per_sec < config.low_speed_limit {
            let since = self.low_speed_since.get_or_insert(now);
            if since.elapsed() >= config.low_speed_window {
                return Some(self.action(
                    has_next_candidate,
                    format!(
                        "candidate too slow: {} B/s below {} B/s for {}s",
                        speed_bytes_per_sec,
                        config.low_speed_limit,
                        config.low_speed_window.as_secs()
                    ),
                ));
            }
        } else if speed_bytes_per_sec >= config.low_speed_limit {
            self.low_speed_since = None;
        }

        None
    }

    fn action(&self, has_next_candidate: bool, reason: String) -> CandidateHealthAction {
        if has_next_candidate {
            CandidateHealthAction::SwitchCandidate { reason }
        } else {
            CandidateHealthAction::FailCandidate { reason }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_waiting_time_prevents_idle_stall() {
        let config = CandidateHealthConfig {
            no_progress_timeout: Duration::from_millis(10),
            low_speed_limit: 0,
            low_speed_window: Duration::ZERO,
        };
        let mut health = CandidateHealth::new(Instant::now() - Duration::from_secs(1));

        health.reset_waiting_time();

        assert!(health.observe(&config, 0, 0, false, true).is_none());
    }
}

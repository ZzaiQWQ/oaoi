//! 下载引擎的全局配置，控制队列、分片、重试和限速。

use std::time::Duration;

pub const DEFAULT_MAX_GLOBAL_CONNECTIONS: usize = 64;
pub const DEFAULT_MAX_ACTIVE_FILES: usize = 16;

#[derive(Clone, Debug)]
pub struct DownloadEngineOptions {
    pub max_global_connections: usize,
    pub max_active_files: usize,
    pub max_connections_per_file: usize,
    pub min_split_size: u64,
    pub buffer_size: usize,
    pub segment_retries: usize,
    /// 单个候选源多久完全没有进度就判定卡死，然后切换到下一个候选源。
    pub candidate_no_progress_timeout: Duration,
    /// 单个候选源持续低于这个速度时判定过慢，0 表示关闭低速换源。
    pub candidate_low_speed_limit: u64,
    /// 低速状态持续多久才切换候选源，避免短时间抖动导致频繁换源。
    pub candidate_low_speed_window: Duration,
    /// 没有下一个候选源可切换时，等待多久再重新请求当前源。
    pub candidate_retry_delay: Duration,
    /// 没有下一个候选源可切换时，当前源最多重新请求几次。
    pub candidate_retry_attempts: usize,
    /// 某个源表现出限流、慢速或卡死后，其他任务跳过该源的时间。
    pub source_cooldown_duration: Duration,
    /// 下载前按冷却表调整候选源顺序，不会主动请求所有源。
    pub source_cooldown_reorder_enabled: bool,
    /// 单个候选源预探测最多等待多久，避免导入任务时长时间卡在探测阶段。
    pub source_probe_timeout: Duration,
    /// 预探测最多读取多少字节，只用于判断可用性，不当成正式下载内容。
    pub source_probe_bytes: u64,
    pub slow_start_interval: Duration,
    pub manager_tick: Duration,
    pub progress_tick: Duration,
    pub resume_flush_tick: Duration,
    pub connect_timeout: Duration,
    pub read_timeout: Duration,
    pub global_speed_limit: Option<u64>,
}

impl Default for DownloadEngineOptions {
    fn default() -> Self {
        Self {
            max_global_connections: DEFAULT_MAX_GLOBAL_CONNECTIONS,
            max_active_files: DEFAULT_MAX_ACTIVE_FILES,
            max_connections_per_file: DEFAULT_MAX_GLOBAL_CONNECTIONS,
            min_split_size: 2 * 1024 * 1024,
            buffer_size: 64 * 1024,
            segment_retries: 3,
            candidate_no_progress_timeout: Duration::from_secs(10),
            candidate_low_speed_limit: 16 * 1024,
            candidate_low_speed_window: Duration::from_secs(10),
            candidate_retry_delay: Duration::from_secs(15),
            candidate_retry_attempts: 3,
            source_cooldown_duration: Duration::from_secs(15),
            source_cooldown_reorder_enabled: true,
            source_probe_timeout: Duration::from_secs(3),
            source_probe_bytes: 1,
            slow_start_interval: Duration::from_millis(700),
            manager_tick: Duration::from_millis(40),
            progress_tick: Duration::from_millis(250),
            resume_flush_tick: Duration::from_secs(1),
            connect_timeout: Duration::from_secs(15),
            read_timeout: Duration::from_secs(15),
            global_speed_limit: None,
        }
    }
}

impl DownloadEngineOptions {
    /// 新下载器默认的全局连接上限，整合包共享连接池从这里取值。
    pub fn default_global_connection_limit() -> usize {
        Self::default().max_global_connections.max(1)
    }
}

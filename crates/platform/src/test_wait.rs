//! 测试里「等某件事发生」的统一上限（#508），与 Python 的 `tools/wait_limit.py` 同一口径。
//!
//! 等待上限只防挂死：等线程或子进程到某个状态、等通道来消息、等宿主就绪都用 [`limit`]，到了就当挂死判失败。
//! 性能断言、「X 时间内不发生」的观察窗口、给产品配的短超时都不归这里，规则见 AGENTS.md「等待上限与性能断言」。
//! 环境变量 `ILLUSTASH_TEST_WAIT_SECONDS`（秒，正数）整体调整上限，默认 60 秒。
//!
//! 这是 `native/terms/tests/common/wait.rs` 的副本：平台 crate 在 terms 的下游依赖之外，不反向引用它的文件。两份要一起改。
#![allow(dead_code)]

use std::time::{Duration, Instant};

const ENV: &str = "ILLUSTASH_TEST_WAIT_SECONDS";
const DEFAULT: Duration = Duration::from_secs(60);

/// 等待上限。环境变量不是正数秒数时 panic，免得悄悄退回默认值。
pub(crate) fn limit() -> Duration {
    match std::env::var(ENV) {
        Err(_) => DEFAULT,
        Ok(value) if value.trim().is_empty() => DEFAULT,
        Ok(value) => value
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
            .map(Duration::from_secs_f64)
            .unwrap_or_else(|| panic!("{ENV} 必须是正数秒数，现在是 {value:?}")),
    }
}

/// 从现在起算的等待截止时间。
pub(crate) fn deadline() -> Instant {
    Instant::now() + limit()
}

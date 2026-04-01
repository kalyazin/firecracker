// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

//! Crate that implements Firecracker specific functionality as far as logging and metrics
//! collecting.

mod logging;
mod metrics;
pub mod rate_limited;

pub use log::{Level, debug, error, info, log_enabled, trace, warn};
// Re-export the #[macro_export] macros so callers can use
// `crate::logger::error_rate_limited` etc. consistently with other log macros.
pub use crate::{debug_rate_limited, error_rate_limited, info_rate_limited, warn_rate_limited};
pub use logging::{
    DEFAULT_INSTANCE_ID, DEFAULT_LEVEL, INSTANCE_ID, LOGGER, LevelFilter, LevelFilterFromStrError,
    LoggerConfig, LoggerInitError, LoggerUpdateError,
};
pub use metrics::{
    IncMetric, LatencyAggregateMetrics, METRICS, MetricsError, ProcessTimeReporter,
    SharedIncMetric, SharedStoreMetric, StoreMetric,
};
use utils::time::{ClockType, get_time_us};

/// Alias for `std::io::LineWriter<std::fs::File>`.
pub type FcLineWriter = std::io::LineWriter<std::fs::File>;

/// Prefix to be used in log lines for functions/modules in Firecracker
/// that are not generally available.
const DEV_PREVIEW_LOG_PREFIX: &str = "[DevPreview]";

/// Log a standard warning message indicating a given feature name
/// is in development preview.
pub fn log_dev_preview_warning(feature_name: &str, msg_opt: Option<String>) {
    match msg_opt {
        None => warn!("{DEV_PREVIEW_LOG_PREFIX} {feature_name} is in development preview."),
        Some(msg) => {
            warn!("{DEV_PREVIEW_LOG_PREFIX} {feature_name} is in development preview - {msg}")
        }
    }
}

/// Helper function for updating the value of a store metric with elapsed time since some time in a
/// past.
pub fn update_metric_with_elapsed_time(metric: &SharedStoreMetric, start_time_us: u64) -> u64 {
    let delta_us = get_time_us(ClockType::Monotonic) - start_time_us;
    metric.store(delta_us);
    delta_us
}

/// Internal helper macro implementing the rate-limiting logic.
/// Not intended for direct use — use `error_rate_limited!`, `warn_rate_limited!`,
/// `info_rate_limited!`, or `debug_rate_limited!` instead.
#[doc(hidden)]
#[macro_export]
macro_rules! __log_rate_limited_impl {
    ($level:expr, $level_macro:path, $($arg:tt)+) => {{
        if $crate::logger::log_enabled!($level) {
            static LIMITER: $crate::logger::rate_limited::LogRateLimiter =
                $crate::logger::rate_limited::LogRateLimiter::new();
            static SUPPRESSED: std::sync::atomic::AtomicU64 =
                std::sync::atomic::AtomicU64::new(0);

            if LIMITER.check() {
                let suppressed =
                    SUPPRESSED.swap(0, std::sync::atomic::Ordering::Relaxed);
                if suppressed > 0 {
                    $crate::logger::warn!(
                        "{suppressed} messages were suppressed due to rate limiting"
                    );
                }
                $level_macro!($($arg)+);
            } else {
                SUPPRESSED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                $crate::logger::IncMetric::inc(
                    &$crate::logger::METRICS.logger.rate_limited_log_count,
                );
            }
        }
    }};
}

/// Rate-limited error log. Default: 10 messages per 5-second refill period.
/// Suppressed messages are tracked via the `rate_limited_log_count` metric.
/// When logging resumes after suppression, a warn-level summary reports
/// the number of suppressed messages.
#[macro_export]
macro_rules! error_rate_limited {
    ($($arg:tt)+) => {
        $crate::__log_rate_limited_impl!(
            $crate::logger::Level::Error,
            $crate::logger::error,
            $($arg)+
        )
    };
}

/// Rate-limited warning log. Same semantics as [`error_rate_limited!`].
#[macro_export]
macro_rules! warn_rate_limited {
    ($($arg:tt)+) => {
        $crate::__log_rate_limited_impl!(
            $crate::logger::Level::Warn,
            $crate::logger::warn,
            $($arg)+
        )
    };
}

/// Rate-limited info log. Same semantics as [`error_rate_limited!`].
#[macro_export]
macro_rules! info_rate_limited {
    ($($arg:tt)+) => {
        $crate::__log_rate_limited_impl!(
            $crate::logger::Level::Info,
            $crate::logger::info,
            $($arg)+
        )
    };
}

/// Rate-limited debug log. Same semantics as [`error_rate_limited!`].
#[macro_export]
macro_rules! debug_rate_limited {
    ($($arg:tt)+) => {
        $crate::__log_rate_limited_impl!(
            $crate::logger::Level::Debug,
            $crate::logger::debug,
            $($arg)+
        )
    };
}

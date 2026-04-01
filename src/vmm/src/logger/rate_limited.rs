// Copyright 2026 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

//! Per-callsite rate limiter for logging, reusing the existing `TokenBucket`
//! implementation. Each macro invocation site gets its own independent
//! `LogRateLimiter` instance via a `static`, so flooding one callsite does
//! not suppress unrelated log messages.

use std::sync::{Mutex, OnceLock};

use crate::rate_limiter::TokenBucket;

/// Maximum number of messages allowed per refill period.
pub const DEFAULT_BURST: u64 = 10;

/// Refill period in milliseconds (5 seconds).
pub const DEFAULT_REFILL_TIME_MS: u64 = 5000;

/// Per-callsite rate limiter wrapping a `TokenBucket` in a `Mutex`.
///
/// Uses `OnceLock` for lazy initialization since `TokenBucket::new()`
/// is not `const` (it calls `Instant::now()`).
#[derive(Debug)]
pub struct LogRateLimiter {
    inner: OnceLock<Mutex<TokenBucket>>,
}

impl Default for LogRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl LogRateLimiter {
    /// Create a new uninitialized rate limiter.
    ///
    /// This is `const` so it can be used in `static` declarations.
    /// The inner `TokenBucket` is lazily initialized on first use.
    pub const fn new() -> Self {
        Self {
            inner: OnceLock::new(),
        }
    }

    /// Check whether a message should be emitted.
    ///
    /// Returns `true` if the message should be logged, `false` if
    /// it should be suppressed.
    pub fn check(&self) -> bool {
        let mutex = self.inner.get_or_init(|| {
            Mutex::new(
                TokenBucket::new(DEFAULT_BURST, 0, DEFAULT_REFILL_TIME_MS)
                    .expect("invalid rate limiter configuration"),
            )
        });
        let mut bucket = mutex.lock().expect("rate limiter lock poisoned");
        matches!(
            bucket.reduce(1),
            crate::rate_limiter::BucketReduction::Success
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_burst_capacity_enforcement() {
        let limiter = LogRateLimiter::new();

        // First DEFAULT_BURST calls should be allowed.
        for _ in 0..DEFAULT_BURST {
            assert!(limiter.check(), "expected allow within burst");
        }

        // The next call should be suppressed.
        assert!(!limiter.check(), "expected suppress after burst");
        assert!(!limiter.check(), "expected suppress after burst");
    }

    #[test]
    fn test_callsite_independence() {
        let limiter_a = LogRateLimiter::new();
        let limiter_b = LogRateLimiter::new();

        // Exhaust limiter_a.
        for _ in 0..DEFAULT_BURST {
            limiter_a.check();
        }
        assert!(!limiter_a.check());

        // limiter_b should be unaffected.
        assert!(limiter_b.check());
    }

    #[test]
    fn test_refill_after_time() {
        let limiter = LogRateLimiter::new();

        // Exhaust burst.
        for _ in 0..DEFAULT_BURST {
            limiter.check();
        }
        assert!(!limiter.check());

        // Wait for refill. TokenBucket refills continuously, so after
        // the full refill period we should have all tokens back.
        std::thread::sleep(std::time::Duration::from_millis(
            DEFAULT_REFILL_TIME_MS + 100,
        ));
        assert!(limiter.check(), "expected allow after refill");
    }
}

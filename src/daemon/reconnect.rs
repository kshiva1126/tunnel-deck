//! Bounded reconnect scheduling.

use std::time::Duration;

pub const MAX_DELAY: Duration = Duration::from_secs(60);
pub const RESET_AFTER: Duration = Duration::from_secs(60);

/// Returns a full-jitter delay in `[0, min(60s, 2^attempt seconds)]`.
/// The caller supplies entropy so policy tests remain deterministic.
pub fn full_jitter(attempt: u32, entropy: u64) -> Duration {
    let cap_seconds = 1_u64.checked_shl(attempt.min(6)).unwrap_or(64).min(60);
    let cap_millis = cap_seconds * 1_000;
    Duration::from_millis(entropy % (cap_millis + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exponential_cap_and_full_jitter_bounds_are_stable() {
        assert_eq!(full_jitter(0, 0), Duration::ZERO);
        assert_eq!(full_jitter(0, 1_000), Duration::from_millis(1_000));
        assert_eq!(full_jitter(3, 8_001), Duration::ZERO);
        assert_eq!(full_jitter(30, 60_000), MAX_DELAY);
        assert!(full_jitter(30, u64::MAX) <= MAX_DELAY);
    }
}

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use semver::Version;

use super::types::UpdateChannel;

pub const STARTUP_DELAY_SECS: u64 = 30;
pub const CHECK_INTERVAL_SECS: u64 = 6 * 60 * 60;
pub const LATER_SECS: u64 = 60 * 60;
const MAX_JITTER_SECS: u64 = 15 * 60;
const FAILURE_RETRY_SECS: u64 = 15 * 60;
const MAX_RETRY_SECS: u64 = CHECK_INTERVAL_SECS;

#[derive(Clone, Debug)]
pub struct RetrySchedule {
    failures: u32,
    jitter_secs: u64,
}

impl RetrySchedule {
    pub fn new() -> Self {
        let jitter_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos() as u64
            % (MAX_JITTER_SECS + 1);
        Self {
            failures: 0,
            jitter_secs,
        }
    }

    pub fn record_success(&mut self) {
        self.failures = 0;
    }
    pub fn record_failure(&mut self) {
        self.failures = self.failures.saturating_add(1);
    }

    pub fn next_delay(&self) -> Duration {
        let base = if self.failures == 0 {
            CHECK_INTERVAL_SECS
        } else {
            FAILURE_RETRY_SECS.saturating_mul(1_u64 << (self.failures - 1).min(5))
        };
        Duration::from_secs(base.min(MAX_RETRY_SECS).saturating_add(self.jitter_secs))
    }

    #[cfg(test)]
    fn with_jitter(jitter_secs: u64) -> Self {
        Self {
            failures: 0,
            jitter_secs,
        }
    }
}

pub fn parse_channel(value: Option<&str>) -> UpdateChannel {
    match value {
        Some("stable") => UpdateChannel::Stable,
        Some("staging") => UpdateChannel::Staging,
        _ => UpdateChannel::Disabled,
    }
}

pub fn strictly_newer(current: &str, candidate: &str, channel: UpdateChannel) -> bool {
    let Ok(current) = Version::parse(current) else {
        return false;
    };
    let Ok(candidate) = Version::parse(candidate) else {
        return false;
    };
    let channel_matches = match channel {
        UpdateChannel::Stable => candidate.pre.is_empty(),
        UpdateChannel::Staging => candidate.pre.as_str().starts_with("staging."),
        UpdateChannel::Disabled => false,
    };
    channel_matches && candidate > current
}

pub fn defer_is_active(value: &str) -> bool {
    let Ok(deadline) =
        time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
    else {
        return false;
    };
    deadline > time::OffsetDateTime::now_utc()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_accepts_newer_versions_in_the_build_channel() {
        assert!(strictly_newer("1.0.0", "1.0.1", UpdateChannel::Stable));
        assert!(!strictly_newer("1.0.0", "1.0.0", UpdateChannel::Stable));
        assert!(!strictly_newer(
            "1.0.0",
            "1.0.0-staging.2",
            UpdateChannel::Stable
        ));
        assert!(strictly_newer(
            "1.0.0-staging.1",
            "1.0.0-staging.2",
            UpdateChannel::Staging
        ));
        assert!(!strictly_newer("1.0.0", "2.0.0", UpdateChannel::Disabled));
    }

    #[test]
    fn expired_or_invalid_defer_does_not_block_a_restart() {
        assert!(!defer_is_active("2020-01-01T00:00:00Z"));
        assert!(!defer_is_active("not-a-date"));
    }

    #[test]
    fn retries_back_off_with_stable_process_jitter_and_reset_after_success() {
        let mut schedule = RetrySchedule::with_jitter(17);
        assert_eq!(
            schedule.next_delay(),
            Duration::from_secs(CHECK_INTERVAL_SECS + 17)
        );
        schedule.record_failure();
        assert_eq!(schedule.next_delay(), Duration::from_secs(15 * 60 + 17));
        for _ in 0..10 {
            schedule.record_failure();
        }
        assert_eq!(
            schedule.next_delay(),
            Duration::from_secs(CHECK_INTERVAL_SECS + 17)
        );
        schedule.record_success();
        assert_eq!(
            schedule.next_delay(),
            Duration::from_secs(CHECK_INTERVAL_SECS + 17)
        );
    }
}

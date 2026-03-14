//! Alert deduplication and delivery guarantees.
//!
//! Provides [`AlertDeduplicator`] to suppress repeated notifications for the
//! same alert fingerprint within a configurable window, preventing alert
//! storms from flooding notification channels.

use std::collections::HashMap;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// AlertDeduplicator
// ---------------------------------------------------------------------------

/// Tracks sent alert fingerprints to suppress duplicate notifications.
///
/// An alert fingerprint is typically a hash of the alert title, severity,
/// and source.  When the same fingerprint is seen within the dedup window,
/// [`should_send`](AlertDeduplicator::should_send) returns `false`.
///
/// Expired entries are pruned lazily on every call to
/// [`should_send`](AlertDeduplicator::should_send).
///
/// # Example
///
/// ```
/// use std::time::Duration;
///
/// let mut dedup = rpg::alert_delivery::AlertDeduplicator::with_window(
///     Duration::from_secs(3600),
/// );
///
/// assert!(dedup.should_send("fp-abc"));   // first time → send
/// assert!(!dedup.should_send("fp-abc"));  // within window → suppress
/// assert!(dedup.should_send("fp-xyz"));   // different fp → send
/// ```
#[allow(dead_code)]
pub struct AlertDeduplicator {
    /// Map of fingerprint → time it was last sent.
    sent: HashMap<String, Instant>,
    /// How long to suppress the same fingerprint after it was sent.
    window: Duration,
}

impl AlertDeduplicator {
    /// Create a deduplicator with the default 1-hour window.
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self::with_window(Duration::from_secs(3600))
    }

    /// Create a deduplicator with a custom dedup window.
    #[allow(dead_code)]
    pub fn with_window(window: Duration) -> Self {
        Self {
            sent: HashMap::new(),
            window,
        }
    }

    /// Check whether an alert with the given fingerprint should be sent.
    ///
    /// Returns `true` if this is the first occurrence or the previous send
    /// was outside the dedup window.  Returns `false` if the same fingerprint
    /// was sent within the configured window.
    ///
    /// Also prunes all expired entries from the internal map on each call.
    #[allow(dead_code)]
    pub fn should_send(&mut self, fingerprint: &str) -> bool {
        let now = Instant::now();

        // Prune expired entries.
        self.sent
            .retain(|_, sent_at| now.duration_since(*sent_at) < self.window);

        if self.sent.contains_key(fingerprint) {
            return false;
        }

        self.sent.insert(fingerprint.to_owned(), now);
        true
    }

    /// Return how many fingerprints are currently tracked (non-expired).
    #[allow(dead_code)]
    pub fn active_count(&self) -> usize {
        self.sent.len()
    }
}

impl Default for AlertDeduplicator {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Fingerprint helpers
// ---------------------------------------------------------------------------

/// Compute an alert fingerprint from title, severity, and source.
///
/// The fingerprint is a lowercase hex string derived from a simple djb2-style
/// hash of the concatenated fields.  It is stable across calls with the same
/// inputs and is collision-resistant enough for deduplication purposes.
#[allow(dead_code)]
pub fn make_fingerprint(title: &str, severity: &str, source: &str) -> String {
    // djb2 hash — deterministic, no external dependencies.
    let mut hash: u64 = 5381;
    for byte in title
        .bytes()
        .chain(b"|".iter().copied())
        .chain(severity.bytes())
        .chain(b"|".iter().copied())
        .chain(source.bytes())
    {
        hash = hash.wrapping_mul(33).wrapping_add(u64::from(byte));
    }
    format!("{hash:016x}")
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_send_is_allowed() {
        let mut dedup = AlertDeduplicator::new();
        assert!(dedup.should_send("fp-unique-001"));
    }

    #[test]
    fn duplicate_within_window_is_suppressed() {
        let mut dedup = AlertDeduplicator::new();
        assert!(dedup.should_send("fp-dup"));
        assert!(!dedup.should_send("fp-dup"));
    }

    #[test]
    fn different_fingerprints_are_independent() {
        let mut dedup = AlertDeduplicator::new();
        assert!(dedup.should_send("fp-a"));
        assert!(dedup.should_send("fp-b"));
        assert!(!dedup.should_send("fp-a"));
        assert!(!dedup.should_send("fp-b"));
    }

    #[test]
    fn expired_entries_are_pruned() {
        // Use a zero-duration window so entries expire immediately.
        let mut dedup = AlertDeduplicator::with_window(Duration::from_nanos(0));
        assert!(dedup.should_send("fp-exp"));
        // Briefly sleep to let the instant advance past the zero window.
        // Spin-wait to avoid sleep dependency; Instant::now() advances.
        let start = Instant::now();
        while start.elapsed() == Duration::ZERO {}
        // After expiry the same fingerprint should be allowed again.
        assert!(dedup.should_send("fp-exp"));
    }

    #[test]
    fn active_count_tracks_non_expired() {
        let mut dedup = AlertDeduplicator::new();
        dedup.should_send("fp-1");
        dedup.should_send("fp-2");
        dedup.should_send("fp-3");
        assert_eq!(dedup.active_count(), 3);
    }

    #[test]
    fn default_constructor_matches_new() {
        let mut d1 = AlertDeduplicator::new();
        let mut d2 = AlertDeduplicator::default();
        // Both should behave identically.
        assert!(d1.should_send("x"));
        assert!(d2.should_send("x"));
        assert!(!d1.should_send("x"));
        assert!(!d2.should_send("x"));
    }

    #[test]
    fn make_fingerprint_is_deterministic() {
        let fp1 = make_fingerprint("High CPU", "critical", "pg-monitor");
        let fp2 = make_fingerprint("High CPU", "critical", "pg-monitor");
        assert_eq!(fp1, fp2);
    }

    #[test]
    fn make_fingerprint_differs_by_field() {
        let fp_a = make_fingerprint("High CPU", "critical", "pg-monitor");
        let fp_b = make_fingerprint("High CPU", "warning", "pg-monitor");
        let fp_c = make_fingerprint("High CPU", "critical", "other-source");
        assert_ne!(fp_a, fp_b);
        assert_ne!(fp_a, fp_c);
        assert_ne!(fp_b, fp_c);
    }

    #[test]
    fn make_fingerprint_is_16_hex_chars() {
        let fp = make_fingerprint("title", "sev", "src");
        assert_eq!(fp.len(), 16);
        assert!(fp.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn zero_window_allows_resend_after_expiry() {
        let mut dedup = AlertDeduplicator::with_window(Duration::from_nanos(1));
        assert!(dedup.should_send("fp-zero"));
        // Busy-wait until at least 1 ns has elapsed.
        let t = Instant::now();
        while t.elapsed() < Duration::from_nanos(10) {}
        // The entry has expired — should be sendable again.
        assert!(dedup.should_send("fp-zero"));
    }
}

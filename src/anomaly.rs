//! Anomaly detection for the observe loop.
//!
//! Tracks database metrics across snapshots and fires alerts when
//! thresholds are exceeded. In Observe mode, alerts are logged.
//! When RCA is available, severe anomalies auto-trigger an RCA
//! investigation.

use std::collections::VecDeque;

// ---------------------------------------------------------------------------
// Anomaly types
// ---------------------------------------------------------------------------

/// Category of detected anomaly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnomalyKind {
    /// Active session count spiked above threshold.
    SessionSpike,
    /// Wait event concentration changed significantly.
    WaitEventSpike,
    /// Blocked session count exceeded threshold.
    LockCascade,
    /// Long-running query count exceeded threshold.
    LongQuerySpike,
}

impl AnomalyKind {
    /// Human-readable label.
    pub fn label(self) -> &'static str {
        match self {
            Self::SessionSpike => "session_spike",
            Self::WaitEventSpike => "wait_event_spike",
            Self::LockCascade => "lock_cascade",
            Self::LongQuerySpike => "long_query_spike",
        }
    }
}

/// A detected anomaly event.
#[derive(Debug, Clone)]
pub struct Anomaly {
    /// What kind of anomaly.
    pub kind: AnomalyKind,
    /// Human-readable description.
    pub description: String,
    /// Should this trigger an RCA investigation?
    pub trigger_rca: bool,
}

// ---------------------------------------------------------------------------
// Detector state
// ---------------------------------------------------------------------------

/// Configuration thresholds for anomaly detection.
#[derive(Debug, Clone)]
pub struct AnomalyThresholds {
    /// Active session spike: fire when current > N * rolling average.
    pub session_spike_factor: f64,
    /// Minimum active sessions before spike detection activates.
    pub session_spike_min: u32,
    /// Blocked sessions threshold for lock cascade alert.
    pub lock_cascade_threshold: u32,
    /// Wait event concentration: fire when top wait has > N% of active sessions.
    pub wait_event_pct_threshold: f64,
    /// Long-running query count threshold.
    pub long_query_threshold: u32,
}

impl Default for AnomalyThresholds {
    fn default() -> Self {
        Self {
            session_spike_factor: 2.0,
            session_spike_min: 5,
            lock_cascade_threshold: 3,
            wait_event_pct_threshold: 50.0,
            long_query_threshold: 3,
        }
    }
}

/// Snapshot of observe-loop metrics for anomaly comparison.
#[derive(Debug, Clone, Default)]
pub struct MetricSnapshot {
    /// Active session count.
    pub active_sessions: u32,
    /// Total session count.
    pub total_sessions: u32,
    /// Top wait event session count.
    pub top_wait_count: u32,
    /// Number of blocked sessions.
    pub blocked_sessions: u32,
    /// Number of long-running queries (> 30s).
    pub long_queries: u32,
}

/// Anomaly detector maintaining a rolling window of snapshots.
pub struct AnomalyDetector {
    thresholds: AnomalyThresholds,
    /// Rolling window of recent active session counts for baseline.
    active_history: VecDeque<u32>,
    /// Maximum window size.
    window_size: usize,
    /// Whether an RCA has been triggered recently (cooldown).
    rca_cooldown: bool,
}

impl AnomalyDetector {
    /// Create a new detector with default thresholds.
    pub fn new() -> Self {
        Self {
            thresholds: AnomalyThresholds::default(),
            active_history: VecDeque::new(),
            window_size: 12, // ~2 minutes at 10s intervals
            rca_cooldown: false,
        }
    }

    /// Reset the RCA cooldown (call after RCA completes).
    pub fn reset_rca_cooldown(&mut self) {
        self.rca_cooldown = false;
    }

    /// Process a new metric snapshot and return any detected anomalies.
    pub fn check(&mut self, snapshot: &MetricSnapshot) -> Vec<Anomaly> {
        let mut anomalies = Vec::new();

        // Session spike detection.
        if !self.active_history.is_empty() {
            let avg = self.rolling_average();
            if snapshot.active_sessions >= self.thresholds.session_spike_min
                && avg > 0.0
                && f64::from(snapshot.active_sessions) > avg * self.thresholds.session_spike_factor
            {
                anomalies.push(Anomaly {
                    kind: AnomalyKind::SessionSpike,
                    description: format!(
                        "Active sessions spiked to {} (rolling avg: {avg:.0})",
                        snapshot.active_sessions
                    ),
                    trigger_rca: true,
                });
            }
        }

        // Lock cascade detection.
        if snapshot.blocked_sessions >= self.thresholds.lock_cascade_threshold {
            anomalies.push(Anomaly {
                kind: AnomalyKind::LockCascade,
                description: format!(
                    "Lock cascade: {} sessions blocked",
                    snapshot.blocked_sessions
                ),
                trigger_rca: true,
            });
        }

        // Wait event concentration.
        if snapshot.active_sessions > 0 {
            let pct =
                f64::from(snapshot.top_wait_count) / f64::from(snapshot.active_sessions) * 100.0;
            if pct > self.thresholds.wait_event_pct_threshold && snapshot.top_wait_count >= 3 {
                anomalies.push(Anomaly {
                    kind: AnomalyKind::WaitEventSpike,
                    description: format!(
                        "Top wait event has {}/{} active sessions ({pct:.0}%)",
                        snapshot.top_wait_count, snapshot.active_sessions
                    ),
                    trigger_rca: false, // Informational, not severe enough for auto-RCA.
                });
            }
        }

        // Long-running query spike.
        if snapshot.long_queries >= self.thresholds.long_query_threshold {
            anomalies.push(Anomaly {
                kind: AnomalyKind::LongQuerySpike,
                description: format!("{} long-running queries (> 30s)", snapshot.long_queries),
                trigger_rca: false,
            });
        }

        // Update rolling window.
        self.active_history.push_back(snapshot.active_sessions);
        if self.active_history.len() > self.window_size {
            self.active_history.pop_front();
        }

        // Apply RCA cooldown — only allow one auto-trigger per cooldown period.
        if self.rca_cooldown {
            for a in &mut anomalies {
                a.trigger_rca = false;
            }
        } else if anomalies.iter().any(|a| a.trigger_rca) {
            self.rca_cooldown = true;
        }

        anomalies
    }

    /// Should an RCA be triggered based on the anomalies?
    pub fn should_trigger_rca(anomalies: &[Anomaly]) -> bool {
        anomalies.iter().any(|a| a.trigger_rca)
    }

    #[allow(clippy::cast_precision_loss)]
    fn rolling_average(&self) -> f64 {
        if self.active_history.is_empty() {
            return 0.0;
        }
        let sum: u32 = self.active_history.iter().sum();
        f64::from(sum) / self.active_history.len() as f64
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn default_snapshot() -> MetricSnapshot {
        MetricSnapshot {
            active_sessions: 2,
            total_sessions: 10,
            top_wait_count: 0,
            blocked_sessions: 0,
            long_queries: 0,
        }
    }

    #[test]
    fn no_anomalies_on_normal_load() {
        let mut detector = AnomalyDetector::new();
        let snap = default_snapshot();
        // Feed a few normal snapshots to build baseline.
        for _ in 0..5 {
            let anomalies = detector.check(&snap);
            assert!(anomalies.is_empty());
        }
    }

    #[test]
    fn session_spike_detected() {
        let mut detector = AnomalyDetector::new();
        let normal = MetricSnapshot {
            active_sessions: 5,
            ..default_snapshot()
        };
        // Build baseline.
        for _ in 0..6 {
            detector.check(&normal);
        }
        // Spike to 15 (3x normal, > 2x threshold).
        let spike = MetricSnapshot {
            active_sessions: 15,
            ..default_snapshot()
        };
        let anomalies = detector.check(&spike);
        assert!(anomalies
            .iter()
            .any(|a| a.kind == AnomalyKind::SessionSpike));
    }

    #[test]
    fn lock_cascade_detected() {
        let mut detector = AnomalyDetector::new();
        let snap = MetricSnapshot {
            blocked_sessions: 5,
            ..default_snapshot()
        };
        let anomalies = detector.check(&snap);
        assert!(anomalies.iter().any(|a| a.kind == AnomalyKind::LockCascade));
    }

    #[test]
    fn wait_event_spike_detected() {
        let mut detector = AnomalyDetector::new();
        let snap = MetricSnapshot {
            active_sessions: 10,
            top_wait_count: 8, // 80% on one wait event
            ..default_snapshot()
        };
        let anomalies = detector.check(&snap);
        assert!(anomalies
            .iter()
            .any(|a| a.kind == AnomalyKind::WaitEventSpike));
    }

    #[test]
    fn long_query_spike_detected() {
        let mut detector = AnomalyDetector::new();
        let snap = MetricSnapshot {
            long_queries: 4,
            ..default_snapshot()
        };
        let anomalies = detector.check(&snap);
        assert!(anomalies
            .iter()
            .any(|a| a.kind == AnomalyKind::LongQuerySpike));
    }

    #[test]
    fn rca_cooldown_prevents_repeated_triggers() {
        let mut detector = AnomalyDetector::new();
        let cascade = MetricSnapshot {
            blocked_sessions: 5,
            ..default_snapshot()
        };
        // First check triggers RCA.
        let a1 = detector.check(&cascade);
        assert!(AnomalyDetector::should_trigger_rca(&a1));

        // Second check: cooldown prevents RCA.
        let a2 = detector.check(&cascade);
        assert!(!AnomalyDetector::should_trigger_rca(&a2));

        // After reset, RCA can trigger again.
        detector.reset_rca_cooldown();
        let a3 = detector.check(&cascade);
        assert!(AnomalyDetector::should_trigger_rca(&a3));
    }

    #[test]
    fn below_min_sessions_no_spike() {
        let mut detector = AnomalyDetector::new();
        let normal = MetricSnapshot {
            active_sessions: 1,
            ..default_snapshot()
        };
        for _ in 0..6 {
            detector.check(&normal);
        }
        // Even 4x the baseline, if below min threshold (5), no spike.
        let snap = MetricSnapshot {
            active_sessions: 4,
            ..default_snapshot()
        };
        let anomalies = detector.check(&snap);
        assert!(!anomalies
            .iter()
            .any(|a| a.kind == AnomalyKind::SessionSpike));
    }

    #[test]
    fn anomaly_kind_labels() {
        assert_eq!(AnomalyKind::SessionSpike.label(), "session_spike");
        assert_eq!(AnomalyKind::WaitEventSpike.label(), "wait_event_spike");
        assert_eq!(AnomalyKind::LockCascade.label(), "lock_cascade");
        assert_eq!(AnomalyKind::LongQuerySpike.label(), "long_query_spike");
    }

    #[test]
    fn rolling_window_caps_at_size() {
        let mut detector = AnomalyDetector::new();
        let snap = default_snapshot();
        for _ in 0..20 {
            detector.check(&snap);
        }
        assert_eq!(detector.active_history.len(), detector.window_size);
    }
}

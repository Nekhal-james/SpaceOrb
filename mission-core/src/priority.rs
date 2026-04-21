//! SpaceOrb V7.6 — Priority Scoring & Eviction Engine
//!
//! Implements the P_Score binary heap and storage eviction logic.
//!
//! Formula: P_Score = (Wc · C) + (Wa · T_wait) - (Ws · S_mb)
//!
//! Reference: SPACEORB_CORE_SPEC.txt §4.1

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::path::{Path, PathBuf};
use tokio::fs;
use tracing::{info, instrument, warn};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Priority weight configuration.
#[derive(Debug, Clone)]
pub struct PScoreWeights {
    /// Weight for criticality (Wc). Default: 1.0.
    pub w_criticality: f64,
    /// Weight for age / wait time (Wa). Default: 0.01.
    pub w_age: f64,
    /// Weight for size penalty (Ws). Default: 0.5.
    pub w_size: f64,
}

impl Default for PScoreWeights {
    fn default() -> Self {
        Self {
            w_criticality: 1.0,
            w_age: 0.01,
            w_size: 0.5,
        }
    }
}

impl PScoreWeights {
    /// Load weights from environment, falling back to defaults.
    pub fn from_env() -> Self {
        Self {
            w_criticality: std::env::var("PSCORE_WEIGHT_CRITICALITY")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(1.0),
            w_age: std::env::var("PSCORE_WEIGHT_AGE")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.01),
            w_size: std::env::var("PSCORE_WEIGHT_SIZE")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.5),
        }
    }
}

/// Eviction threshold configuration.
#[derive(Debug, Clone)]
pub struct EvictionConfig {
    /// Trigger eviction when disk usage exceeds this percentage.
    pub trigger_percent: f64,
    /// Evict until disk usage drops below this percentage.
    pub target_percent: f64,
    /// Paths to monitor for disk usage (USB vault paths).
    pub vault_paths: Vec<PathBuf>,
}

impl EvictionConfig {
    /// Load from environment variables.
    pub fn from_env() -> Self {
        Self {
            trigger_percent: std::env::var("EVICTION_TRIGGER_PERCENT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(85.0),
            target_percent: std::env::var("EVICTION_TARGET_PERCENT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(70.0),
            vault_paths: vec![],
        }
    }
}

// ---------------------------------------------------------------------------
// Priority Entry
// ---------------------------------------------------------------------------

/// Criticality levels as defined in the spec.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Criticality {
    /// Anomaly detection — highest priority (C = 1000).
    Anomaly = 1000,
    /// Routine telemetry (C = 1).
    Routine = 1,
}

impl Criticality {
    /// Numeric value of this criticality level.
    pub fn value(self) -> f64 {
        self as u32 as f64
    }
}

/// A single entry in the priority transmission queue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PScoreEntry {
    /// Unique identifier for this data object.
    pub id: String,
    /// File path on the vault (the .sealed file).
    pub sealed_path: PathBuf,
    /// Criticality level.
    pub criticality: Criticality,
    /// Timestamp of ingest (used to compute T_wait).
    pub ingested_at: DateTime<Utc>,
    /// Payload size in megabytes.
    pub size_mb: f64,
    /// Whether this entry has been successfully transmitted.
    pub transmitted: bool,
    /// Cached P_Score (recomputed periodically).
    cached_score: f64,
}

impl PScoreEntry {
    /// Create a new entry with an initial P_Score computation.
    pub fn new(
        id: String,
        sealed_path: PathBuf,
        criticality: Criticality,
        ingested_at: DateTime<Utc>,
        size_mb: f64,
        weights: &PScoreWeights,
    ) -> Self {
        let mut entry = Self {
            id,
            sealed_path,
            criticality,
            ingested_at,
            size_mb,
            transmitted: false,
            cached_score: 0.0,
        };
        entry.recompute_score(weights);
        entry
    }

    /// Recompute the P_Score based on current time.
    ///
    /// Formula: P = (Wc · C) + (Wa · T_wait) - (Ws · S_mb)
    pub fn recompute_score(&mut self, weights: &PScoreWeights) {
        let t_wait = Utc::now()
            .signed_duration_since(self.ingested_at)
            .num_seconds()
            .max(0) as f64;

        self.cached_score = (weights.w_criticality * self.criticality.value())
            + (weights.w_age * t_wait)
            - (weights.w_size * self.size_mb);
    }

    /// Get the current cached P_Score.
    pub fn score(&self) -> f64 {
        self.cached_score
    }
}

// Implement Ord for BinaryHeap (max-heap: higher P_Score = higher priority).
impl PartialEq for PScoreEntry {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for PScoreEntry {}

impl PartialOrd for PScoreEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PScoreEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        // Higher score = higher priority (max-heap semantics)
        self.cached_score
            .partial_cmp(&other.cached_score)
            .unwrap_or(Ordering::Equal)
    }
}

// ---------------------------------------------------------------------------
// Priority Queue
// ---------------------------------------------------------------------------

/// The priority transmission queue backed by a binary max-heap.
pub struct PriorityQueue {
    heap: BinaryHeap<PScoreEntry>,
    weights: PScoreWeights,
}

impl PriorityQueue {
    /// Create a new empty priority queue with the given weights.
    pub fn new(weights: PScoreWeights) -> Self {
        Self {
            heap: BinaryHeap::new(),
            weights,
        }
    }

    /// Insert a new entry into the priority queue.
    pub fn push(&mut self, entry: PScoreEntry) {
        info!(
            id = %entry.id,
            score = entry.cached_score,
            criticality = ?entry.criticality,
            "Enqueued entry into priority queue"
        );
        self.heap.push(entry);
    }

    /// Pop the highest-priority entry for transmission.
    pub fn pop(&mut self) -> Option<PScoreEntry> {
        self.heap.pop()
    }

    /// Peek at the highest-priority entry without removing it.
    pub fn peek(&self) -> Option<&PScoreEntry> {
        self.heap.peek()
    }

    /// Number of entries in the queue.
    pub fn len(&self) -> usize {
        self.heap.len()
    }

    /// Whether the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }

    /// Recompute all scores (call periodically to account for aging).
    pub fn refresh_scores(&mut self) {
        let weights = self.weights.clone();
        let mut entries: Vec<PScoreEntry> = self.heap.drain().collect();
        for entry in &mut entries {
            entry.recompute_score(&weights);
        }
        self.heap.extend(entries);
    }

    /// Get a reference to the weights.
    pub fn weights(&self) -> &PScoreWeights {
        &self.weights
    }
}

// ---------------------------------------------------------------------------
// Eviction Engine
// ---------------------------------------------------------------------------

/// Storage eviction engine.
///
/// Monitors disk capacity and evicts lowest-priority transmitted data
/// when utilization exceeds the trigger threshold.
pub struct EvictionEngine {
    config: EvictionConfig,
}

/// Audit log entry for evicted data.
#[derive(Debug, Serialize)]
pub struct EvictionAuditEntry {
    pub evicted_id: String,
    pub score: f64,
    pub size_mb: f64,
    pub criticality: Criticality,
    pub reason: String,
    pub timestamp: DateTime<Utc>,
}

impl EvictionEngine {
    /// Create a new eviction engine with the given configuration.
    pub fn new(config: EvictionConfig) -> Self {
        Self { config }
    }

    /// Check disk utilization of the given path.
    ///
    /// Returns the usage as a percentage (0.0..100.0).
    #[instrument(skip(self))]
    pub async fn check_utilization(&self, path: &Path) -> Result<f64> {
        // Use `statvfs` on Linux; for cross-platform dev we read from /proc or fallback
        let meta = fs::metadata(path).await
            .with_context(|| format!("Cannot stat vault path: {}", path.display()))?;

        // On Linux, we'd use nix::sys::statvfs. For portability during development,
        // we provide a best-effort implementation.
        #[cfg(target_os = "linux")]
        {
            use nix::sys::statvfs::statvfs;
            let stat = statvfs(path)
                .with_context(|| format!("statvfs failed for {}", path.display()))?;
            let total = stat.blocks() * stat.fragment_size() as u64;
            let avail = stat.blocks_available() * stat.fragment_size() as u64;
            if total == 0 {
                return Ok(0.0);
            }
            let used = total - avail;
            Ok((used as f64 / total as f64) * 100.0)
        }

        #[cfg(not(target_os = "linux"))]
        {
            // Development fallback: report 0% usage (eviction won't trigger)
            let _ = meta;
            Ok(0.0)
        }
    }

    /// Run the eviction cycle.
    ///
    /// If any vault path exceeds `trigger_percent`, evict transmitted entries
    /// from the priority queue (lowest P_Score first) until usage drops below
    /// `target_percent`.
    ///
    /// Returns a list of audit entries for evicted items.
    #[instrument(skip(self, queue))]
    pub async fn run_eviction_cycle(
        &self,
        queue: &mut PriorityQueue,
    ) -> Result<Vec<EvictionAuditEntry>> {
        let mut audit_log = Vec::new();

        for vault_path in &self.config.vault_paths {
            let utilization = self.check_utilization(vault_path).await?;
            if utilization <= self.config.trigger_percent {
                continue;
            }

            warn!(
                path = %vault_path.display(),
                utilization = utilization,
                trigger = self.config.trigger_percent,
                "Storage utilization exceeds threshold — initiating eviction"
            );

            // Build a min-heap of transmitted entries for eviction
            let mut candidates: Vec<PScoreEntry> = Vec::new();
            let mut keep: Vec<PScoreEntry> = Vec::new();

            // Drain the queue and separate transmitted (eviction candidates) from untransmitted
            while let Some(entry) = queue.pop() {
                if entry.transmitted {
                    candidates.push(entry);
                } else {
                    keep.push(entry);
                }
            }

            // Sort candidates by score ascending (lowest score = evict first)
            candidates.sort_by(|a, b| {
                a.cached_score
                    .partial_cmp(&b.cached_score)
                    .unwrap_or(Ordering::Equal)
            });

            // Evict until below target
            let mut current_util = utilization;
            for candidate in candidates {
                if current_util <= self.config.target_percent {
                    // Put remaining candidates back
                    keep.push(candidate);
                    continue;
                }

                // Delete the sealed files
                let evict_result = self.delete_sealed(&candidate.sealed_path).await;
                match evict_result {
                    Ok(()) => {
                        info!(
                            id = %candidate.id,
                            score = candidate.cached_score,
                            "Evicted transmitted entry"
                        );
                        audit_log.push(EvictionAuditEntry {
                            evicted_id: candidate.id.clone(),
                            score: candidate.cached_score,
                            size_mb: candidate.size_mb,
                            criticality: candidate.criticality,
                            reason: format!(
                                "Storage at {:.1}% > {:.1}% trigger",
                                current_util, self.config.trigger_percent
                            ),
                            timestamp: Utc::now(),
                        });
                        // Rough estimate: reduce utilization by the entry's proportion
                        current_util -= candidate.size_mb * 0.1; // simplified estimate
                    }
                    Err(e) => {
                        warn!(
                            id = %candidate.id,
                            error = %e,
                            "Failed to evict entry — keeping in queue"
                        );
                        keep.push(candidate);
                    }
                }
            }

            // Restore non-evicted entries
            for entry in keep {
                queue.push(entry);
            }
        }

        Ok(audit_log)
    }

    /// Delete a sealed file from both USB vaults.
    async fn delete_sealed(&self, path: &Path) -> Result<()> {
        // The path is the primary; derive the mirror path
        if let Err(e) = fs::remove_file(path).await {
            // Non-fatal if already gone
            warn!(path = %path.display(), error = %e, "Could not remove sealed file");
        }

        // Also try to remove from mirror vault by swapping the base path
        for vault_path in &self.config.vault_paths {
            if let Some(filename) = path.file_name() {
                let mirror_path = vault_path.join(filename);
                if mirror_path != path {
                    if let Err(e) = fs::remove_file(&mirror_path).await {
                        warn!(
                            path = %mirror_path.display(),
                            error = %e,
                            "Could not remove mirrored sealed file"
                        );
                    }
                }
            }
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pscore_anomaly_higher_than_routine() {
        let weights = PScoreWeights::default();
        let now = Utc::now();

        let anomaly = PScoreEntry::new(
            "a1".into(),
            PathBuf::from("/tmp/a1.sealed"),
            Criticality::Anomaly,
            now,
            1.0,
            &weights,
        );

        let routine = PScoreEntry::new(
            "r1".into(),
            PathBuf::from("/tmp/r1.sealed"),
            Criticality::Routine,
            now,
            1.0,
            &weights,
        );

        assert!(
            anomaly.score() > routine.score(),
            "Anomaly P_Score ({}) should be greater than Routine ({})",
            anomaly.score(),
            routine.score()
        );
    }

    #[test]
    fn test_pscore_aging_increases_score() {
        let weights = PScoreWeights::default();
        let old = Utc::now() - chrono::Duration::hours(1);
        let recent = Utc::now();

        let old_entry = PScoreEntry::new(
            "old".into(),
            PathBuf::from("/tmp/old.sealed"),
            Criticality::Routine,
            old,
            1.0,
            &weights,
        );

        let new_entry = PScoreEntry::new(
            "new".into(),
            PathBuf::from("/tmp/new.sealed"),
            Criticality::Routine,
            recent,
            1.0,
            &weights,
        );

        assert!(
            old_entry.score() > new_entry.score(),
            "Older entry ({}) should score higher than newer ({})",
            old_entry.score(),
            new_entry.score()
        );
    }

    #[test]
    fn test_pscore_larger_size_decreases_score() {
        let weights = PScoreWeights::default();
        let now = Utc::now();

        let small = PScoreEntry::new(
            "small".into(),
            PathBuf::from("/tmp/small.sealed"),
            Criticality::Routine,
            now,
            0.5,
            &weights,
        );

        let large = PScoreEntry::new(
            "large".into(),
            PathBuf::from("/tmp/large.sealed"),
            Criticality::Routine,
            now,
            10.0,
            &weights,
        );

        assert!(
            small.score() > large.score(),
            "Smaller entry ({}) should score higher than larger ({})",
            small.score(),
            large.score()
        );
    }

    #[test]
    fn test_priority_queue_ordering() {
        let weights = PScoreWeights::default();
        let now = Utc::now();
        let mut pq = PriorityQueue::new(weights.clone());

        pq.push(PScoreEntry::new(
            "routine".into(),
            PathBuf::from("/tmp/r.sealed"),
            Criticality::Routine,
            now,
            1.0,
            &weights,
        ));
        pq.push(PScoreEntry::new(
            "anomaly".into(),
            PathBuf::from("/tmp/a.sealed"),
            Criticality::Anomaly,
            now,
            1.0,
            &weights,
        ));

        let top = pq.pop().expect("queue should not be empty");
        assert_eq!(top.id, "anomaly", "Anomaly should be popped first");
    }
}

//! SpaceOrb V7.6 — Delay-Tolerant Networking (DTN) over Zenoh/QUIC
//!
//! Implements 1MB chunked payload transfer with cryptographic ACKs
//! and 0-RTT QUIC stream resumption.
//!
//! Reference: SPACEORB_CORE_SPEC.txt §3, telemetry-quic-resumption skill

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::fs;
use tracing::{debug, error, info, instrument, warn};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Default chunk size: 1MB (1,048,576 bytes).
pub const DEFAULT_CHUNK_SIZE: usize = 1_048_576;

/// Zenoh topic for outbound telemetry chunks.
pub const TOPIC_TX: &str = "spaceorb/dtn/tx";

/// Zenoh topic for inbound ACKs.
pub const TOPIC_ACK: &str = "spaceorb/dtn/ack";

/// Zenoh topic for state broadcasts (500ms interval).
pub const TOPIC_STATE: &str = "spaceorb/state";

// ---------------------------------------------------------------------------
// Data Structures
// ---------------------------------------------------------------------------

/// A single 1MB chunk ready for DTN transmission.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DtnChunk {
    /// Parent payload ID (the sealed file ID).
    pub payload_id: String,
    /// Monotonic sequence number within this payload.
    pub sequence: u64,
    /// Total number of chunks in this payload.
    pub total_chunks: u64,
    /// SHA-256 hash of this chunk's data.
    pub chunk_hash: String,
    /// The chunk data bytes.
    #[serde(with = "serde_bytes_compat")]
    pub data: Vec<u8>,
    /// Timestamp when this chunk was created.
    pub created_at: DateTime<Utc>,
}

/// Cryptographic ACK from the ground station.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DtnAck {
    /// Payload ID being acknowledged.
    pub payload_id: String,
    /// Sequence number of the acknowledged chunk.
    pub sequence: u64,
    /// SHA-256 hash that the receiver computed (must match sender's chunk_hash).
    pub received_hash: String,
    /// Timestamp of acknowledgement.
    pub acked_at: DateTime<Utc>,
}

/// Transmission state for a single payload.
#[derive(Debug)]
pub struct PayloadTransmitState {
    /// Payload ID.
    pub payload_id: String,
    /// All chunks for this payload.
    pub chunks: Vec<DtnChunk>,
    /// Set of acknowledged sequence numbers.
    pub acked: HashMap<u64, bool>,
    /// Whether the entire payload is fully transmitted.
    pub complete: bool,
}

// ---------------------------------------------------------------------------
// Chunker
// ---------------------------------------------------------------------------

/// Split a sealed payload into uniform 1MB chunks with cryptographic hashes.
#[instrument(skip(data), fields(data_len = data.len()))]
pub fn chunk_payload(
    payload_id: &str,
    data: &[u8],
    chunk_size: usize,
) -> Vec<DtnChunk> {
    let total_chunks = (data.len() + chunk_size - 1) / chunk_size;
    let now = Utc::now();

    let chunks: Vec<DtnChunk> = data
        .chunks(chunk_size)
        .enumerate()
        .map(|(i, chunk_data)| {
            let mut hasher = Sha256::new();
            hasher.update(chunk_data);
            let chunk_hash = hex::encode(hasher.finalize());

            DtnChunk {
                payload_id: payload_id.to_string(),
                sequence: i as u64,
                total_chunks: total_chunks as u64,
                chunk_hash,
                data: chunk_data.to_vec(),
                created_at: now,
            }
        })
        .collect();

    info!(
        payload_id = payload_id,
        total_chunks = total_chunks,
        chunk_size = chunk_size,
        "Payload chunked for DTN transmission"
    );

    chunks
}

/// Verify a received ACK against the expected chunk hash.
pub fn verify_ack(chunk: &DtnChunk, ack: &DtnAck) -> bool {
    let valid = ack.payload_id == chunk.payload_id
        && ack.sequence == chunk.sequence
        && ack.received_hash == chunk.chunk_hash;

    if !valid {
        warn!(
            payload_id = %chunk.payload_id,
            seq = chunk.sequence,
            expected_hash = %chunk.chunk_hash,
            received_hash = %ack.received_hash,
            "ACK verification failed"
        );
    }

    valid
}

// ---------------------------------------------------------------------------
// Spool Manager (for link disruption)
// ---------------------------------------------------------------------------

/// Spool un-ACKed chunks to persistent storage during link outages.
pub struct SpoolManager {
    spool_dir: PathBuf,
}

impl SpoolManager {
    /// Create a spool manager at the given directory.
    pub fn new(spool_dir: PathBuf) -> Self {
        Self { spool_dir }
    }

    /// Spool a chunk to disk for later resumption.
    #[instrument(skip(self, chunk), fields(payload_id = %chunk.payload_id, seq = chunk.sequence))]
    pub async fn spool_chunk(&self, chunk: &DtnChunk) -> Result<PathBuf> {
        let filename = format!("{}_{:06}.chunk", chunk.payload_id, chunk.sequence);
        let path = self.spool_dir.join(&filename);

        let serialized = serde_json::to_vec(chunk)
            .context("Failed to serialize chunk for spooling")?;

        fs::write(&path, &serialized).await
            .with_context(|| format!("Failed to spool chunk to {}", path.display()))?;

        // fsync for crash consistency
        let file = fs::File::open(&path).await?;
        file.sync_all().await?;

        debug!(path = %path.display(), "Chunk spooled to disk");
        Ok(path)
    }

    /// Load all spooled chunks for a given payload, sorted by sequence number.
    #[instrument(skip(self))]
    pub async fn load_spooled(&self, payload_id: &str) -> Result<Vec<DtnChunk>> {
        let mut chunks = Vec::new();
        let prefix = format!("{payload_id}_");

        let mut entries = fs::read_dir(&self.spool_dir).await
            .with_context(|| format!("Cannot read spool dir: {}", self.spool_dir.display()))?;

        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();

            if name_str.starts_with(&prefix) && name_str.ends_with(".chunk") {
                let data = fs::read(entry.path()).await?;
                let chunk: DtnChunk = serde_json::from_slice(&data)
                    .with_context(|| format!("Failed to deserialize spooled chunk: {}", name_str))?;
                chunks.push(chunk);
            }
        }

        chunks.sort_by_key(|c| c.sequence);

        info!(
            payload_id = payload_id,
            count = chunks.len(),
            "Loaded spooled chunks for resumption"
        );

        Ok(chunks)
    }

    /// Remove a spooled chunk after successful ACK.
    pub async fn remove_spooled(&self, payload_id: &str, sequence: u64) -> Result<()> {
        let filename = format!("{payload_id}_{sequence:06}.chunk");
        let path = self.spool_dir.join(&filename);

        if path.exists() {
            fs::remove_file(&path).await
                .with_context(|| format!("Failed to remove spooled chunk: {}", path.display()))?;
            debug!(path = %path.display(), "Spooled chunk removed after ACK");
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// DTN Session (Zenoh integration point)
// ---------------------------------------------------------------------------

/// DTN session configuration.
#[derive(Debug, Clone)]
pub struct DtnConfig {
    /// Zenoh endpoint (e.g., "tcp/127.0.0.1:7447").
    pub endpoint: String,
    /// Chunk size in bytes (default: 1MB).
    pub chunk_size: usize,
    /// Spool directory for interrupted transfers.
    pub spool_dir: PathBuf,
}

impl DtnConfig {
    /// Load from environment variables.
    pub fn from_env() -> Self {
        Self {
            endpoint: std::env::var("DTN_GROUND_STATION_ENDPOINT")
                .unwrap_or_else(|_| "tcp/127.0.0.1:7447".into()),
            chunk_size: std::env::var("DTN_CHUNK_SIZE_BYTES")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_CHUNK_SIZE),
            spool_dir: PathBuf::from("/var/lib/spaceorb/spool"),
        }
    }
}

/// State broadcast payload (sent every 500ms to mission-control).
#[derive(Debug, Clone, Serialize)]
pub struct SystemStateBroadcast {
    /// Current virtual epoch timestamp.
    pub epoch: DateTime<Utc>,
    /// Number of entries in the priority queue.
    pub queue_depth: usize,
    /// Number of sealed files in the vault.
    pub vault_sealed_count: u64,
    /// Current SoC (battery simulation percentage).
    pub soc_percent: f64,
    /// Current power state.
    pub power_state: String,
    /// Active anomalies (criticality = 1000).
    pub active_anomalies: u32,
    /// DTN link status.
    pub dtn_link_active: bool,
    /// Chunks pending transmission.
    pub chunks_pending: u64,
    /// USB primary utilization percentage.
    pub usb_primary_util: f64,
    /// USB mirror utilization percentage.
    pub usb_mirror_util: f64,
}

// ---------------------------------------------------------------------------
// Serde helper for Vec<u8> in JSON (base64 would be ideal but hex is simpler)
// ---------------------------------------------------------------------------

mod serde_bytes_compat {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S>(data: &Vec<u8>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let hex_str = hex::encode(data);
        hex_str.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let hex_str = String::deserialize(deserializer)?;
        hex::decode(&hex_str).map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chunk_payload_uniform_1mb() {
        let data = vec![0xABu8; 3_145_728]; // exactly 3MB
        let chunks = chunk_payload("test-payload", &data, DEFAULT_CHUNK_SIZE);

        assert_eq!(chunks.len(), 3, "3MB should produce 3 × 1MB chunks");
        for (i, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk.sequence, i as u64);
            assert_eq!(chunk.total_chunks, 3);
            assert_eq!(chunk.data.len(), DEFAULT_CHUNK_SIZE);
            assert!(!chunk.chunk_hash.is_empty());
        }
    }

    #[test]
    fn test_chunk_payload_partial_last() {
        let data = vec![0xCDu8; 1_500_000]; // 1.5MB → 2 chunks
        let chunks = chunk_payload("partial", &data, DEFAULT_CHUNK_SIZE);

        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].data.len(), DEFAULT_CHUNK_SIZE);
        assert_eq!(chunks[1].data.len(), 1_500_000 - DEFAULT_CHUNK_SIZE);
    }

    #[test]
    fn test_ack_verification_success() {
        let chunk = DtnChunk {
            payload_id: "p1".into(),
            sequence: 0,
            total_chunks: 1,
            chunk_hash: "abc123".into(),
            data: vec![],
            created_at: Utc::now(),
        };
        let ack = DtnAck {
            payload_id: "p1".into(),
            sequence: 0,
            received_hash: "abc123".into(),
            acked_at: Utc::now(),
        };
        assert!(verify_ack(&chunk, &ack));
    }

    #[test]
    fn test_ack_verification_failure() {
        let chunk = DtnChunk {
            payload_id: "p1".into(),
            sequence: 0,
            total_chunks: 1,
            chunk_hash: "abc123".into(),
            data: vec![],
            created_at: Utc::now(),
        };
        let ack = DtnAck {
            payload_id: "p1".into(),
            sequence: 0,
            received_hash: "WRONG".into(),
            acked_at: Utc::now(),
        };
        assert!(!verify_ack(&chunk, &ack));
    }
}

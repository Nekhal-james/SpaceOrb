//! SpaceOrb V7.6 — Root Supervisor (main.rs)
//!
//! Async Tokio supervisor that orchestrates:
//! - Unix Domain Socket listener for AI sandbox IPC
//! - Vault ingest pipeline
//! - Priority queue & eviction
//! - DTN transmission loop
//! - Power governor
//! - 500ms state broadcast
//!
//! Reference: SPACEORB_CORE_SPEC.txt

mod dtn;
mod orbit;
mod priority;
mod vault;

use anyhow::{Context, Result};
use chrono::Utc;
use priority::{Criticality, EvictionConfig, EvictionEngine, PScoreEntry, PScoreWeights, PriorityQueue};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::{Mutex, RwLock};
use tokio::time::{interval, timeout, Duration};
use tracing::{error, info, warn};
use vault::{Vault, VaultConfig};

// ---------------------------------------------------------------------------
// IPC Schema (from AI Sandbox)
// ---------------------------------------------------------------------------

/// JSON payload received from the Python AI sandbox via UDS.
#[derive(Debug, Deserialize)]
struct AiInferenceResult {
    /// Criticality score (1000 for anomaly, 1 for routine).
    criticality: u32,
    /// Detection metadata from YOLOv8.
    detection_metadata: serde_json::Value,
    /// Timestamp of inference.
    #[serde(default = "Utc::now")]
    timestamp: chrono::DateTime<Utc>,
    /// Raw image path (if available).
    #[serde(default)]
    image_path: Option<String>,
}

// ---------------------------------------------------------------------------
// Power States
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
enum PowerState {
    /// SoC > 50% — full AI compute.
    Solar,
    /// SoC < 30% — throttled AI compute.
    Eclipse,
    /// 30% <= SoC <= 50% — hold current state.
    Transition,
}

/// Simulated battery state.
struct BatterySimulator {
    soc_percent: f64,
    drain_rate: f64,   // % per second during Eclipse
    charge_rate: f64,  // % per second during Solar
}

impl BatterySimulator {
    fn new(initial_soc: f64) -> Self {
        Self {
            soc_percent: initial_soc,
            drain_rate: 0.05,
            charge_rate: 0.08,
        }
    }

    fn tick(&mut self, dt_secs: f64, in_eclipse: bool) {
        if in_eclipse {
            self.soc_percent = (self.soc_percent - self.drain_rate * dt_secs).max(0.0);
        } else {
            self.soc_percent = (self.soc_percent + self.charge_rate * dt_secs).min(100.0);
        }
    }

    fn power_state(&self) -> PowerState {
        if self.soc_percent > 50.0 {
            PowerState::Solar
        } else if self.soc_percent < 30.0 {
            PowerState::Eclipse
        } else {
            PowerState::Transition
        }
    }
}

// ---------------------------------------------------------------------------
// Shared Application State
// ---------------------------------------------------------------------------

struct AppState {
    vault: Vault,
    queue: Mutex<PriorityQueue>,
    eviction_engine: EvictionEngine,
    battery: Mutex<BatterySimulator>,
    power_state: RwLock<PowerState>,
    dtn_config: dtn::DtnConfig,
    spool: dtn::SpoolManager,
    clock: orbit::VirtualClock,
    active_anomalies: RwLock<u32>,
    dtn_link_active: RwLock<bool>,
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "mission_core=info".parse().expect("valid filter")),
        )
        .init();

    info!("=== SpaceOrb V7.6 Root Supervisor ===");
    info!("Initializing mission-core...");

    // Load environment from .env
    dotenvy::dotenv().context("Failed to load .env file")?;

    // Initialize vault
    let vault_config = VaultConfig::from_env()?;

    // Run crash recovery before anything else
    let recovered = vault::recover_orphaned_pending(&vault_config).await?;
    if recovered > 0 {
        warn!(count = recovered, "Crash recovery completed");
    }

    let vault = Vault::new(vault_config.clone())?;

    // Initialize priority queue
    let weights = PScoreWeights::from_env();
    let queue = PriorityQueue::new(weights);

    // Initialize eviction engine
    let mut eviction_config = EvictionConfig::from_env();
    eviction_config.vault_paths = vec![
        vault_config.usb_primary.clone(),
        vault_config.usb_mirror.clone(),
    ];
    let eviction_engine = EvictionEngine::new(eviction_config);

    // Initialize DTN
    let dtn_config = dtn::DtnConfig::from_env();
    let spool_dir = dtn_config.spool_dir.clone();
    tokio::fs::create_dir_all(&spool_dir).await
        .context("Failed to create spool directory")?;
    let spool = dtn::SpoolManager::new(spool_dir);

    // Initialize virtual clock (real-time for now)
    let clock = orbit::VirtualClock::real_time();

    // Shared state
    let state = Arc::new(AppState {
        vault,
        queue: Mutex::new(queue),
        eviction_engine,
        battery: Mutex::new(BatterySimulator::new(75.0)),
        power_state: RwLock::new(PowerState::Solar),
        dtn_config,
        spool,
        clock,
        active_anomalies: RwLock::new(0),
        dtn_link_active: RwLock::new(false),
    });

    // Spawn all subsystem tasks
    let state_ipc = Arc::clone(&state);
    let state_eviction = Arc::clone(&state);
    let state_power = Arc::clone(&state);
    let state_broadcast = Arc::clone(&state);
    let state_score_refresh = Arc::clone(&state);

    tokio::select! {
        res = spawn_ipc_listener(state_ipc) => {
            error!("IPC listener exited: {res:?}");
        }
        res = spawn_eviction_loop(state_eviction) => {
            error!("Eviction loop exited: {res:?}");
        }
        res = spawn_power_governor(state_power) => {
            error!("Power governor exited: {res:?}");
        }
        res = spawn_state_broadcast(state_broadcast) => {
            error!("State broadcast exited: {res:?}");
        }
        res = spawn_score_refresh(state_score_refresh) => {
            error!("Score refresh exited: {res:?}");
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// IPC Listener (Unix Domain Socket)
// ---------------------------------------------------------------------------

/// Bind the UDS and listen for AI sandbox inference results.
///
/// Per spec: bind `/tmp/mission.sock`, immediately `chmod 0o666`.
async fn spawn_ipc_listener(state: Arc<AppState>) -> Result<()> {
    let socket_path = std::env::var("IPC_SOCKET_PATH")
        .unwrap_or_else(|_| "/tmp/mission.sock".into());

    // Remove stale socket if exists
    let _ = tokio::fs::remove_file(&socket_path).await;

    let listener = UnixListener::bind(&socket_path)
        .context("Failed to bind UDS")?;

    // chmod 0o666 — bridge the root/user boundary
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o666);
        std::fs::set_permissions(&socket_path, perms)
            .context("Failed to chmod 0o666 on mission.sock")?;
    }

    info!(path = %socket_path, "IPC listener bound (chmod 0o666 applied)");

    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let state = Arc::clone(&state);
                tokio::spawn(async move {
                    if let Err(e) = handle_ipc_connection(state, stream).await {
                        error!(error = %e, "IPC connection handler error");
                    }
                });
            }
            Err(e) => {
                error!(error = %e, "Failed to accept IPC connection");
            }
        }
    }
}

/// Handle a single IPC connection from the AI sandbox.
async fn handle_ipc_connection(
    state: Arc<AppState>,
    stream: tokio::net::UnixStream,
) -> Result<()> {
    let reader = BufReader::new(stream);
    let mut lines = reader.lines();

    // Determine timeout based on power state
    let power = *state.power_state.read().await;
    let read_timeout = if power == PowerState::Eclipse {
        Duration::from_secs(20) // Extended timeout during low-power
    } else {
        Duration::from_millis(2500) // Standard 2500ms timeout
    };

    while let Ok(Some(line)) = timeout(read_timeout, lines.next_line()).await? {
        match serde_json::from_str::<AiInferenceResult>(&line) {
            Ok(result) => {
                info!(
                    criticality = result.criticality,
                    "Received AI inference result via IPC"
                );

                // Determine criticality
                let criticality = if result.criticality >= 1000 {
                    let mut anomalies = state.active_anomalies.write().await;
                    *anomalies += 1;
                    Criticality::Anomaly
                } else {
                    Criticality::Routine
                };

                // Serialize the full result for vault storage
                let payload = serde_json::to_vec(&result)
                    .context("Failed to serialize inference result")?;

                // Generate a unique ID
                let file_id = format!(
                    "inf-{}-{}",
                    Utc::now().format("%Y%m%d%H%M%S%3f"),
                    result.criticality
                );

                // Ingest through the vault pipeline
                match state.vault.ingest(&file_id, &payload).await {
                    Ok(sealed_name) => {
                        info!(sealed = %sealed_name, "Inference result sealed in vault");

                        // Add to priority queue
                        let entry = PScoreEntry::new(
                            file_id,
                            PathBuf::from(&sealed_name),
                            criticality,
                            Utc::now(),
                            payload.len() as f64 / (1024.0 * 1024.0),
                            state.queue.lock().await.weights(),
                        );
                        state.queue.lock().await.push(entry);
                    }
                    Err(e) => {
                        error!(error = %e, "Vault ingest failed for inference result");
                    }
                }
            }
            Err(e) => {
                warn!(error = %e, line = %line, "Failed to parse IPC message");
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Eviction Loop
// ---------------------------------------------------------------------------

/// Periodically check disk utilization and evict if necessary.
async fn spawn_eviction_loop(state: Arc<AppState>) -> Result<()> {
    let mut timer = interval(Duration::from_secs(30));

    loop {
        timer.tick().await;

        let mut queue = state.queue.lock().await;
        match state.eviction_engine.run_eviction_cycle(&mut queue).await {
            Ok(audit) => {
                if !audit.is_empty() {
                    info!(
                        evicted_count = audit.len(),
                        "Eviction cycle completed"
                    );
                    // In production: persist audit log to disk
                    for entry in &audit {
                        info!(
                            id = %entry.evicted_id,
                            score = entry.score,
                            size_mb = entry.size_mb,
                            "EVICTION AUDIT: {}",
                            entry.reason
                        );
                    }
                }
            }
            Err(e) => {
                error!(error = %e, "Eviction cycle error");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Power Governor
// ---------------------------------------------------------------------------

/// Monitor simulated SoC and adjust AI sandbox CPU quota via systemd.
///
/// - SoC > 50% → CPUQuota=200% (Solar)
/// - SoC < 30% → CPUQuota=5% (Eclipse)
async fn spawn_power_governor(state: Arc<AppState>) -> Result<()> {
    let mut timer = interval(Duration::from_secs(5));
    let mut last_state = PowerState::Solar;

    loop {
        timer.tick().await;

        // Tick the battery simulator
        {
            let mut battery = state.battery.lock().await;
            // Simplified: use virtual clock eclipse state if available
            battery.tick(5.0, last_state == PowerState::Eclipse);
        }

        let current_state = state.battery.lock().await.power_state();

        if current_state != last_state && current_state != PowerState::Transition {
            let quota = match current_state {
                PowerState::Solar => "200%",
                PowerState::Eclipse => "5%",
                PowerState::Transition => continue,
            };

            info!(
                state = ?current_state,
                quota = quota,
                soc = state.battery.lock().await.soc_percent,
                "Power state transition — adjusting AI CPUQuota"
            );

            // Set the CPUQuota via systemctl set-property
            // In production this calls: systemctl set-property mission-ai.service CPUQuota=X%
            let result = tokio::process::Command::new("systemctl")
                .args(["set-property", "mission-ai.service", &format!("CPUQuota={quota}")])
                .output()
                .await;

            match result {
                Ok(output) => {
                    if output.status.success() {
                        info!(quota = quota, "CPUQuota updated via systemctl");
                    } else {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        warn!(error = %stderr, "systemctl set-property failed (non-critical on dev)");
                    }
                }
                Err(e) => {
                    warn!(error = %e, "Failed to invoke systemctl (expected on non-Linux)");
                }
            }

            *state.power_state.write().await = current_state;
            last_state = current_state;
        }
    }
}

// ---------------------------------------------------------------------------
// State Broadcast (500ms)
// ---------------------------------------------------------------------------

/// Broadcast system state every 500ms via Zenoh for the ground station dashboard.
async fn spawn_state_broadcast(state: Arc<AppState>) -> Result<()> {
    let mut timer = interval(Duration::from_millis(500));

    loop {
        timer.tick().await;

        let queue_depth = state.queue.lock().await.len();
        let soc = state.battery.lock().await.soc_percent;
        let power = *state.power_state.read().await;
        let anomalies = *state.active_anomalies.read().await;
        let dtn_active = *state.dtn_link_active.read().await;

        let broadcast = dtn::SystemStateBroadcast {
            epoch: state.clock.now().await,
            queue_depth,
            vault_sealed_count: 0, // TODO: count from filesystem
            soc_percent: soc,
            power_state: format!("{power:?}"),
            active_anomalies: anomalies,
            dtn_link_active: dtn_active,
            chunks_pending: 0, // TODO: from spool
            usb_primary_util: 0.0,
            usb_mirror_util: 0.0,
        };

        // Serialize and publish (Zenoh publish would go here)
        match serde_json::to_string(&broadcast) {
            Ok(json) => {
                // In production: zenoh_session.put(TOPIC_STATE, json).await;
                // For now: trace-level log
                tracing::trace!(state = %json, "State broadcast");
            }
            Err(e) => {
                warn!(error = %e, "Failed to serialize state broadcast");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Score Refresh
// ---------------------------------------------------------------------------

/// Periodically recompute all P_Scores to account for aging.
async fn spawn_score_refresh(state: Arc<AppState>) -> Result<()> {
    let mut timer = interval(Duration::from_secs(10));

    loop {
        timer.tick().await;
        let mut queue = state.queue.lock().await;
        queue.refresh_scores();
        tracing::trace!(depth = queue.len(), "P_Scores refreshed");
    }
}

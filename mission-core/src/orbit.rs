//! SpaceOrb V7.6 — Virtual Epoch Clock & SGP4 Orbital Propagation
//!
//! Decouples simulation time from hardware RTC.
//! Provides SGP4 position/velocity state vectors from TLE data.
//!
//! Reference: SPACEORB_CORE_SPEC.txt §5 (orbit.rs)

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use sgp4::{Constants, Elements};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, instrument, warn};

// ---------------------------------------------------------------------------
// Virtual Clock
// ---------------------------------------------------------------------------

/// A decoupled virtual epoch clock that never touches the hardware RTC.
///
/// Supports:
/// - Real-time (1:1) tracking
/// - Fast-forward / time-warp for simulation
/// - Instant jump to arbitrary epoch
#[derive(Debug, Clone)]
pub struct VirtualClock {
    inner: Arc<RwLock<VirtualClockState>>,
}

#[derive(Debug)]
struct VirtualClockState {
    /// The virtual "current" time.
    epoch: DateTime<Utc>,
    /// Wall-clock anchor (when the clock was last set/synced).
    anchor_wall: DateTime<Utc>,
    /// Time warp factor: 1.0 = real-time, 100.0 = 100x fast-forward.
    warp_factor: f64,
    /// Whether the clock is running (vs paused).
    running: bool,
}

impl VirtualClock {
    /// Create a new virtual clock starting at the given epoch.
    pub fn new(start_epoch: DateTime<Utc>) -> Self {
        Self {
            inner: Arc::new(RwLock::new(VirtualClockState {
                epoch: start_epoch,
                anchor_wall: Utc::now(),
                warp_factor: 1.0,
                running: true,
            })),
        }
    }

    /// Create a clock starting at the current wall-clock time (1:1 real-time).
    pub fn real_time() -> Self {
        Self::new(Utc::now())
    }

    /// Get the current virtual epoch timestamp.
    pub async fn now(&self) -> DateTime<Utc> {
        let state = self.inner.read().await;
        if !state.running {
            return state.epoch;
        }
        let wall_elapsed = Utc::now()
            .signed_duration_since(state.anchor_wall)
            .num_milliseconds() as f64;
        let virtual_elapsed_ms = wall_elapsed * state.warp_factor;
        state.epoch + Duration::milliseconds(virtual_elapsed_ms as i64)
    }

    /// Set the time warp factor.
    ///
    /// `factor = 1.0` is real-time. `factor = 1000.0` is 1000x fast-forward.
    pub async fn set_warp_factor(&self, factor: f64) -> Result<()> {
        anyhow::ensure!(factor > 0.0, "Warp factor must be positive, got {factor}");
        let mut state = self.inner.write().await;
        // Re-anchor to preserve continuity
        let current = self.compute_now(&state);
        state.epoch = current;
        state.anchor_wall = Utc::now();
        state.warp_factor = factor;
        info!(warp_factor = factor, "Virtual clock warp factor updated");
        Ok(())
    }

    /// Jump the virtual clock to an arbitrary epoch.
    pub async fn jump_to(&self, epoch: DateTime<Utc>) {
        let mut state = self.inner.write().await;
        state.epoch = epoch;
        state.anchor_wall = Utc::now();
        info!(epoch = %epoch, "Virtual clock jumped to new epoch");
    }

    /// Pause the virtual clock.
    pub async fn pause(&self) {
        let mut state = self.inner.write().await;
        state.epoch = self.compute_now(&state);
        state.running = false;
        info!("Virtual clock paused");
    }

    /// Resume the virtual clock.
    pub async fn resume(&self) {
        let mut state = self.inner.write().await;
        state.anchor_wall = Utc::now();
        state.running = true;
        info!("Virtual clock resumed");
    }

    /// Sync the virtual clock to UTC (re-anchor to wall clock, warp = 1.0).
    pub async fn sync_to_utc(&self) {
        let mut state = self.inner.write().await;
        state.epoch = Utc::now();
        state.anchor_wall = Utc::now();
        state.warp_factor = 1.0;
        state.running = true;
        info!("Virtual clock synced to UTC (live tracking mode)");
    }

    /// Internal: compute the current virtual time without acquiring the lock.
    fn compute_now(&self, state: &VirtualClockState) -> DateTime<Utc> {
        if !state.running {
            return state.epoch;
        }
        let wall_elapsed = Utc::now()
            .signed_duration_since(state.anchor_wall)
            .num_milliseconds() as f64;
        let virtual_elapsed_ms = wall_elapsed * state.warp_factor;
        state.epoch + Duration::milliseconds(virtual_elapsed_ms as i64)
    }
}

// ---------------------------------------------------------------------------
// SGP4 Orbital Propagation
// ---------------------------------------------------------------------------

/// Position and velocity state vector in TEME frame.
#[derive(Debug, Clone, Serialize)]
pub struct OrbitalState {
    /// Virtual epoch at which this state was computed.
    pub epoch: DateTime<Utc>,
    /// Position vector [km] (x, y, z) in TEME.
    pub position_km: [f64; 3],
    /// Velocity vector [km/s] (vx, vy, vz) in TEME.
    pub velocity_km_s: [f64; 3],
    /// Orbital period (minutes).
    pub period_minutes: f64,
    /// Whether the satellite is in eclipse (simplified: z < 0 heuristic).
    pub in_eclipse: bool,
}

/// SGP4 orbital propagator initialized from Two-Line Element (TLE) data.
pub struct OrbitalPropagator {
    /// Parsed TLE elements.
    elements: Elements,
    /// SGP4 constants (computed from elements).
    constants: Constants,
    /// Reference to the virtual clock.
    clock: VirtualClock,
}

impl OrbitalPropagator {
    /// Initialize the propagator from TLE line strings.
    ///
    /// # Arguments
    /// - `tle_line1`: First line of the TLE set.
    /// - `tle_line2`: Second line of the TLE set.
    /// - `clock`: Virtual clock reference for time.
    pub fn from_tle(tle_line1: &str, tle_line2: &str, clock: VirtualClock) -> Result<Self> {
        let elements = Elements::from_tle(
            None,
            tle_line1.as_bytes(),
            tle_line2.as_bytes(),
        )
        .map_err(|e| anyhow::anyhow!("Failed to parse TLE: {e:?}"))?;

        let constants = Constants::from_elements(&elements)
            .map_err(|e| anyhow::anyhow!("Failed to compute SGP4 constants: {e:?}"))?;

        info!(
            norad_id = elements.norad_id,
            "SGP4 propagator initialized from TLE"
        );

        Ok(Self {
            elements,
            constants,
            clock,
        })
    }

    /// Propagate to the current virtual epoch and return the orbital state.
    #[instrument(skip(self))]
    pub async fn propagate(&self) -> Result<OrbitalState> {
        let virtual_now = self.clock.now().await;

        // Compute minutes since TLE epoch
        let tle_epoch = self.tle_epoch_as_datetime()?;
        let elapsed_minutes = virtual_now
            .signed_duration_since(tle_epoch)
            .num_milliseconds() as f64
            / 60_000.0;

        let prediction = self
            .constants
            .propagate(elapsed_minutes)
            .map_err(|e| anyhow::anyhow!("SGP4 propagation failed: {e:?}"))?;

        let position_km = [
            prediction.position[0],
            prediction.position[1],
            prediction.position[2],
        ];
        let velocity_km_s = [
            prediction.velocity[0],
            prediction.velocity[1],
            prediction.velocity[2],
        ];

        // Simplified eclipse detection: if the satellite's z-component
        // in TEME is negative and altitude is within Earth's shadow cone.
        let altitude_km =
            (position_km[0].powi(2) + position_km[1].powi(2) + position_km[2].powi(2)).sqrt()
                - 6371.0; // Earth radius
        let in_eclipse = position_km[2] < 0.0 && altitude_km < 2000.0;

        // Orbital period from mean motion (rev/day → minutes)
        let mean_motion = self.elements.mean_motion;
        let period_minutes = if mean_motion > 0.0 {
            1440.0 / mean_motion
        } else {
            0.0
        };

        debug!(
            epoch = %virtual_now,
            elapsed_min = elapsed_minutes,
            alt_km = altitude_km,
            in_eclipse = in_eclipse,
            "SGP4 propagation complete"
        );

        Ok(OrbitalState {
            epoch: virtual_now,
            position_km,
            velocity_km_s,
            period_minutes,
            in_eclipse,
        })
    }

    /// Convert the TLE epoch to a UTC `DateTime`.
    fn tle_epoch_as_datetime(&self) -> Result<DateTime<Utc>> {
        let year = if self.elements.epoch_afspc_compatibility_mode {
            // AFSPC mode: 2-digit year
            let y = self.elements.datetime.year();
            if y < 57 { 2000 + y } else { 1900 + y }
        } else {
            self.elements.datetime.year() as i32
        };

        // The sgp4 crate's elements.datetime gives us the epoch directly
        // We reconstruct it from the crate's representation.
        use chrono::NaiveDate;
        let month = self.elements.datetime.month() as u32;
        let day = self.elements.datetime.day() as u32;
        let hour = self.elements.datetime.hour() as u32;
        let minute = self.elements.datetime.minute() as u32;
        let second = self.elements.datetime.second() as u32;
        let nanos = self.elements.datetime.nanosecond();

        let naive = NaiveDate::from_ymd_opt(year, month, day)
            .and_then(|d| d.and_hms_nano_opt(hour, minute, second, nanos))
            .context("Invalid TLE epoch date components")?;

        Ok(DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc))
    }

    /// Get the virtual clock reference.
    pub fn clock(&self) -> &VirtualClock {
        &self.clock
    }
}

// ---------------------------------------------------------------------------
// ISS Default TLE (for demo/exhibition use)
// ---------------------------------------------------------------------------

/// Default ISS TLE for exhibition demonstrations.
pub const ISS_TLE_LINE1: &str =
    "1 25544U 98067A   24001.50000000  .00016717  00000-0  10270-3 0  9003";
pub const ISS_TLE_LINE2: &str =
    "2 25544  51.6400 208.9163 0006703 300.3486  59.7145 15.49560532484199";

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_virtual_clock_real_time() {
        let clock = VirtualClock::real_time();
        let t1 = clock.now().await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let t2 = clock.now().await;
        assert!(t2 > t1, "Virtual clock should advance in real-time");
    }

    #[tokio::test]
    async fn test_virtual_clock_warp() {
        let clock = VirtualClock::real_time();
        clock.set_warp_factor(100.0).await.expect("set warp");
        let t1 = clock.now().await;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let t2 = clock.now().await;
        let elapsed = t2.signed_duration_since(t1).num_seconds();
        // At 100x warp, 100ms wall time → ~10s virtual time
        assert!(elapsed >= 5, "Warped clock should advance ~10s, got {elapsed}s");
    }

    #[tokio::test]
    async fn test_virtual_clock_pause_resume() {
        let clock = VirtualClock::real_time();
        clock.pause().await;
        let t1 = clock.now().await;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let t2 = clock.now().await;
        assert_eq!(t1, t2, "Paused clock should not advance");

        clock.resume().await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let t3 = clock.now().await;
        assert!(t3 > t2, "Resumed clock should advance");
    }
}

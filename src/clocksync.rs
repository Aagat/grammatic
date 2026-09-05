//! Scale clock maintenance.
//!
//! The OpenScale app normally keeps the scale's clock right; since we drop
//! the app, this module takes the job — in one place. [`ClockSync`] absorbs
//! the whole policy: how wrong the scale clock must look to warrant a sync
//! (drift threshold, or receiver-clocked frames), the idle window (never
//! interrupt a weigh-in), and the retry backoff. How the clock actually gets
//! written sits behind the [`ClockWriter`] seam — `GattClockWriter` in
//! production, a recording fake in tests.

use std::future::Future;
use std::time::{Duration, Instant};

use anyhow::Context;
use bluer::{Address, Uuid};
use chrono::Local;
use tracing::{info, warn};

use crate::capture::CaptureResult;
use crate::parser::current_time_payload;

pub const CURRENT_TIME_SERVICE_UUID: Uuid = uuid::uuid!("00001805-0000-1000-8000-00805f9b34fb");
pub const CURRENT_TIME_CHARACTERISTIC_UUID: Uuid =
    uuid::uuid!("00002a2b-0000-1000-8000-00805f9b34fb");

const SCALE_IDLE_WINDOW: Duration = Duration::from_secs(30);
const INITIAL_BACKOFF: Duration = Duration::from_secs(5);
const MAX_BACKOFF: Duration = Duration::from_secs(5 * 60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// How the scale's clock actually gets set — the seam under clock
/// maintenance. Two adapters: GATT in production, a recording fake in tests.
pub trait ClockWriter {
    /// Write the 10-byte Current Time payload to the scale.
    fn write_time(&self, payload: [u8; 10]) -> impl Future<Output = anyhow::Result<()>> + Send;
}

/// Production adapter: GATT connect, write with response (like the OpenScale
/// app) with a fallback to a plain write command, then disconnect.
pub struct GattClockWriter {
    target: Address,
}

impl GattClockWriter {
    pub fn new(target: Address) -> GattClockWriter {
        GattClockWriter { target }
    }
}

impl ClockWriter for GattClockWriter {
    async fn write_time(&self, payload: [u8; 10]) -> anyhow::Result<()> {
        let (_session, adapter) = crate::ble::default_adapter().await?;
        let device = crate::ble::find_device(&adapter, self.target).await?;

        tokio::time::timeout(CONNECT_TIMEOUT, device.connect())
            .await
            .map_err(|_| anyhow::anyhow!("connecting to the scale timed out"))?
            .context("connecting to the scale")?;
        info!("connected to scale; writing current time");

        let mut written = false;
        let mut error = None;
        for service in device.services().await? {
            if service.uuid().await? != CURRENT_TIME_SERVICE_UUID {
                continue;
            }
            for characteristic in service.characteristics().await? {
                if characteristic.uuid().await? != CURRENT_TIME_CHARACTERISTIC_UUID {
                    continue;
                }
                // Write with response (like the OpenScale app); fall back to a
                // write command if the scale rejects request-style writes.
                let request = bluer::gatt::remote::CharacteristicWriteRequest {
                    op_type: bluer::gatt::WriteOp::Request,
                    ..Default::default()
                };
                match characteristic.write_ext(&payload, &request).await {
                    Ok(()) => written = true,
                    Err(request_error) => match characteristic.write(&payload).await {
                        Ok(()) => written = true,
                        Err(command_error) => {
                            error = Some(anyhow::Error::new(request_error).context(command_error));
                        }
                    },
                }
            }
        }
        let _ = device.disconnect().await;
        if let Some(error) = error {
            return Err(error.context("writing to the Current Time characteristic"));
        }
        anyhow::ensure!(
            written,
            "scale exposes no writable Current Time characteristic (0x2A2B)"
        );
        Ok(())
    }
}

/// Pure state machine for when a clock sync may run: idle window + backoff.
#[derive(Debug)]
struct ClockMonitor {
    last_frame: Option<Instant>,
    backoff: Duration,
    next_attempt: Instant,
}

impl Default for ClockMonitor {
    fn default() -> Self {
        ClockMonitor {
            last_frame: None,
            backoff: INITIAL_BACKOFF,
            next_attempt: Instant::now(),
        }
    }
}

impl ClockMonitor {
    fn note_frame(&mut self, now: Instant) {
        self.last_frame = Some(now);
    }

    /// A sync may run only when the scale has been quiet for the idle window
    /// (don't interrupt a weigh-in) and the backoff has elapsed.
    fn ready(&self, now: Instant) -> bool {
        let idle = self
            .last_frame
            .is_none_or(|frame| now.duration_since(frame) >= SCALE_IDLE_WINDOW);
        idle && now >= self.next_attempt
    }

    fn note_attempt(&mut self, success: bool, now: Instant) {
        if success {
            self.backoff = INITIAL_BACKOFF;
        } else {
            self.backoff = (self.backoff * 2).min(MAX_BACKOFF);
        }
        self.next_attempt = now + self.backoff;
    }
}

/// Scale clock maintenance in one module.
///
/// Call [`ClockSync::on_frame`] for every received frame (stabilized or not)
/// so the idle window reflects a weigh-in in progress, and
/// [`ClockSync::on_capture`] for each stabilized outcome — it writes the
/// clock through the adapter when the scale has been idle long enough.
pub struct ClockSync<W: ClockWriter> {
    enabled: bool,
    drift_threshold_secs: i64,
    monitor: ClockMonitor,
    writer: W,
}

impl<W: ClockWriter> ClockSync<W> {
    pub fn new(enabled: bool, drift_threshold_secs: i64, writer: W) -> ClockSync<W> {
        ClockSync {
            enabled,
            drift_threshold_secs,
            monitor: ClockMonitor::default(),
            writer,
        }
    }

    /// The scale was heard — keeps the idle window honest.
    pub fn on_frame(&mut self, now: Instant) {
        self.monitor.note_frame(now);
    }

    /// Evaluate one stabilized measurement. When the scale clock looks wrong
    /// and the scale is idle and the backoff has elapsed, write the current
    /// time through the adapter and apply the backoff.
    pub async fn on_capture(&mut self, result: &CaptureResult, now: Instant) {
        if !self.enabled {
            return;
        }
        let needs_sync = match result.clock_drift {
            Some(drift) => drift.abs() > self.drift_threshold_secs,
            // Receiver-clocked frame: the scale clock is unset or implausible.
            None => true,
        };
        if !needs_sync {
            return;
        }
        info!(
            "scale clock looks off (drift {:?}); will synchronize when idle",
            result
                .clock_drift
                .map(|s| format!("{s}s"))
                .unwrap_or_else(|| "implausible".into())
        );
        if !self.monitor.ready(now) {
            return;
        }
        match self
            .writer
            .write_time(current_time_payload(Local::now().naive_local()))
            .await
        {
            Ok(()) => {
                info!("scale clock synchronized");
                self.monitor.note_attempt(true, now);
            }
            Err(error) => {
                warn!("clock sync failed: {error:#}");
                self.monitor.note_attempt(false, now);
            }
        }
    }
}

/// Set the scale's clock to this machine's local time (the `sync-clock`
/// command; the listen loop goes through [`ClockSync`] instead).
pub async fn set_scale_clock(target: Address) -> anyhow::Result<()> {
    GattClockWriter::new(target)
        .write_time(current_time_payload(Local::now().naive_local()))
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::sync::{Arc, Mutex};

    // ── the recording fake: the adapter that makes the seam real ──

    struct FakeWriter {
        fail: bool,
        writes: Arc<Mutex<Vec<[u8; 10]>>>,
    }

    impl FakeWriter {
        fn new(fail: bool) -> (FakeWriter, Arc<Mutex<Vec<[u8; 10]>>>) {
            let writes = Arc::new(Mutex::new(Vec::new()));
            (
                FakeWriter {
                    fail,
                    writes: writes.clone(),
                },
                writes,
            )
        }
    }

    impl ClockWriter for FakeWriter {
        async fn write_time(&self, payload: [u8; 10]) -> anyhow::Result<()> {
            self.writes.lock().unwrap().push(payload);
            if self.fail {
                Err(anyhow::anyhow!("gatt down"))
            } else {
                Ok(())
            }
        }
    }

    // ── helpers ──

    fn captured(clock_drift: Option<i64>) -> CaptureResult {
        CaptureResult {
            measurement: crate::parser::Measurement {
                weight_kg: 75.0,
                impedance_ohm: Some(500),
                stabilized: true,
                unit: crate::parser::Unit::Kg,
                timestamp: None,
            },
            measured_at: Local.with_ymd_and_hms(2026, 9, 3, 12, 46, 0).unwrap(),
            recorded: true,
            clock_drift,
        }
    }

    fn idle_at(t0: Instant) -> Instant {
        t0 + SCALE_IDLE_WINDOW
    }

    // ── the policy, driven through ClockSync's interface ──

    #[tokio::test]
    async fn syncs_when_drifted_and_idle() {
        let (writer, writes) = FakeWriter::new(false);
        let mut sync = ClockSync::new(true, 120, writer);
        let t0 = Instant::now();

        sync.on_frame(t0);
        // Mid-weigh-in (10 s quiet): drifted, but not idle — no sync.
        sync.on_capture(&captured(Some(180)), t0 + Duration::from_secs(10))
            .await;
        assert!(writes.lock().unwrap().is_empty());

        // Idle window elapsed: the write goes through.
        sync.on_capture(&captured(Some(180)), idle_at(t0)).await;
        assert_eq!(writes.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn drift_within_threshold_never_syncs() {
        let (writer, writes) = FakeWriter::new(false);
        let mut sync = ClockSync::new(true, 120, writer);
        sync.on_capture(&captured(Some(60)), Instant::now()).await;
        assert!(writes.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn receiver_clocked_frames_always_need_sync() {
        let (writer, writes) = FakeWriter::new(false);
        let mut sync = ClockSync::new(true, 120, writer);
        sync.on_capture(&captured(None), Instant::now()).await;
        assert_eq!(writes.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn disabled_never_syncs() {
        let (writer, writes) = FakeWriter::new(false);
        let mut sync = ClockSync::new(false, 120, writer);
        sync.on_capture(&captured(Some(999_999)), Instant::now()).await;
        assert!(writes.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn failures_back_off() {
        let (writer, writes) = FakeWriter::new(true);
        let mut sync = ClockSync::new(true, 120, writer);
        let t0 = Instant::now();

        sync.on_frame(t0);
        // First attempt fails → backoff doubles to 10 s.
        sync.on_capture(&captured(Some(180)), idle_at(t0)).await;
        assert_eq!(writes.lock().unwrap().len(), 1);

        // Still within the 10 s backoff: no second write.
        sync.on_capture(&captured(Some(180)), idle_at(t0) + Duration::from_secs(9))
            .await;
        assert_eq!(writes.lock().unwrap().len(), 1);

        // Backoff elapsed → second attempt, also failing → backoff 20 s.
        sync.on_capture(
            &captured(Some(180)),
            idle_at(t0) + INITIAL_BACKOFF * 2 + Duration::from_secs(1),
        )
        .await;
        assert_eq!(writes.lock().unwrap().len(), 2);
    }

    // ── the internal state machine ──

    #[test]
    fn not_ready_until_idle_window_passes() {
        let mut monitor = ClockMonitor::default();
        let t0 = Instant::now();
        monitor.note_frame(t0);
        assert!(!monitor.ready(t0 + Duration::from_secs(10)));
        assert!(monitor.ready(t0 + SCALE_IDLE_WINDOW));
    }

    #[test]
    fn not_ready_without_frames_once_idle() {
        // No frames at all: idle condition holds trivially.
        let monitor = ClockMonitor::default();
        assert!(monitor.ready(Instant::now()));
    }

    #[test]
    fn backoff_doubles_and_caps() {
        let mut monitor = ClockMonitor::default();
        let t0 = Instant::now();
        // First failure: 5 s -> 10 s.
        monitor.note_attempt(false, t0);
        assert!(!monitor.ready(t0 + Duration::from_secs(9)));
        assert!(monitor.ready(t0 + Duration::from_secs(11)));

        // Second failure: 10 s -> 20 s.
        monitor.note_attempt(false, t0);
        assert!(!monitor.ready(t0 + Duration::from_secs(19)));
        assert!(monitor.ready(t0 + Duration::from_secs(21)));

        for _ in 0..20 {
            monitor.note_attempt(false, t0);
        }
        assert_eq!(monitor.backoff, MAX_BACKOFF);
        assert!(!monitor.ready(t0 + MAX_BACKOFF - Duration::from_secs(1)));
        assert!(monitor.ready(t0 + MAX_BACKOFF + Duration::from_secs(1)));
    }

    #[test]
    fn success_resets_backoff() {
        let mut monitor = ClockMonitor::default();
        let t0 = Instant::now();
        for _ in 0..10 {
            monitor.note_attempt(false, t0);
        }
        monitor.note_attempt(true, t0);
        assert_eq!(monitor.backoff, INITIAL_BACKOFF);
        assert!(monitor.ready(t0 + INITIAL_BACKOFF));
    }
}

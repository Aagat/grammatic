//! GATT history pull: recovering missed stabilized frames from the scale.
//!
//! The scale stores each finished weigh-in and exposes it over the Body
//! Composition History characteristic (`0x2A2F`, WRITE + NOTIFY, under service
//! `0x181B`). When live frames were seen but the stabilized frame never
//! arrived (agent restarted mid-weigh-in, advertisement missed), a history
//! pull asks the scale for its stored entries and feeds each through the
//! normal capture path. History entries are stabilized records by
//! construction, so the history-only invariant holds: a pull that yields
//! nothing records nothing — live frames are never synthesized into
//! measurements.
//!
//! Protocol (spike-confirmed live on the XMTZC05HM 2026-09-03; sources:
//! wiecosystem/Bluetooth `huami.health.scale2.md`, OpenScale wiki
//! "Xiaomi Bluetooth Mi Scale" for the v1 `0x2A2F` sequence):
//!
//! ```text
//! client → scale: 0x01 + u32 device-id   (session start / size probe)
//! scale → client: 0x01 hi lo + id-echo   (pending-entry count LE in
//!                                          bytes 1-2; bytes 3-6 echo the
//!                                          probe's device-id)
//! client → scale: 0x02                    (fetch stored entries)
//! scale → client: 13-byte entries         (byte-identical frames — the
//!                                          same layout as the advertisements,
//!                                          seconds in byte 8)
//! scale → client: 0x03                    (stop / end of history)
//! client → scale: 0x03                    (confirm stop)
//! client → scale: 0x04 + u32 device-id   (ack; advances the scale's
//!                                          per-device position)
//! ```
//!
//! The `u32` device-id is client-chosen (we persist a random one on disk).
//! The scale tracks the history position per device-id, so repeated pulls by
//! the same id only re-send new entries; re-delivery is harmless anyway
//! (dedup collapses it). The v1 form is `0x01 FF FF FF FF` with the
//! same `0x02` / `0x03` / `0x04 FF FF FF FF` framing.
//!
//! Layout: history entries are byte-identical 13-byte frames (two control
//! bytes, clock at minute resolution + seconds in byte 8, impedance,
//! weight). See [`crate::parser::parse_history_entry`]. (Reverse-engineering
//! docs describe a 12-byte variant without the first control byte; this
//! firmware does not send it.)
//!
//! GATT access mirrors [`crate::clocksync::ClockWriter`]: the actual radio
//! work sits behind the [`HistoryReader`] seam (production GATT impl +
//! recording fake in tests), and callers serialize history pulls against
//! clock writes — one GATT session at a time, never during a weigh-in.

use std::future::Future;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use bluer::{Address, Device};
use futures::StreamExt;
use tracing::{debug, info, warn};

use crate::parser::{BODY_COMPOSITION_HISTORY_UUID, BODY_COMPOSITION_SERVICE_UUID};

/// Body Composition service (`0x181B`) — also home to the history char.
pub const BODY_COMPOSITION_SERVICE: uuid::Uuid = BODY_COMPOSITION_SERVICE_UUID;
/// History characteristic (`0x2A2F`) — WRITE + NOTIFY.
pub const HISTORY_CHARACTERISTIC: uuid::Uuid = BODY_COMPOSITION_HISTORY_UUID;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// Whole-pull budget: connect + probe + fetch + cleanup, then give up.
const PULL_TIMEOUT: Duration = Duration::from_secs(60);
/// Per-notification wait while fetching entries after `0x02`.
const ENTRY_TIMEOUT: Duration = Duration::from_secs(10);

/// Command bytes on the history characteristic.
const CMD_SIZE_PROBE: u8 = 0x01;
const CMD_FETCH: u8 = 0x02;
const CMD_STOP: u8 = 0x03;
const CMD_ACK: u8 = 0x04;

/// Where the pull gets its stored entries — the seam under history recovery.
///
/// Two adapters: GATT in production, a recording fake in tests. Without it,
/// the fallback session tracker and the `fetch-history` command would need a
/// live scale. Failures are silent w.r.t. capture: the fake and the GATT
/// impl both return errors, and callers log + back off — never spool live
/// frames, never synthesize.
pub trait HistoryReader {
    /// Pull stored history entries (raw 13-byte frame payloads). Returns an
    /// empty vec when the scale has nothing new — not an error.
    fn fetch_history(&self) -> impl Future<Output = anyhow::Result<Vec<Vec<u8>>>> + Send;
}

/// Production adapter: GATT connect → size probe → fetch → ack, disconnect
/// always.
pub struct GattHistoryReader {
    target: Address,
    device_id: u32,
}

impl GattHistoryReader {
    pub fn new(target: Address, device_id: u32) -> GattHistoryReader {
        GattHistoryReader { target, device_id }
    }

    fn probe_command(device_id: u32) -> [u8; 5] {
        let mut cmd = [0u8; 5];
        cmd[0] = CMD_SIZE_PROBE;
        cmd[1..5].copy_from_slice(&device_id.to_le_bytes());
        cmd
    }

    fn ack_command(device_id: u32) -> [u8; 5] {
        let mut cmd = [0u8; 5];
        cmd[0] = CMD_ACK;
        cmd[1..5].copy_from_slice(&device_id.to_le_bytes());
        cmd
    }
}

impl HistoryReader for GattHistoryReader {
    async fn fetch_history(&self) -> anyhow::Result<Vec<Vec<u8>>> {
        tokio::time::timeout(PULL_TIMEOUT, self.pull())
            .await
            .map_err(|_| anyhow::anyhow!("history pull timed out"))?
    }
}

impl GattHistoryReader {
    async fn pull(&self) -> anyhow::Result<Vec<Vec<u8>>> {
        let (_session, adapter) = crate::ble::default_adapter().await?;
        let device = crate::ble::find_device(&adapter, self.target).await?;

        tokio::time::timeout(CONNECT_TIMEOUT, device.connect())
            .await
            .map_err(|_| anyhow::anyhow!("connecting to the scale timed out"))?
            .context("connecting to the scale")?;
        info!("connected to scale; pulling history");

        let result = self.pull_from(&device).await;
        let _ = device.disconnect().await;
        result
    }

    async fn pull_from(&self, device: &Device) -> anyhow::Result<Vec<Vec<u8>>> {
        let characteristic = find_history_characteristic(device).await?;

        // Subscribe before writing anything: the size probe's response and
        // the entries both arrive as notifications. `notify()` issues
        // StartNotify and streams Value changes; dropping the stream issues
        // StopNotify, so hold it for the whole session. The stream is
        // !Unpin, so pin it once and poll via `next()` on the pin.
        let notifications = characteristic.notify().await.context(
            "subscribing to the history characteristic (is the spike-confirmed NOTIFY present?)",
        )?;
        let mut notifications = Box::pin(notifications);

        // Size probe: 0x01 + device-id → 0x01 hi lo ... (count in bytes 1-2).
        characteristic
            .write(&Self::probe_command(self.device_id))
            .await
            .context("writing the history size probe")?;
        let pending = tokio::time::timeout(ENTRY_TIMEOUT, notifications.next())
            .await
            .map_err(|_| anyhow::anyhow!("no response to the history size probe"))?
            .context("history notification stream ended")?;
        let pending = parse_size_response(&pending)?;
        debug!("scale reports {pending} pending history entr(y/ies)");
        if pending == 0 {
            let _ = characteristic.write(&[CMD_STOP]).await;
            return Ok(Vec::new());
        }

        // Fetch: 0x02 → entries, terminated by 0x03.
        characteristic
            .write(&[CMD_FETCH])
            .await
            .context("writing the history fetch command")?;
        let mut entries = Vec::new();
        loop {
            let notification = tokio::time::timeout(ENTRY_TIMEOUT, notifications.next())
                .await
                .map_err(|_| {
                    anyhow::anyhow!("history fetch stalled ({}/{} entries)", entries.len(), pending)
                })?
                .context("history notification stream ended")?;
            match classify_notification(&notification) {
                Notification::Stop => break,
                Notification::Entry(entry) => entries.push(entry),
                Notification::Size => {
                    debug!("ignoring stray size response during fetch");
                }
                Notification::Unknown => {
                    warn!(
                        "ignoring malformed history notification: {}",
                        hex::encode(&notification)
                    );
                }
            }
            if entries.len() >= pending {
                break;
            }
        }

        // Cleanup always: confirm stop, then ack with the same device-id so
        // the scale advances our per-device position. Best-effort — a failed
        // ack only means re-delivery next time, which dedup collapses.
        if let Err(error) = characteristic.write(&[CMD_STOP]).await {
            warn!("history stop confirm failed: {error:#}");
        }
        if let Err(error) = characteristic
            .write(&Self::ack_command(self.device_id))
            .await
        {
            warn!("history ack failed (entries will re-deliver): {error:#}");
        }
        Ok(entries)
    }
}

async fn find_history_characteristic(
    device: &Device,
) -> anyhow::Result<bluer::gatt::remote::Characteristic> {
    for service in device.services().await? {
        if service.uuid().await? != BODY_COMPOSITION_SERVICE {
            continue;
        }
        for characteristic in service.characteristics().await? {
            if characteristic.uuid().await? == HISTORY_CHARACTERISTIC {
                return Ok(characteristic);
            }
        }
    }
    anyhow::bail!(
        "scale exposes no history characteristic (0x2A2F) — is this the XMTZC05HM variant?"
    )
}

/// Pending-entry count from a size-probe response (`0x01 hi lo ...`, with
/// the probe's device-id echoed in bytes 3-6 — spike: `01020078563412`).
/// Errors when the payload is not a size response.
fn parse_size_response(payload: &[u8]) -> anyhow::Result<usize> {
    if payload.len() < 3 || payload[0] != CMD_SIZE_PROBE {
        anyhow::bail!(
            "unexpected history size response: {}",
            hex::encode(payload)
        );
    }
    Ok(u16::from_le_bytes([payload[1], payload[2]]) as usize)
}

enum Notification {
    /// `0x03`: end of history.
    Stop,
    /// A 13-byte stored entry (byte-identical frame).
    Entry(Vec<u8>),
    /// A stray size response (only meaningful right after the probe).
    Size,
    /// Anything else: skip + count, never fail the pull.
    Unknown,
}

fn classify_notification(payload: &[u8]) -> Notification {
    if payload.len() == crate::parser::HISTORY_ENTRY_LEN {
        return Notification::Entry(payload.to_vec());
    }
    if payload.len() == 1 && payload[0] == CMD_STOP {
        return Notification::Stop;
    }
    if !payload.is_empty() && payload[0] == CMD_SIZE_PROBE && payload.len() >= 3 {
        return Notification::Size;
    }
    Notification::Unknown
}

/// Load the persisted per-agent device-id, or mint + store a fresh random
/// one. The id lets the scale track our history position across pulls, so
/// repeated pulls don't re-send old entries. Re-delivery is harmless anyway
/// (dedup collapses it), so a missing/unwritable file only costs noise.
pub fn load_or_create_device_id(path: &std::path::Path) -> anyhow::Result<u32> {
    if let Ok(raw) = std::fs::read(path) {
        // Accept a bare LE u32 (4 bytes) or the hex/decimal text we write.
        if raw.len() == 4 {
            return Ok(u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]));
        }
        let text = String::from_utf8_lossy(&raw).trim().to_string();
        if let Ok(id) = text.parse::<u32>() {
            return Ok(id);
        }
        if let Ok(bytes) = hex::decode(&text)
            && bytes.len() == 4
        {
            return Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]));
        }
    }
    let id = rand_u32();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(path, id.to_string())
        .with_context(|| format!("storing device id in {}", path.display()))?;
    Ok(id)
}

fn rand_u32() -> u32 {
    // No rand dependency: fold time + pid into 32 bits. Uniqueness only
    // needs to be plausible per agent — collisions just re-deliver.
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    std::time::SystemTime::now().hash(&mut hasher);
    std::process::id().hash(&mut hasher);
    std::thread::current().id().hash(&mut hasher);
    (hasher.finish() & 0xFFFF_FFFF) as u32
}

/// Pure state machine for the automatic history fallback in `listen`.
///
/// Arms when ≥1 live frame was observed since the last recorded stabilized
/// measurement; fires when no frame at all has been seen for `quiet_timeout`
/// **and** the scale is idle per the clock-sync monitor — then disarms on
/// any stabilized measurement or on a pull that yields nothing new.
/// Failures back off exponentially (same shape as `ClockMonitor`).
///
/// Critical: idle/quiet detection must use raw advertisement sightings, not
/// filtered frames — [`crate::ble::FrameFilter`] suppresses identical
/// re-broadcasts before the listen loop sees them, so filtered silence while
/// the user stands still is not scale silence. Call [`SessionTracker::note_sighting`]
/// for every advertisement (even filter-suppressed ones) and
/// [`SessionTracker::note_frame`] only for frames that pass the filter.
#[derive(Debug)]
pub struct SessionTracker {
    quiet_timeout: Duration,
    backoff: Duration,
    max_backoff: Duration,
    last_sighting: Option<std::time::Instant>,
    last_fire: Option<std::time::Instant>,
    next_attempt: Option<std::time::Instant>,
    armed: bool,
}

impl SessionTracker {
    pub fn new(quiet_timeout: Duration) -> SessionTracker {
        SessionTracker {
            quiet_timeout,
            backoff: Duration::from_secs(5),
            max_backoff: Duration::from_secs(5 * 60),
            last_sighting: None,
            last_fire: None,
            next_attempt: None,
            armed: false,
        }
    }

    /// Any advertisement from the target scale — including
    /// filter-suppressed re-broadcasts.
    pub fn note_sighting(&mut self, now: std::time::Instant) {
        self.last_sighting = Some(now);
    }

    /// A live frame passed the filter: a weigh-in may be in progress.
    pub fn note_live_frame(&mut self) {
        self.armed = true;
    }

    /// A stabilized measurement was recorded (or collapsed as a
    /// re-broadcast): nothing is missing, stand down.
    pub fn note_stabilized(&mut self) {
        self.armed = false;
        self.last_fire = None;
    }

    /// A pull returned no new entries: the silence was a step-off, not a
    /// missed stabilized frame. Stand down until new live frames arm again.
    pub fn note_empty_pull(&mut self) {
        self.armed = false;
        self.last_fire = None;
    }

    /// A pull recorded ≥1 measurement: stay disarmed (the missing weigh-in
    /// is recovered); new live frames re-arm.
    pub fn note_recovered(&mut self) {
        self.armed = false;
        self.last_fire = None;
    }

    pub fn note_attempt(&mut self, success: bool, now: std::time::Instant) {
        if success {
            self.backoff = Duration::from_secs(5);
        } else {
            self.backoff = (self.backoff * 2).min(self.max_backoff);
        }
        self.next_attempt = Some(now + self.backoff);
    }

    /// Fire only when armed, quiet for the full timeout (on raw sightings),
    /// idle per the clock-sync window, and past the failure backoff.
    /// `clock_idle` is `ClockMonitor`-equivalent readiness, passed in so
    /// this module stays decoupled from clocksync internals.
    pub fn ready(&self, now: std::time::Instant, clock_idle: bool) -> bool {
        if !self.armed || !clock_idle {
            return false;
        }
        let quiet = self
            .last_sighting
            .is_none_or(|sighting| now.duration_since(sighting) >= self.quiet_timeout);
        if !quiet {
            return false;
        }
        if let Some(next) = self.next_attempt
            && now < next
        {
            return false;
        }
        // Fire once per arming: after firing, wait for disarm (stabilized /
        // empty pull) before firing again.
        self.last_fire.is_none()
    }

    /// Mark that a pull is starting (so `ready` won't fire again until the
    /// outcome disarms or re-arms).
    pub fn note_fire(&mut self, now: std::time::Instant) {
        self.last_fire = Some(now);
    }

    /// Clear the single-fire latch after a failed pull so the backoff (not
    /// the latch) gates the retry. Stays armed.
    pub fn last_fire_clear(&mut self) {
        self.last_fire = None;
    }

    pub fn is_armed(&self) -> bool {
        self.armed
    }
}

/// Feed fetched history entries through capture, counting outcomes.
/// Malformed entries are skipped + counted (same pattern as spool replay);
/// DB-outage spooling happens inside capture's record path.
pub async fn record_fetched<S: crate::capture::MeasurementSink>(
    capture: &crate::capture::Capture<S>,
    entries: &[Vec<u8>],
    received_at: chrono::DateTime<chrono::Local>,
    rssi: Option<i16>,
) -> (usize, usize) {
    let mut recorded = 0;
    let mut malformed = 0;
    for entry in entries {
        match capture.handle_history_entry(entry, received_at, rssi).await {
            Some(result) => {
                if result.recorded {
                    recorded += 1;
                }
            }
            None => malformed += 1,
        }
    }
    (recorded, malformed)
}

/// History configuration: `[history]` in `config.toml`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct HistoryConfig {
    /// Master switch for the history seam (parser + `fetch-history` stay
    /// available regardless; this gates the automatic fallback).
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Automatic pull in `listen` when live frames stop with no stabilized
    /// frame. Default on since the 2026-09-03/04 soak validated the
    /// fallback end to end (see ADR-0002).
    #[serde(default = "default_true")]
    pub auto_fetch: bool,
    /// Quiet period (on raw sightings) before the fallback fires. Must stay
    /// strictly longer than the 30 s clock-sync idle window.
    #[serde(default = "default_quiet_timeout_secs")]
    pub quiet_timeout_secs: u64,
    /// Where the per-agent GATT device-id is persisted.
    #[serde(default = "default_device_id_file")]
    pub device_id_file: PathBuf,
}

fn default_true() -> bool {
    true
}

fn default_quiet_timeout_secs() -> u64 {
    75
}

fn default_device_id_file() -> PathBuf {
    PathBuf::from("/var/lib/grammatic/device_id")
}

impl Default for HistoryConfig {
    fn default() -> Self {
        HistoryConfig {
            enabled: default_true(),
            auto_fetch: default_true(),
            quiet_timeout_secs: default_quiet_timeout_secs(),
            device_id_file: default_device_id_file(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── the recording fake: the adapter that makes the seam real ──

    struct FakeReader {
        entries: Vec<Vec<u8>>,
        fail: bool,
        pulls: std::sync::Arc<std::sync::Mutex<u32>>,
    }

    impl FakeReader {
        fn new(entries: Vec<Vec<u8>>, fail: bool) -> (FakeReader, std::sync::Arc<std::sync::Mutex<u32>>) {
            let pulls = std::sync::Arc::new(std::sync::Mutex::new(0));
            (
                FakeReader {
                    entries,
                    fail,
                    pulls: pulls.clone(),
                },
                pulls,
            )
        }
    }

    impl HistoryReader for FakeReader {
        async fn fetch_history(&self) -> anyhow::Result<Vec<Vec<u8>>> {
            *self.pulls.lock().unwrap() += 1;
            if self.fail {
                Err(anyhow::anyhow!("gatt down"))
            } else {
                Ok(self.entries.clone())
            }
        }
    }

    #[tokio::test]
    async fn fake_returns_entries_and_counts_pulls() {
        let (reader, pulls) = FakeReader::new(vec![vec![0u8; 13]], false);
        let entries = reader.fetch_history().await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(*pulls.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn fake_failure_is_an_error_not_entries() {
        let (reader, _) = FakeReader::new(vec![], true);
        assert!(reader.fetch_history().await.is_err());
    }

    // ── protocol framing ──

    #[test]
    fn probe_and_ack_carry_the_device_id_le() {
        let id = 0x1234_5678u32;
        assert_eq!(
            GattHistoryReader::probe_command(id),
            [0x01, 0x78, 0x56, 0x34, 0x12]
        );
        assert_eq!(
            GattHistoryReader::ack_command(id),
            [0x04, 0x78, 0x56, 0x34, 0x12]
        );
    }

    #[test]
    fn size_response_count_is_le() {
        assert_eq!(parse_size_response(&[0x01, 0x03, 0x00]).unwrap(), 3);
        assert_eq!(parse_size_response(&[0x01, 0x00, 0x00]).unwrap(), 0);
        // Live spike trace: count 2 with the probe id echoed.
        assert_eq!(
            parse_size_response(&hex::decode("01020078563412").unwrap()).unwrap(),
            2
        );
    }

    #[test]
    fn size_response_rejects_non_size_payloads() {
        assert!(parse_size_response(&[]).is_err());
        assert!(parse_size_response(&[0x02]).is_err());
        assert!(parse_size_response(&[0x01, 0x02]).is_err());
        // A 13-byte entry starting with 0x01 is length-ambiguous with a
        // size response — the pull only parses sizes right after the probe,
        // so framing (not this function) disambiguates.
        assert!(parse_size_response(&[0x01; 13]).is_ok());
    }

    #[test]
    fn notifications_classify() {
        assert!(matches!(
            classify_notification(&[0x03]),
            Notification::Stop
        ));
        assert!(matches!(
            classify_notification(&[0u8; 13]),
            Notification::Entry(_)
        ));
        assert!(matches!(
            classify_notification(&[0x01, 0x02, 0x00]),
            Notification::Size
        ));
        assert!(matches!(
            classify_notification(&[]),
            Notification::Unknown
        ));
        assert!(matches!(
            classify_notification(&[0x09, 0x00]),
            Notification::Unknown
        ));
    }

    // ── device-id persistence ──

    #[test]
    fn device_id_roundtrips_on_disk() {
        let path = std::env::temp_dir().join(format!(
            "grammatic-device-id-{}-{}",
            std::process::id(),
            rand_u32()
        ));
        let _ = std::fs::remove_file(&path);
        let first = load_or_create_device_id(&path).unwrap();
        let second = load_or_create_device_id(&path).unwrap();
        assert_eq!(first, second);
        let _ = std::fs::remove_file(&path);
    }

    // ── the session tracker ──

    fn now() -> std::time::Instant {
        std::time::Instant::now()
    }

    fn new_tracker() -> SessionTracker {
        SessionTracker::new(Duration::from_secs(75))
    }

    #[test]
    fn disarmed_never_fires() {
        let tracker = new_tracker();
        assert!(!tracker.ready(now(), true));
    }

    #[test]
    fn fires_after_quiet_plus_clock_idle() {
        let mut tracker = new_tracker();
        let t0 = now();
        tracker.note_sighting(t0);
        tracker.note_live_frame();
        // Still within the quiet window: no fire.
        assert!(!tracker.ready(t0 + Duration::from_secs(74), true));
        // Quiet elapsed but the clock-sync monitor says busy: no fire.
        assert!(!tracker.ready(t0 + Duration::from_secs(76), false));
        // Quiet + idle: fire.
        assert!(tracker.ready(t0 + Duration::from_secs(76), true));
    }

    #[test]
    fn fires_once_per_arming() {
        let mut tracker = new_tracker();
        let t0 = now();
        tracker.note_sighting(t0);
        tracker.note_live_frame();
        let fire_at = t0 + Duration::from_secs(76);
        assert!(tracker.ready(fire_at, true));
        tracker.note_fire(fire_at);
        // Same conditions, but already fired: wait for disarm.
        assert!(!tracker.ready(fire_at + Duration::from_secs(60), true));
    }

    #[test]
    fn stabilized_disarms() {
        let mut tracker = new_tracker();
        let t0 = now();
        tracker.note_sighting(t0);
        tracker.note_live_frame();
        tracker.note_stabilized();
        assert!(!tracker.is_armed());
        assert!(!tracker.ready(t0 + Duration::from_secs(300), true));
    }

    #[test]
    fn empty_pull_disarms_until_new_live_frames() {
        let mut tracker = new_tracker();
        let t0 = now();
        tracker.note_sighting(t0);
        tracker.note_live_frame();
        tracker.note_fire(t0 + Duration::from_secs(76));
        tracker.note_empty_pull();
        assert!(!tracker.ready(t0 + Duration::from_secs(300), true));
        // New weigh-in activity re-arms.
        tracker.note_sighting(t0 + Duration::from_secs(300));
        tracker.note_live_frame();
        assert!(tracker.ready(t0 + Duration::from_secs(400), true));
    }

    #[test]
    fn failures_back_off() {
        let mut tracker = new_tracker();
        let t0 = now();
        tracker.note_sighting(t0);
        tracker.note_live_frame();
        let fire_at = t0 + Duration::from_secs(76);
        assert!(tracker.ready(fire_at, true));
        tracker.note_fire(fire_at);
        tracker.note_attempt(false, fire_at);
        // Fire consumed the arming; a new weigh-in re-arms, but the doubled
        // (10 s) backoff still gates the next fire. The re-arm sighting
        // resets the quiet window, so check relative to it.
        let t1 = fire_at + Duration::from_secs(1);
        tracker.note_sighting(t1);
        tracker.note_live_frame();
        tracker.last_fire = None;
        assert!(!tracker.ready(t1 + Duration::from_secs(9), true));
        assert!(tracker.ready(t1 + Duration::from_secs(76), true));
    }

    #[test]
    fn config_defaults_keep_auto_fetch_on() {
        let config = HistoryConfig::default();
        assert!(config.enabled);
        assert!(config.auto_fetch);
        assert!(config.quiet_timeout_secs > 30);
    }
}

//! The capture pipeline: everything that happens between a raw BLE frame and
//! a stored measurement, behind a single interface.
//!
//! `Capture::handle_frame` hides frame parsing, clock classification, profile
//! assignment, metric computation, dedup (via the DB constraint), and
//! spool-and-forward when Postgres is unreachable. Callers (the listen loop,
//! the replay command) decide only what to *do* with the outcome — log, exit,
//! trigger a clock sync.
//!
//! Persistence sits behind the [`MeasurementSink`] seam, owned by this
//! module: `store` provides the Postgres adapter, tests an in-memory one.

use std::future::Future;
use std::sync::Arc;

use chrono::{DateTime, FixedOffset, Local};
use tracing::{debug, info, warn};

use crate::clock::{self, ClockDecision};
use crate::metrics::MeasurementMetrics;
use crate::parser::{Measurement, Unit, parse_body_composition_frame, parse_history_entry};
use crate::profile::{HistoryPoint, MetricsPolicy, Profile};
use crate::spool::Spool;

/// An existing weight-only row upgraded with a later impedance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Enrichment {
    pub id: i64,
    /// The weight-only frame hex this replaces (kept in the log).
    pub superseded_raw_frame: String,
}

/// Durable outcome of the complete persistence decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersistOutcome {
    Inserted,
    Enriched(Enrichment),
    Duplicate,
}

impl PersistOutcome {
    pub fn recorded(&self) -> bool {
        !matches!(self, Self::Duplicate)
    }
}

/// Capture's persistence seam: Postgres and an in-memory test adapter.
/// Futures promise `Send` so capture can run in spawned tasks.
pub trait MeasurementSink {
    /// The profiles that assignment chooses among.
    fn list_profiles(&self) -> impl Future<Output = anyhow::Result<Vec<Profile>>> + Send;
    /// Atomically record, enrich, or suppress one completed measurement.
    /// The adapter owns sibling ordering, dedup, and first-observation preservation.
    fn persist_measurement(
        &self,
        record: &MeasurementRecord,
    ) -> impl Future<Output = anyhow::Result<PersistOutcome>> + Send;
    /// Past measurements of one profile strictly before `before`, newest
    /// first, capped at `limit` — the tie-break view. Only called on
    /// overlapping weight windows; the bound is the current dedup-key time so
    /// replay sees the identical prefix regardless of replay wall-clock time.
    /// History failure is non-fatal: capture records the measurement as guest.
    fn recent_history(
        &self,
        profile_id: i64,
        before: chrono::DateTime<FixedOffset>,
        limit: u32,
    ) -> impl Future<Output = anyhow::Result<Vec<HistoryPoint>>> + Send;
}

/// One completed weigh-in, as handed across the sink seam.
#[derive(Debug, Clone)]
pub struct MeasurementRecord {
    pub measured_at: DateTime<FixedOffset>,
    pub clock_source: String,
    pub received_at: DateTime<FixedOffset>,
    pub weight_kg: f64,
    pub impedance_ohm: Option<i32>,
    pub profile_id: Option<i64>,
    pub unit: Unit,
    pub raw_frame: String,
    pub rssi: Option<i16>,
    pub metrics: Option<MeasurementMetrics>,
}

#[derive(Debug, Clone)]
pub struct CaptureResult {
    pub measurement: Measurement,
    /// Effective measurement time (validated frame clock or receive time).
    pub measured_at: DateTime<Local>,
    /// False for duplicates (re-broadcasts suppressed by the DB constraint)
    /// and when the frame was spooled due to a DB outage.
    pub recorded: bool,
    /// How far the frame's scale clock has drifted from receive time, when
    /// the scale clock is trusted. `None` means the frame is receiver-clocked
    /// — the scale clock is missing or implausible. Clock maintenance decides
    /// what to do with this; capture only reports it.
    pub clock_drift: Option<i64>,
}

pub struct Capture<S: MeasurementSink> {
    sink: S,
    spool: Arc<Spool>,
    /// Which derived calculations are stored alongside a measurement.
    metrics_policy: MetricsPolicy,
    /// The latest impedance seen in a live (not-yet-stabilized) frame, with
    /// its weight and receive time. The scale emits its stabilized frame
    /// without impedance, but broadcasts impedance-bearing live frames during
    /// the composition phase just before — this memory lets the stabilized
    /// measurement adopt the impedance of its own weigh-in. Overwritten by
    /// each newer impedance-bearing frame.
    pending_impedance: std::sync::Mutex<Option<PendingImpedance>>,
}

/// One impedance-bearing live frame, remembered for the attach rule.
#[derive(Debug, Clone, Copy)]
struct PendingImpedance {
    weight_kg: f64,
    impedance_ohm: u16,
    received_at: DateTime<Local>,
}

/// How long a remembered live impedance stays attachable. Compared on
/// receive time, never on the frame's own clock (the scale's clock may be
/// minutes off, as e2e weigh-ins showed).
const PENDING_IMPEDANCE_WINDOW: chrono::Duration = chrono::Duration::seconds(120);

impl<S: MeasurementSink> Capture<S> {
    pub fn new(sink: S, spool: Arc<Spool>) -> Capture<S> {
        Self::with_metrics_policy(sink, spool, MetricsPolicy::default())
    }

    pub fn with_metrics_policy(
        sink: S,
        spool: Arc<Spool>,
        metrics_policy: MetricsPolicy,
    ) -> Capture<S> {
        Capture {
            sink,
            spool,
            metrics_policy,
            pending_impedance: std::sync::Mutex::new(None),
        }
    }

    /// The impedance to attach to a stabilized frame without its own.
    /// `Some` only when the remembered live frame has exactly the same
    /// weight and is fresh — otherwise the composition belongs to another
    /// weigh-in (or nobody stood still long enough) and the measurement
    /// stays weight-only.
    fn attachable_impedance(&self, weight_kg: f64, received_at: DateTime<Local>) -> Option<u16> {
        let pending = *self.pending_impedance.lock().unwrap();
        let pending = pending?;
        (pending.weight_kg == weight_kg
            && received_at.signed_duration_since(pending.received_at) <= PENDING_IMPEDANCE_WINDOW)
            .then_some(pending.impedance_ohm)
    }

    /// Handle one raw service-data payload. Returns `None` for unparseable or
    /// not-yet-stabilized frames (still worth a debug log, which this module
    /// emits itself).
    pub async fn handle_frame(
        &self,
        payload: &[u8],
        received_at: DateTime<Local>,
        rssi: Option<i16>,
    ) -> Option<CaptureResult> {
        let Some(mut measurement) = parse_body_composition_frame(payload) else {
            debug!("ignoring frame: {}", hex::encode(payload));
            return None;
        };
        // Remember impedance-bearing live frames for the attach rule below.
        // Never cleared on record: re-broadcasts of the stabilized frame must
        // re-attach the same impedance so they collapse onto the original
        // measurement instead of inserting a weight-only duplicate.
        if !measurement.stabilized
            && let Some(impedance) = measurement.impedance_ohm
        {
            *self.pending_impedance.lock().unwrap() = Some(PendingImpedance {
                weight_kg: measurement.weight_kg,
                impedance_ohm: impedance,
                received_at,
            });
            debug!(
                "live frame: {:.2} kg (not stabilized yet)",
                measurement.weight_kg
            );
            return None;
        }
        if !measurement.stabilized {
            debug!(
                "live frame: {:.2} kg (not stabilized yet)",
                measurement.weight_kg
            );
            return None;
        }
        // The scale's stabilized frame carries no impedance of its own; adopt
        // the composition phase's when it is from this same weigh-in.
        if measurement.impedance_ohm.is_none()
            && let Some(impedance) = self.attachable_impedance(measurement.weight_kg, received_at)
        {
            measurement.impedance_ohm = Some(impedance);
        }

        self.record_measurement(payload, measurement, received_at, rssi)
            .await
    }

    /// Handle one raw history entry (spike-confirmed: a byte-identical
    /// 13-byte frame) from the GATT pull. History entries are stabilized
    /// records by construction, so this bypasses the live-frame gate and
    /// the impedance attach rule (the entry already carries its own
    /// impedance) and shares the record path below. Returns `None` for
    /// unparseable or unstabilized entries (still debug-logged).
    pub async fn handle_history_entry(
        &self,
        payload: &[u8],
        received_at: DateTime<Local>,
        rssi: Option<i16>,
    ) -> Option<CaptureResult> {
        let Some(measurement) = parse_history_entry(payload) else {
            debug!("ignoring history entry: {}", hex::encode(payload));
            return None;
        };
        if !measurement.stabilized {
            debug!(
                "history entry not stabilized ({:.2} kg); skipping",
                measurement.weight_kg
            );
            return None;
        }
        self.record_measurement(payload, measurement, received_at, rssi)
            .await
    }

    /// Shared record path for stabilized frames and history entries: clock
    /// decision → profile/metrics/dedup insert → spool on DB outage.
    async fn record_measurement(
        &self,
        payload: &[u8],
        measurement: Measurement,
        received_at: DateTime<Local>,
        rssi: Option<i16>,
    ) -> Option<CaptureResult> {
        let decision = clock::decide(measurement.timestamp, received_at);
        // measured_at must be *exactly* the frame's own clock (minute
        // resolution) when trusted, receive time truncated to the minute
        // otherwise: it is the dedup key, so any other derivation would make
        // re-broadcasts of the same frame look distinct. The clock module
        // owns this; capture only uses the decision.
        let measured_at = decision.measured_at;
        let clock_drift = decision.drift_secs;

        match self
            .record(payload, &measurement, &decision, received_at, rssi)
            .await
        {
            Ok(recorded) => {
                if recorded {
                    info!(
                        "recorded {:.2} kg{} from {}",
                        measurement.weight_kg,
                        measurement
                            .impedance_ohm
                            .map(|z| format!(", impedance {z} ohm"))
                            .unwrap_or_default(),
                        measured_at.to_rfc3339()
                    );
                } else {
                    debug!("duplicate of an already-recorded measurement, skipping");
                }
                Some(CaptureResult {
                    measurement,
                    measured_at,
                    recorded,
                    clock_drift,
                })
            }
            // DB unreachable: spool the raw frame; a later replay recomputes
            // profile and metrics with the original receive time.
            Err(error) => {
                warn!("database unreachable, spooling frame: {error}");
                let fixed = received_at.fixed_offset();
                if let Err(spool_error) = self.spool.append(payload, fixed, rssi) {
                    warn!("spooling failed, measurement lost: {spool_error}");
                }
                Some(CaptureResult {
                    measurement,
                    measured_at,
                    recorded: false,
                    clock_drift,
                })
            }
        }
    }

    async fn record(
        &self,
        payload: &[u8],
        measurement: &Measurement,
        decision: &ClockDecision,
        received_at: DateTime<Local>,
        rssi: Option<i16>,
    ) -> anyhow::Result<bool> {
        let profiles = self.sink.list_profiles().await?;
        // The measurement date ages the profile (ADR-0001): it arrives with
        // the clock decision, so capture never derives it itself.
        let weigh_in = crate::profile::WeighIn {
            weight_kg: measurement.weight_kg,
            impedance_ohm: measurement.impedance_ohm,
            measured_at: decision.measured_at.fixed_offset(),
            measurement_date: decision.measurement_date,
        };
        let (profile_id, metrics) =
            match crate::profile::resolve(&profiles, weigh_in, &self.metrics_policy) {
                // Zero (no window) or one (fast path): complete, no history I/O.
                crate::profile::Resolution::Done(profile, metrics) => {
                    (profile.map(|p| p.id), metrics)
                }
                // Overlapping windows only: one capped history fetch per
                // candidate, then the pure tie-break. History failure degrades to
                // guest — it never blocks the measurement or spools.
                crate::profile::Resolution::NeedsHistory(tie) => {
                    let mut histories = std::collections::BTreeMap::new();
                    for candidate in tie.candidates.iter() {
                        match self
                            .sink
                            .recent_history(
                                candidate.id,
                                weigh_in.measured_at,
                                crate::profile::HISTORY_LIMIT,
                            )
                            .await
                        {
                            Ok(past) => {
                                histories.insert(candidate.id, past);
                            }
                            Err(error) => {
                                warn!("history unavailable, recording as guest: {error:#}");
                                histories.clear();
                                break;
                            }
                        }
                    }
                    let (winner, metrics) = crate::profile::resolve_with_history(
                        &tie,
                        &histories,
                        &self.metrics_policy,
                    );
                    (winner.map(|p| p.id), metrics)
                }
            };
        if let Some(profile_id) = profile_id {
            if let Some(profile) = profiles.iter().find(|p| p.id == profile_id) {
                debug!("assigned to profile {}", profile.name);
            }
        } else if !profiles.is_empty() {
            info!(
                "weight {:.2} kg matches no profile window (or matches several); recording as guest",
                measurement.weight_kg
            );
        }

        let record = MeasurementRecord {
            measured_at: decision.measured_at.fixed_offset(),
            clock_source: decision.source.as_str().to_string(),
            received_at: received_at.fixed_offset(),
            weight_kg: measurement.weight_kg,
            impedance_ohm: measurement.impedance_ohm.map(i32::from),
            profile_id,
            unit: measurement.unit,
            raw_frame: hex::encode(payload),
            rssi,
            metrics,
        };
        let outcome = self.sink.persist_measurement(&record).await?;
        if let PersistOutcome::Enriched(enriched) = &outcome {
            info!(
                "enriched measurement {} with impedance {} ohm (superseded {})",
                enriched.id,
                record.impedance_ohm.unwrap_or_default(),
                enriched.superseded_raw_frame,
            );
        }
        Ok(outcome.recorded())
    }

    /// Replay spooled frames (captured during a DB outage) through the normal
    /// pipeline. Returns (replayed, malformed).
    pub async fn replay_spool(&self) -> anyhow::Result<(usize, usize)> {
        let drained = self.spool.drain()?;
        let mut replayed = 0;
        for frame in &drained.frames {
            let received_at = frame.received_at.with_timezone(&Local);
            if let Some(result) = self
                .handle_frame(&frame.payload, received_at, frame.rssi)
                .await
                && result.recorded
            {
                replayed += 1;
            }
        }
        if drained.malformed > 0 {
            warn!("dropped {} malformed spool lines", drained.malformed);
        }
        Ok((replayed, drained.malformed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, NaiveDateTime, TimeZone, Timelike};
    use std::sync::Mutex;

    // ── the in-memory sink: the adapter that makes the seam real ──

    #[derive(Clone, Copy)]
    enum InsertOutcome {
        Recorded,
        Duplicate,
        Unreachable,
    }

    struct FakeSink {
        profiles: Vec<Profile>,
        outcome: InsertOutcome,
        inserted: Arc<Mutex<Vec<MeasurementRecord>>>,
        /// Seed history the tie-break can consult, mirroring stored rows.
        history: Vec<MeasurementRecord>,
        history_error: bool,
    }

    impl FakeSink {
        fn new(
            profiles: Vec<Profile>,
            outcome: InsertOutcome,
        ) -> (FakeSink, Arc<Mutex<Vec<MeasurementRecord>>>) {
            Self::with_history(profiles, outcome, vec![])
        }

        fn with_history(
            profiles: Vec<Profile>,
            outcome: InsertOutcome,
            history: Vec<MeasurementRecord>,
        ) -> (FakeSink, Arc<Mutex<Vec<MeasurementRecord>>>) {
            let inserted = Arc::new(Mutex::new(Vec::new()));
            (
                FakeSink {
                    profiles,
                    outcome,
                    inserted: inserted.clone(),
                    history,
                    history_error: false,
                },
                inserted,
            )
        }
    }

    impl MeasurementSink for FakeSink {
        async fn list_profiles(&self) -> anyhow::Result<Vec<Profile>> {
            Ok(self.profiles.clone())
        }

        async fn persist_measurement(
            &self,
            record: &MeasurementRecord,
        ) -> anyhow::Result<PersistOutcome> {
            let mut inserted = self.inserted.lock().unwrap();
            match self.outcome {
                InsertOutcome::Unreachable => return Err(anyhow::anyhow!("database unreachable")),
                InsertOutcome::Duplicate => return Ok(PersistOutcome::Duplicate),
                InsertOutcome::Recorded => {}
            }
            let siblings = || {
                self.history.iter().chain(inserted.iter()).filter(|r| {
                    r.measured_at == record.measured_at && r.weight_kg == record.weight_kg
                })
            };
            if siblings().any(|r| {
                r.impedance_ohm == record.impedance_ohm
                    || (record.impedance_ohm.is_none() && r.impedance_ohm.is_some())
            }) {
                return Ok(PersistOutcome::Duplicate);
            }
            if record.impedance_ohm.is_some()
                && let Some(pos) = inserted.iter().position(|r| {
                    r.measured_at == record.measured_at
                        && r.weight_kg == record.weight_kg
                        && r.impedance_ohm.is_none()
                })
            {
                let old = inserted[pos].clone();
                let mut upgraded = record.clone();
                upgraded.received_at = old.received_at;
                upgraded.rssi = old.rssi;
                inserted[pos] = upgraded;
                return Ok(PersistOutcome::Enriched(Enrichment {
                    id: -(pos as i64) - 1,
                    superseded_raw_frame: old.raw_frame,
                }));
            }
            inserted.push(record.clone());
            Ok(PersistOutcome::Inserted)
        }

        async fn recent_history(
            &self,
            profile_id: i64,
            before: chrono::DateTime<FixedOffset>,
            limit: u32,
        ) -> anyhow::Result<Vec<HistoryPoint>> {
            if self.history_error {
                return Err(anyhow::anyhow!("history unavailable"));
            }
            let mut points: Vec<HistoryPoint> = self
                .history
                .iter()
                .chain(self.inserted.lock().unwrap().iter())
                .filter(|record| {
                    record.profile_id == Some(profile_id) && record.measured_at < before
                })
                .map(|record| HistoryPoint {
                    weight_kg: record.weight_kg,
                    impedance_ohm: record.impedance_ohm,
                    measured_at: record.measured_at,
                })
                .collect();
            points.sort_by(|a, b| a.measured_at.cmp(&b.measured_at).reverse());
            points.truncate(limit as usize);
            Ok(points)
        }
    }

    // ── frame builders (13-byte kg body-composition frames) ──

    fn body_frame(weight_raw: u16, flags: u8, timestamp: Option<NaiveDateTime>) -> Vec<u8> {
        body_frame_with_impedance(weight_raw, flags, Some(500), timestamp)
    }

    fn body_frame_with_impedance(
        weight_raw: u16,
        flags: u8,
        impedance: Option<u16>,
        timestamp: Option<NaiveDateTime>,
    ) -> Vec<u8> {
        let mut frame = vec![0u8; 13];
        frame[0] = 0x02;
        frame[1] = flags;
        if let Some(ts) = timestamp {
            frame[2] = (ts.year() & 0xFF) as u8;
            frame[3] = ((ts.year() >> 8) & 0xFF) as u8;
            frame[4] = ts.month() as u8;
            frame[5] = ts.day() as u8;
            frame[6] = ts.hour() as u8;
            frame[7] = ts.minute() as u8;
        }
        if let Some(impedance) = impedance {
            frame[9] = (impedance & 0xFF) as u8;
            frame[10] = (impedance >> 8) as u8;
        }
        frame[11] = (weight_raw & 0xFF) as u8;
        frame[12] = (weight_raw >> 8) as u8;
        frame
    }

    /// Stabilized (final) frame: impedance present, no removed flag.
    fn stabilized_frame(weight_raw: u16, timestamp: Option<NaiveDateTime>) -> Vec<u8> {
        body_frame(weight_raw, 0x22, timestamp)
    }

    /// Stabilized frame without impedance — what the scale actually emits
    /// after a weigh-in (the composition phase's live frames carry it).
    fn stabilized_frame_no_impedance(weight_raw: u16, timestamp: Option<NaiveDateTime>) -> Vec<u8> {
        body_frame_with_impedance(weight_raw, 0x20, None, timestamp)
    }

    /// Live (unstable) frame with impedance: the composition phase.
    fn live_frame_with_impedance(weight_raw: u16, impedance: u16) -> Vec<u8> {
        body_frame_with_impedance(weight_raw, 0x02, Some(impedance), None)
    }

    /// Live (unstable) frame: same shape, stabilization bit unset.
    fn live_frame() -> Vec<u8> {
        body_frame(15000, 0x02, None)
    }

    // ── helpers ──

    fn profile(id: i64, min: Option<f64>, max: Option<f64>) -> Profile {
        Profile {
            id,
            name: format!("p{id}"),
            sex: "male".into(),
            height_cm: 175.0,
            dob: chrono::NaiveDate::from_ymd_opt(1996, 1, 1).unwrap(),
            weight_min: min,
            weight_max: max,
        }
    }

    fn spool_at(name: &str) -> Arc<Spool> {
        let path = std::env::temp_dir().join(format!(
            "grammatic-capture-test-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        Arc::new(Spool::new(path, 4096))
    }

    fn at(minute: u32, second: u64) -> DateTime<Local> {
        Local
            .with_ymd_and_hms(2026, 9, 3, 12, minute, second as u32)
            .unwrap()
    }

    fn naive(minute: u32) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(&format!("2026-09-03 12:{minute:02}"), "%Y-%m-%d %H:%M")
            .unwrap()
    }

    // ── the decision tree, driven through handle_frame ──

    #[tokio::test]
    async fn stabilized_frames_are_recorded_with_the_scale_clock() {
        let (sink, inserted) = FakeSink::new(
            vec![profile(1, Some(40.0), Some(80.0))],
            InsertOutcome::Recorded,
        );
        let capture = Capture::new(sink, spool_at("recorded"));

        let result = capture
            .handle_frame(
                &stabilized_frame(15000, Some(naive(46))),
                at(46, 5),
                Some(-55),
            )
            .await
            .unwrap();

        // measured_at is exactly the frame's clock — the dedup key.
        assert_eq!(result.measured_at, at(46, 0));
        assert!(result.recorded);

        let records = inserted.lock().unwrap().clone();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].clock_source, "scale");
        assert_eq!(records[0].profile_id, Some(1));
        assert_eq!(records[0].weight_kg, 75.0);
        assert_eq!(records[0].impedance_ohm, Some(500));
        assert_eq!(records[0].rssi, Some(-55));
        // profile assigned → metrics computed
        assert!(records[0].metrics.is_some());
    }

    #[tokio::test]
    async fn live_frames_never_become_measurements() {
        let (sink, inserted) = FakeSink::new(vec![], InsertOutcome::Recorded);
        let capture = Capture::new(sink, spool_at("live"));

        assert!(
            capture
                .handle_frame(&live_frame(), at(46, 5), None)
                .await
                .is_none()
        );
        assert!(inserted.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn unparseable_payloads_are_ignored() {
        let (sink, inserted) = FakeSink::new(vec![], InsertOutcome::Recorded);
        let capture = Capture::new(sink, spool_at("junk"));

        assert!(
            capture
                .handle_frame(&[0u8; 5], at(46, 5), None)
                .await
                .is_none()
        );
        assert!(inserted.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn duplicates_report_recorded_false() {
        let (sink, inserted) = FakeSink::new(
            vec![profile(1, Some(40.0), Some(80.0))],
            InsertOutcome::Duplicate,
        );
        let capture = Capture::new(sink, spool_at("duplicate"));

        let result = capture
            .handle_frame(&stabilized_frame(15000, Some(naive(46))), at(46, 5), None)
            .await
            .unwrap();

        assert!(!result.recorded);
        // Suppression leaves no newly persisted row.
        assert!(inserted.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn database_outage_spools_the_frame() {
        let (sink, inserted) = FakeSink::new(
            vec![profile(1, Some(40.0), Some(80.0))],
            InsertOutcome::Unreachable,
        );
        let spool = spool_at("outage");
        let capture = Capture::new(sink, spool.clone());

        let payload = stabilized_frame(15000, Some(naive(46)));
        let result = capture
            .handle_frame(&payload, at(46, 5), Some(-55))
            .await
            .unwrap();

        assert!(!result.recorded);
        // Failed persistence leaves no row; the frame is spooled.
        assert!(inserted.lock().unwrap().is_empty());

        // the frame sits in the spool, ready for replay with its receive time
        let drained = spool.drain().unwrap();
        assert_eq!(drained.frames.len(), 1);
        assert_eq!(drained.frames[0].payload, payload);
        assert_eq!(drained.frames[0].received_at, at(46, 5).fixed_offset());
    }

    #[tokio::test]
    async fn receiver_clock_frames_get_a_minute_stable_key() {
        // Year 1999 is outside the plausible range: the scale clock is not
        // trusted, so receive time (truncated to the minute) becomes the key.
        let (sink, inserted) = FakeSink::new(
            vec![profile(1, Some(40.0), Some(80.0))],
            InsertOutcome::Recorded,
        );
        let capture = Capture::new(sink, spool_at("receiver"));

        let stale = NaiveDateTime::parse_from_str("1999-09-03 12:46", "%Y-%m-%d %H:%M").unwrap();
        let result = capture
            .handle_frame(&stabilized_frame(15000, Some(stale)), at(46, 44), None)
            .await
            .unwrap();

        assert_eq!(result.measured_at, at(46, 0));
        let records = inserted.lock().unwrap().clone();
        assert_eq!(records[0].clock_source, "receiver");
        assert_eq!(records[0].measured_at.second(), 0);
        assert_eq!(records[0].measured_at.nanosecond(), 0);
    }

    #[tokio::test]
    async fn capture_reports_clock_drift_and_receiver_frames() {
        let (sink, _) = FakeSink::new(vec![], InsertOutcome::Recorded);
        let capture = Capture::new(sink, spool_at("drift"));

        // frame says 12:43, received at 12:46 → 180 s drift
        let drifted = capture
            .handle_frame(&stabilized_frame(15000, Some(naive(43))), at(46, 0), None)
            .await
            .unwrap();
        assert_eq!(drifted.clock_drift, Some(180));

        // frame says 12:45 → 60 s drift
        let close = capture
            .handle_frame(&stabilized_frame(15000, Some(naive(45))), at(46, 0), None)
            .await
            .unwrap();
        assert_eq!(close.clock_drift, Some(60));
    }

    #[tokio::test]
    async fn guests_get_no_profile_and_no_metrics() {
        let (sink, inserted) = FakeSink::new(
            vec![profile(1, Some(40.0), Some(80.0))],
            InsertOutcome::Recorded,
        );
        let capture = Capture::new(sink, spool_at("guest"));

        // 90 kg (raw 18000) matches no window
        let result = capture
            .handle_frame(&stabilized_frame(18000, Some(naive(46))), at(46, 5), None)
            .await
            .unwrap();

        assert!(result.recorded);
        let records = inserted.lock().unwrap().clone();
        assert_eq!(records[0].profile_id, None);
        assert!(records[0].metrics.is_none());
    }

    // ── the impedance attach rule ──

    #[tokio::test]
    async fn stabilized_frame_adopts_matching_live_impedance() {
        let (sink, inserted) = FakeSink::new(
            vec![profile(1, Some(40.0), Some(80.0))],
            InsertOutcome::Recorded,
        );
        let capture = Capture::new(sink, spool_at("attach"));

        // Composition phase: live frames with impedance 599 at 75 kg.
        assert!(
            capture
                .handle_frame(&live_frame_with_impedance(15000, 599), at(46, 0), None)
                .await
                .is_none()
        );
        // The scale's stabilized frame carries no impedance of its own.
        let result = capture
            .handle_frame(
                &stabilized_frame_no_impedance(15000, Some(naive(46))),
                at(46, 5),
                None,
            )
            .await
            .unwrap();

        assert!(result.recorded);
        let records = inserted.lock().unwrap().clone();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].impedance_ohm, Some(599));
        assert!(records[0].metrics.is_some());
    }

    #[tokio::test]
    async fn mismatched_weight_stays_weight_only() {
        let (sink, inserted) = FakeSink::new(
            vec![profile(1, Some(40.0), Some(80.0))],
            InsertOutcome::Recorded,
        );
        let capture = Capture::new(sink, spool_at("mismatch"));

        // Live impedance at 75 kg, but the stabilized frame says 74 kg —
        // a different weigh-in. No attach.
        assert!(
            capture
                .handle_frame(&live_frame_with_impedance(15000, 599), at(46, 0), None)
                .await
                .is_none()
        );
        capture
            .handle_frame(
                &stabilized_frame_no_impedance(14800, Some(naive(46))),
                at(46, 5),
                None,
            )
            .await
            .unwrap();

        let records = inserted.lock().unwrap().clone();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].weight_kg, 74.0);
        assert_eq!(records[0].impedance_ohm, None);
    }

    #[tokio::test]
    async fn stale_live_impedance_is_not_attached() {
        let (sink, inserted) = FakeSink::new(
            vec![profile(1, Some(40.0), Some(80.0))],
            InsertOutcome::Recorded,
        );
        let capture = Capture::new(sink, spool_at("stale"));

        // Impedance from an earlier session, 121 s ago: expired.
        assert!(
            capture
                .handle_frame(&live_frame_with_impedance(15000, 599), at(44, 0), None)
                .await
                .is_none()
        );
        capture
            .handle_frame(
                &stabilized_frame_no_impedance(15000, Some(naive(46))),
                at(46, 1),
                None,
            )
            .await
            .unwrap();

        let records = inserted.lock().unwrap().clone();
        assert_eq!(records[0].impedance_ohm, None);
    }

    #[tokio::test]
    async fn re_broadcast_reattaches_the_same_impedance() {
        let (sink, inserted) = FakeSink::new(
            vec![profile(1, Some(40.0), Some(80.0))],
            InsertOutcome::Recorded,
        );
        let capture = Capture::new(sink, spool_at("rebroadcast"));

        assert!(
            capture
                .handle_frame(&live_frame_with_impedance(15000, 599), at(46, 0), None)
                .await
                .is_none()
        );
        let frame = stabilized_frame_no_impedance(15000, Some(naive(46)));
        capture.handle_frame(&frame, at(46, 5), None).await.unwrap();
        // Re-broadcast of the identical final frame: memory is retained, so
        // it re-attaches and collapses onto the same dedup key.
        capture.handle_frame(&frame, at(46, 8), None).await.unwrap();

        let records = inserted.lock().unwrap().clone();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].impedance_ohm, Some(599));
    }

    // ── history entries (GATT pull) share the record path ──

    /// History entries are byte-identical 13-byte frames (spike-confirmed),
    /// so the test builder is the frame builder with a unit byte + seconds.
    fn history_entry(
        flags: u8,
        impedance: Option<u16>,
        weight_raw: u16,
        timestamp: NaiveDateTime,
        seconds: u8,
    ) -> Vec<u8> {
        let mut entry = body_frame_with_impedance(weight_raw, flags, impedance, Some(timestamp));
        entry[0] = 0x02;
        entry[8] = seconds;
        entry
    }

    #[tokio::test]
    async fn history_entry_records_without_live_frames() {
        let (sink, inserted) = FakeSink::new(
            vec![profile(1, Some(40.0), Some(80.0))],
            InsertOutcome::Recorded,
        );
        let capture = Capture::new(sink, spool_at("history"));

        // No live frames first: the pull alone recovers the weigh-in.
        let entry = history_entry(0x22, Some(500), 15000, naive(46), 44);
        let result = capture
            .handle_history_entry(&entry, at(50, 0), Some(-55))
            .await
            .unwrap();

        assert!(result.recorded);
        // Seconds never enter the key: history and live share one dedup key.
        assert_eq!(result.measured_at, at(46, 0));
        let records = inserted.lock().unwrap().clone();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].weight_kg, 75.0);
        assert_eq!(records[0].impedance_ohm, Some(500));
        assert_eq!(records[0].clock_source, "scale");
        assert_eq!(records[0].rssi, Some(-55));
        assert!(records[0].metrics.is_some());
    }

    #[tokio::test]
    async fn history_entry_needs_no_impedance_attach() {
        let (sink, inserted) = FakeSink::new(
            vec![profile(1, Some(40.0), Some(80.0))],
            InsertOutcome::Recorded,
        );
        let capture = Capture::new(sink, spool_at("history-no-attach"));

        // A stale live impedance from another weigh-in must not leak into
        // the history measurement, which carries its own.
        assert!(
            capture
                .handle_frame(&live_frame_with_impedance(14800, 599), at(44, 0), None)
                .await
                .is_none()
        );
        let entry = history_entry(0x22, Some(500), 15000, naive(46), 0);
        capture
            .handle_history_entry(&entry, at(50, 0), None)
            .await
            .unwrap();

        let records = inserted.lock().unwrap().clone();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].impedance_ohm, Some(500));
    }

    #[tokio::test]
    async fn re_pulled_history_entry_collapses() {
        let (sink, inserted) = FakeSink::new(
            vec![profile(1, Some(40.0), Some(80.0))],
            InsertOutcome::Duplicate,
        );
        let capture = Capture::new(sink, spool_at("history-duplicate"));

        let entry = history_entry(0x22, Some(500), 15000, naive(46), 44);
        let result = capture
            .handle_history_entry(&entry, at(50, 0), None)
            .await
            .unwrap();

        assert!(!result.recorded);
        assert!(inserted.lock().unwrap().is_empty());
    }

    // ── impedance enrichment: one weigh-in, one row ──
    //
    // Soak 2026-09-04: the live stabilized frame arrived impedance-free
    // (row 21) and the fallback pull later delivered the same minute+weight
    // with impedance (row 22) — two rows for one weigh-in. The history entry
    // now enriches the weight-only sibling instead.

    #[tokio::test]
    async fn history_entry_enriches_its_weight_only_live_row() {
        let (sink, inserted) = FakeSink::new(
            vec![profile(1, Some(40.0), Some(80.0))],
            InsertOutcome::Recorded,
        );
        let capture = Capture::new(sink, spool_at("history-enrich"));

        // Live stabilized frame, no impedance (the scale's stabilized frame
        // carries none; no live impedance in memory to attach).
        let live = capture
            .handle_frame(
                &stabilized_frame_no_impedance(15000, Some(naive(46))),
                at(46, 5),
                Some(-55),
            )
            .await
            .unwrap();
        assert!(live.recorded);

        // The pull delivers the same minute+weight with impedance.
        let entry = history_entry(0x22, Some(500), 15000, naive(46), 44);
        let recovered = capture
            .handle_history_entry(&entry, at(50, 0), None)
            .await
            .unwrap();
        assert!(recovered.recorded);

        // One row, upgraded: first observation keeps received_at/rssi.
        let records = inserted.lock().unwrap().clone();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].impedance_ohm, Some(500));
        assert_eq!(records[0].received_at, at(46, 5).fixed_offset());
        assert_eq!(records[0].rssi, Some(-55));
        assert!(records[0].metrics.is_some());
    }

    #[tokio::test]
    async fn weight_only_frame_after_impedance_sibling_collapses() {
        let (sink, inserted) = FakeSink::new(
            vec![profile(1, Some(40.0), Some(80.0))],
            InsertOutcome::Recorded,
        );
        let capture = Capture::new(sink, spool_at("weight-only-after"));

        // Reverse order: history first, then the weight-only live frame
        // (e.g. a re-broadcast arriving after the pull enriched the row).
        let entry = history_entry(0x22, Some(500), 15000, naive(46), 44);
        assert!(
            capture
                .handle_history_entry(&entry, at(46, 5), None)
                .await
                .unwrap()
                .recorded
        );
        let late = capture
            .handle_frame(
                &stabilized_frame_no_impedance(15000, Some(naive(46))),
                at(46, 8),
                None,
            )
            .await
            .unwrap();
        assert!(!late.recorded);

        let records = inserted.lock().unwrap().clone();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].impedance_ohm, Some(500));
    }

    #[tokio::test]
    async fn conflicting_impedance_stays_a_distinct_row() {
        let (sink, inserted) = FakeSink::new(
            vec![profile(1, Some(40.0), Some(80.0))],
            InsertOutcome::Recorded,
        );
        let capture = Capture::new(sink, spool_at("impedance-conflict"));

        // Same minute+weight but a *different* impedance is genuinely odd
        // evidence — it must not silently merge.
        let first = history_entry(0x22, Some(500), 15000, naive(46), 44);
        assert!(
            capture
                .handle_history_entry(&first, at(50, 0), None)
                .await
                .unwrap()
                .recorded
        );
        let second = history_entry(0x22, Some(501), 15000, naive(46), 45);
        assert!(
            capture
                .handle_history_entry(&second, at(50, 1), None)
                .await
                .unwrap()
                .recorded
        );

        assert_eq!(inserted.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn malformed_history_entries_are_ignored() {
        let (sink, inserted) = FakeSink::new(vec![], InsertOutcome::Recorded);
        let capture = Capture::new(sink, spool_at("history-junk"));

        assert!(
            capture
                .handle_history_entry(&[0u8; 5], at(50, 0), None)
                .await
                .is_none()
        );
        // Unstabilized shape: parsed but gated, like live frames.
        let live_shape = history_entry(0x02, Some(500), 15000, naive(46), 0);
        assert!(
            capture
                .handle_history_entry(&live_shape, at(50, 0), None)
                .await
                .is_none()
        );
        assert!(inserted.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn history_outage_spools_the_entry() {
        let (sink, inserted) = FakeSink::new(
            vec![profile(1, Some(40.0), Some(80.0))],
            InsertOutcome::Unreachable,
        );
        let spool = spool_at("history-outage");
        let capture = Capture::new(sink, spool.clone());

        let entry = history_entry(0x22, Some(500), 15000, naive(46), 44);
        let result = capture
            .handle_history_entry(&entry, at(50, 0), Some(-55))
            .await
            .unwrap();

        assert!(!result.recorded);
        assert!(inserted.lock().unwrap().is_empty());
        let drained = spool.drain().unwrap();
        assert_eq!(drained.frames.len(), 1);
        assert_eq!(drained.frames[0].payload, entry);
    }

    // ── the history tie-break: overlapping windows only ──

    fn overlapping() -> Vec<Profile> {
        vec![
            profile(1, Some(30.0), Some(80.0)),
            profile(2, Some(40.0), Some(90.0)),
        ]
    }

    fn seed_record(
        profile_id: i64,
        weight_kg: f64,
        impedance_ohm: Option<i32>,
    ) -> MeasurementRecord {
        MeasurementRecord {
            measured_at: at(40, 0).fixed_offset(),
            clock_source: "scale".into(),
            received_at: at(40, 0).fixed_offset(),
            weight_kg,
            impedance_ohm,
            profile_id: Some(profile_id),
            unit: crate::parser::Unit::Kg,
            raw_frame: String::new(),
            rssi: None,
            metrics: None,
        }
    }

    #[tokio::test]
    async fn overlap_resolves_to_the_nearer_history() {
        let history = vec![
            seed_record(1, 66.5, Some(500)),
            seed_record(2, 72.0, Some(520)),
        ];
        let (sink, inserted) =
            FakeSink::with_history(overlapping(), InsertOutcome::Recorded, history);
        let capture = Capture::new(sink, spool_at("tiebreak"));

        // 66.9 kg (raw 13380) sits in both windows, nearer to profile 1.
        let result = capture
            .handle_frame(&stabilized_frame(13380, Some(naive(46))), at(46, 5), None)
            .await
            .unwrap();

        assert!(result.recorded);
        let records = inserted.lock().unwrap().clone();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].profile_id, Some(1));
        assert!(records[0].metrics.is_some());
    }

    #[tokio::test]
    async fn overlap_without_history_stays_guest() {
        let (sink, inserted) =
            FakeSink::with_history(overlapping(), InsertOutcome::Recorded, vec![]);
        let capture = Capture::new(sink, spool_at("tiebreak-cold"));

        let result = capture
            .handle_frame(&stabilized_frame(14000, Some(naive(46))), at(46, 5), None)
            .await
            .unwrap();

        assert!(result.recorded);
        let records = inserted.lock().unwrap().clone();
        assert_eq!(records[0].profile_id, None);
        assert!(records[0].metrics.is_none());
    }

    #[tokio::test]
    async fn history_failure_still_records_as_guest() {
        let (mut sink, inserted) =
            FakeSink::with_history(overlapping(), InsertOutcome::Recorded, vec![]);
        sink.history_error = true;
        let capture = Capture::new(sink, spool_at("tiebreak-error"));

        let result = capture
            .handle_frame(&stabilized_frame(14000, Some(naive(46))), at(46, 5), None)
            .await
            .unwrap();

        // History never blocks a measurement.
        assert!(result.recorded);
        let records = inserted.lock().unwrap().clone();
        assert_eq!(records[0].profile_id, None);
    }

    #[tokio::test]
    async fn weight_only_policy_stores_no_derived_metrics() {
        let (sink, inserted) = FakeSink::new(
            vec![profile(1, Some(40.0), Some(80.0))],
            InsertOutcome::Recorded,
        );
        let capture =
            Capture::with_metrics_policy(sink, spool_at("weight-only"), MetricsPolicy::WeightOnly);

        capture
            .handle_frame(&stabilized_frame(15000, Some(naive(46))), at(46, 5), None)
            .await
            .unwrap();

        let records = inserted.lock().unwrap().clone();
        let metrics = records[0].metrics.as_ref().unwrap();
        assert!(metrics.bmi > 0.0);
        assert!(metrics.body_fat_pct.is_none());
        assert!(metrics.body_type.is_none());
    }

    #[tokio::test]
    async fn none_policy_stores_no_metrics_but_still_records() {
        let (sink, inserted) = FakeSink::new(
            vec![profile(1, Some(40.0), Some(80.0))],
            InsertOutcome::Recorded,
        );
        let capture =
            Capture::with_metrics_policy(sink, spool_at("no-metrics"), MetricsPolicy::None);

        let result = capture
            .handle_frame(&stabilized_frame(15000, Some(naive(46))), at(46, 5), None)
            .await
            .unwrap();

        assert!(result.recorded);
        let records = inserted.lock().unwrap().clone();
        assert_eq!(records[0].profile_id, Some(1));
        assert!(records[0].metrics.is_none());
    }
}

//! Deciding the Scale clock against receive time.
//!
//! The frame carries the scale's own clock, which nobody may have synced in
//! ages (we dropped the phone app). This module owns the whole trust
//! decision in one call: whether the frame timestamp can be trusted, the
//! effective measurement time, its source, the drift, and the measurement
//! date (ADR-0001). Callers cross this single seam; they no longer assemble
//! the decision themselves.

use chrono::{DateTime, Datelike, Local, NaiveDate, NaiveDateTime, TimeZone, Timelike};

/// How far the frame clock may stray from receive time and still be trusted.
pub const TRUST_WINDOW_SECS: i64 = 24 * 3600;

/// Where the effective measurement time came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockSource {
    Scale,
    Receiver,
}

impl ClockSource {
    pub fn as_str(self) -> &'static str {
        match self {
            ClockSource::Scale => "scale",
            ClockSource::Receiver => "receiver",
        }
    }
}

/// The finished clock decision for one frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockDecision {
    /// Effective measurement time: exactly the frame's own clock (minute
    /// resolution) when trusted, receive time truncated to the minute
    /// otherwise — so re-broadcasts of one Measurement share one Dedup key.
    pub measured_at: DateTime<Local>,
    pub source: ClockSource,
    /// How far the frame's scale clock has drifted from receive time, when
    /// the scale clock is trusted. `None` means the frame is
    /// receiver-clocked — the scale clock is missing or implausible.
    pub drift_secs: Option<i64>,
    /// The date that ages a profile (ADR-0001): the validated scale clock's
    /// date when trusted, the receive date otherwise.
    pub measurement_date: NaiveDate,
}

/// Interpret a naive frame timestamp in the machine's local timezone and
/// decide the effective measurement time, its source, drift, and date.
pub fn decide(frame_ts: Option<NaiveDateTime>, received_at: DateTime<Local>) -> ClockDecision {
    match trusted_frame_local(frame_ts, received_at) {
        Some((frame_local, drift_secs)) => ClockDecision {
            measured_at: frame_local,
            source: ClockSource::Scale,
            drift_secs: Some(drift_secs),
            measurement_date: frame_local.date_naive(),
        },
        None => {
            let measured_at = truncate_to_minute(received_at);
            ClockDecision {
                measured_at,
                source: ClockSource::Receiver,
                drift_secs: None,
                measurement_date: measured_at.date_naive(),
            }
        }
    }
}

/// The trusted frame instant and its drift, or `None` when the scale clock
/// must not be trusted.
fn trusted_frame_local(
    frame_ts: Option<NaiveDateTime>,
    received_at: DateTime<Local>,
) -> Option<(DateTime<Local>, i64)> {
    let frame_ts = frame_ts?;
    // Semantic plausibility lives here, not in the frame parser: the parser
    // only checks byte shape, this module owns every trust rule.
    if !(2000..=2099).contains(&frame_ts.year()) {
        return None;
    }
    let frame_local = received_at.timezone().from_local_datetime(&frame_ts);
    let frame_local = frame_local.single()?;
    // Ambiguous or skipped local time (DST transition) falls through to None.
    let drift = (received_at - frame_local).num_seconds();
    if drift.abs() > TRUST_WINDOW_SECS {
        return None;
    }
    Some((frame_local, drift))
}

/// Drop seconds and subseconds, keeping the minute.
fn truncate_to_minute(value: DateTime<Local>) -> DateTime<Local> {
    value
        .with_second(0)
        .and_then(|v| v.with_nanosecond(0))
        .unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};

    fn at(offset_secs: i64) -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 9, 3, 12, 0, 0).unwrap() + Duration::seconds(offset_secs)
    }

    fn naive(offset_secs: i64) -> NaiveDateTime {
        NaiveDateTime::parse_from_str("2026-09-03 12:00:00", "%Y-%m-%d %H:%M:%S").unwrap()
            + Duration::seconds(offset_secs)
    }

    fn local(offset_secs: i64) -> DateTime<Local> {
        Local
            .from_local_datetime(&naive(offset_secs))
            .single()
            .unwrap()
    }

    #[test]
    fn trusted_clock_returns_finished_decision() {
        let decision = decide(Some(naive(0)), at(0));
        assert_eq!(decision.measured_at, local(0));
        assert_eq!(decision.source, ClockSource::Scale);
        assert_eq!(decision.source.as_str(), "scale");
        assert_eq!(decision.drift_secs, Some(0));
        assert_eq!(
            decision.measurement_date,
            NaiveDate::from_ymd_opt(2026, 9, 3).unwrap()
        );
    }

    #[test]
    fn drift_reported_with_sign() {
        // Frame 90 s behind receive time: drift +90.
        assert_eq!(decide(Some(naive(-90)), at(0)).drift_secs, Some(90));
        // Frame an hour ahead: drift -3600.
        assert_eq!(decide(Some(naive(3600)), at(0)).drift_secs, Some(-3600));
    }

    #[test]
    fn missing_timestamp_is_receiver_clocked_with_minute_stable_key() {
        let received = at(44);
        let decision = decide(None, received);
        assert_eq!(decision.source, ClockSource::Receiver);
        assert_eq!(decision.source.as_str(), "receiver");
        assert_eq!(decision.drift_secs, None);
        assert_eq!(decision.measured_at, truncate_to_minute(received));
        assert_eq!(decision.measurement_date, decision.measured_at.date_naive());
    }

    #[test]
    fn wild_drift_is_receiver_clocked() {
        assert_eq!(
            decide(Some(naive(-25 * 3600)), at(0)).source,
            ClockSource::Receiver
        );
        assert_eq!(
            decide(Some(naive(25 * 3600)), at(0)).source,
            ClockSource::Receiver
        );
        let close = decide(Some(naive(-23 * 3600)), at(0));
        assert_eq!(close.source, ClockSource::Scale);
        assert_eq!(close.drift_secs, Some(23 * 3600));
    }

    #[test]
    fn implausible_year_is_receiver_clocked() {
        // Year range trust (2000-2099) lives here, not in the frame parser.
        let stale = NaiveDateTime::parse_from_str("1999-09-03 12:00", "%Y-%m-%d %H:%M").unwrap();
        let decision = decide(Some(stale), at(0));
        assert_eq!(decision.source, ClockSource::Receiver);
        assert_eq!(decision.drift_secs, None);

        let future = NaiveDateTime::parse_from_str("2100-09-03 12:00", "%Y-%m-%d %H:%M").unwrap();
        assert_eq!(decide(Some(future), at(0)).source, ClockSource::Receiver);
    }

    #[test]
    fn measurement_date_follows_measured_at() {
        // Scale-clocked: the frame's date ages the profile (ADR-0001).
        let frame_day =
            NaiveDateTime::parse_from_str("2026-09-02 23:59", "%Y-%m-%d %H:%M").unwrap();
        let received = Local.with_ymd_and_hms(2026, 9, 3, 0, 1, 0).unwrap();
        let decision = decide(Some(frame_day), received);
        assert_eq!(decision.source, ClockSource::Scale);
        assert_eq!(
            decision.measurement_date,
            NaiveDate::from_ymd_opt(2026, 9, 2).unwrap()
        );
    }

    #[test]
    fn truncation_stabilizes_the_dedup_key() {
        let base = Local.with_ymd_and_hms(2026, 9, 3, 12, 46, 44).unwrap()
            + chrono::Duration::nanoseconds(276_790_820);
        let truncated = truncate_to_minute(base);
        assert_eq!(truncated.second(), 0);
        assert_eq!(truncated.nanosecond(), 0);
        assert_eq!(truncated.minute(), 46);
        assert_eq!(truncate_to_minute(truncated), truncated);
    }
}

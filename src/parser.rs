//! BLE frame parsing for the Xiaomi Mi Body Composition Scale 2 (XMTZC05HM).
//!
//! The scale broadcasts a 13-byte service-data frame under the Body Composition
//! Service UUID 0x181B:
//!
//! ```text
//! byte 0       unit flags (bit 0 = lbs); 0x02 = kg, 0x03 = lbs
//! byte 1       flags: bit 5 (0x20) = measurement stabilized,
//!                   bit 1 (0x02) = impedance value present,
//!                   bit 6 (0x40) = catty (jin) unit,
//!                   bit 7 (0x80) = weight removed
//! bytes 2-3    year, little endian
//! bytes 4-7    month, day, hour, minute
//! bytes 9-10   impedance in ohm, high byte at index 10
//! bytes 11-12  weight raw value, little endian; kg = raw / 200
//! ```
//!
//! Layout derived from lolouk44/xiaomi_mi_scale, barkayshahar/mi-scale-automation
//! and oliexdev/openScale (MiScaleHandler.kt).

use chrono::{Datelike, NaiveDate, NaiveDateTime, Timelike};

pub const BODY_COMPOSITION_SERVICE_UUID: uuid::Uuid =
    uuid::uuid!("0000181b-0000-1000-8000-00805f9b34fb");

/// Body Composition History characteristic: the scale's stored stabilized
/// records, pulled over GATT to recover a weigh-in whose stabilized frame
/// never arrived live. WRITE + NOTIFY under the Body Composition service.
///
/// Protocol (validated live on the XMTZC05HM 2026-09-03 — see `src/history.rs`
/// and ADR-0002):
/// `0x01 + u32 device-id` → count probe, `0x02` → fetch, `0x03` → stop,
/// `0x04 + u32 device-id` → ack and advance the scale's per-device position.
pub const BODY_COMPOSITION_HISTORY_UUID: uuid::Uuid =
    uuid::uuid!("00002a2f-0000-3512-2118-0009af100700");

/// Canonical history-entry length, confirmed live on the XMTZC05HM
/// (2026-09-03 spike): entries are byte-identical 13-byte frames — same
/// layout as the advertisements, including both control bytes and the
/// seconds byte. (Reverse-engineering docs describe a 12-byte variant
/// without the first control byte; this firmware does not send it.)
pub const HISTORY_ENTRY_LEN: usize = 13;

const FLAG_STABILIZED: u8 = 0x20;
const FLAG_HAS_IMPEDANCE: u8 = 0x02;
const FLAG_IS_CATTY: u8 = 0x40;
const FLAG_REMOVED: u8 = 0x80;

const MIN_IMPEDANCE: u16 = 1;
const MAX_IMPEDANCE: u16 = 3000;

const LBS_TO_KG: f64 = 0.45359237;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Kg,
    Lbs,
    Jin,
}

impl Unit {
    pub fn as_str(self) -> &'static str {
        match self {
            Unit::Kg => "kg",
            Unit::Lbs => "lbs",
            Unit::Jin => "jin",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Measurement {
    pub weight_kg: f64,
    pub impedance_ohm: Option<u16>,
    pub stabilized: bool,
    pub unit: Unit,
    /// Timestamp embedded in the frame (the scale's own clock); naive, no timezone.
    pub timestamp: Option<NaiveDateTime>,
}

/// Round to 2 decimal places with round-half-to-even semantics.
pub fn round2(value: f64) -> f64 {
    format!("{value:.2}").parse().unwrap()
}

fn le_u16(low: u8, high: u8) -> u16 {
    ((high as u16) << 8) | low as u16
}

fn parse_frame_timestamp(payload: &[u8]) -> Option<NaiveDateTime> {
    // Shape only: a constructible calendar instant or nothing. Whether the
    // year is plausible (2000-2099) and close to receive time is the clock
    // module's trust decision, not the parser's.
    let year = le_u16(payload[2], payload[3]);
    let date = NaiveDate::from_ymd_opt(year as i32, payload[4] as u32, payload[5] as u32)?;
    date.and_hms_opt(payload[6] as u32, payload[7] as u32, 0)
}

/// Body composition service data (0x181B): weight + impedance.
pub fn parse_body_composition_frame(payload: &[u8]) -> Option<Measurement> {
    if payload.len() < 13 {
        return None;
    }
    let unit_byte = payload[0];
    let flags = payload[1];
    let raw_weight = le_u16(payload[11], payload[12]);

    let (unit, weight_kg) = if flags & FLAG_IS_CATTY != 0 {
        (Unit::Jin, f64::from(raw_weight) * 0.005)
    } else if unit_byte & 0x01 != 0 {
        (Unit::Lbs, f64::from(raw_weight) / 100.0 * LBS_TO_KG)
    } else {
        (Unit::Kg, f64::from(raw_weight) / 200.0)
    };

    let impedance = if flags & FLAG_HAS_IMPEDANCE != 0 {
        let value = le_u16(payload[9], payload[10]);
        (MIN_IMPEDANCE..=MAX_IMPEDANCE)
            .contains(&value)
            .then_some(value)
    } else {
        None
    };

    let stabilized = flags & FLAG_STABILIZED != 0 && flags & FLAG_REMOVED == 0;
    Some(Measurement {
        weight_kg: round2(weight_kg),
        impedance_ohm: impedance,
        stabilized,
        unit,
        timestamp: parse_frame_timestamp(payload),
    })
}

/// One stored stabilized record from the history characteristic.
///
/// Confirmed live on the XMTZC05HM (2026-09-03 spike): entries are
/// byte-identical 13-byte frames — the same layout as
/// [`parse_body_composition_frame`], seconds byte included (byte 8):
///
/// ```text
/// byte 0       unit flags (bit 0 = lbs); 0x02 = kg, 0x03 = lbs
/// byte 1       flags: bit 5 (0x20) = measurement stabilized,
///                   bit 1 (0x02) = impedance value present,
///                   bit 6 (0x40) = catty (jin) unit,
///                   bit 7 (0x80) = weight removed
/// bytes 2-3    year, little endian
/// bytes 4-7    month, day, hour, minute
/// byte 8       seconds
/// bytes 9-10   impedance in ohm, high byte at index 10
/// bytes 11-12  weight raw value, little endian; kg = raw / 200
/// ```
///
/// History entries are stabilized records by construction — the scale only
/// stores finished weigh-ins — so the result is marked stabilized unless the
/// removed bit is set (defensive: a stored removal, if ever seen, must not
/// become a measurement). The seconds byte is parsed but dropped from the
/// timestamp: the clock decision works at minute resolution, and the dedup
/// key must match the equivalent live frame's minute-exact `measured_at`.
///
/// This delegates to [`parse_body_composition_frame`] — the layout is the
/// layout — so unit/impedance decoding cannot drift between the two paths;
/// the seconds byte (byte 8) is the only history-specific handling.
pub fn parse_history_entry(payload: &[u8]) -> Option<Measurement> {
    parse_body_composition_frame(payload)
}

/// Build the 10-byte Current Time Service payload written to characteristic
/// 0x2A2B to set the scale's clock (same as the OpenScale app does).
pub fn current_time_payload(now: NaiveDateTime) -> [u8; 10] {
    // Weekday: 1 = Monday .. 7 = Sunday (ISO-8601 day-of-week).
    let weekday = now.weekday().number_from_monday() as u8;
    [
        (now.year() & 0xFF) as u8,
        ((now.year() >> 8) & 0xFF) as u8,
        now.month() as u8,
        now.day() as u8,
        now.hour() as u8,
        now.minute() as u8,
        now.second() as u8,
        weekday,
        0, // fractions of a second
        0, // adjustment reason
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_body_frame(
        unit: u8,
        flags: u8,
        impedance: Option<u16>,
        weight_raw: u16,
        date: Option<NaiveDateTime>,
    ) -> Vec<u8> {
        let mut frame = vec![0u8; 13];
        frame[0] = unit;
        frame[1] = flags;
        if let Some(date) = date {
            frame[2] = (date.year() & 0xFF) as u8;
            frame[3] = ((date.year() >> 8) & 0xFF) as u8;
            frame[4] = date.month() as u8;
            frame[5] = date.day() as u8;
            frame[6] = date.hour() as u8;
            frame[7] = date.minute() as u8;
        }
        if let Some(impedance) = impedance {
            frame[9] = (impedance & 0xFF) as u8;
            frame[10] = (impedance >> 8) as u8;
        }
        frame[11] = (weight_raw & 0xFF) as u8;
        frame[12] = (weight_raw >> 8) as u8;
        frame
    }

    #[test]
    fn kg_stabilized_with_impedance() {
        let m = parse_body_composition_frame(&make_body_frame(0x02, 0x22, Some(500), 15000, None))
            .unwrap();
        assert_eq!(
            m,
            Measurement {
                weight_kg: 75.0,
                impedance_ohm: Some(500),
                stabilized: true,
                unit: Unit::Kg,
                timestamp: None
            }
        );
    }

    #[test]
    fn not_stabilized() {
        let m = parse_body_composition_frame(&make_body_frame(0x02, 0x02, Some(500), 15000, None))
            .unwrap();
        assert!(!m.stabilized);
        assert_eq!(m.weight_kg, 75.0);
    }

    #[test]
    fn no_impedance_flag() {
        let m = parse_body_composition_frame(&make_body_frame(0x02, 0x20, Some(500), 15000, None))
            .unwrap();
        assert_eq!(m.impedance_ohm, None);
        assert!(m.stabilized);
    }

    #[test]
    fn impedance_out_of_range_treated_as_missing() {
        let m = parse_body_composition_frame(&make_body_frame(0x02, 0x22, Some(3500), 15000, None))
            .unwrap();
        assert_eq!(m.impedance_ohm, None);
        let m = parse_body_composition_frame(&make_body_frame(0x02, 0x22, Some(0), 15000, None))
            .unwrap();
        assert_eq!(m.impedance_ohm, None);
    }

    #[test]
    fn lbs() {
        let m = parse_body_composition_frame(&make_body_frame(0x03, 0x22, Some(500), 16500, None))
            .unwrap();
        assert_eq!(m.unit, Unit::Lbs);
        assert_eq!(m.weight_kg, 74.84);
    }

    #[test]
    fn catty() {
        let m = parse_body_composition_frame(&make_body_frame(0x02, 0x60, Some(500), 15000, None))
            .unwrap();
        assert_eq!(m.unit, Unit::Jin);
        assert_eq!(m.weight_kg, 75.0);
    }

    #[test]
    fn removed_flag_not_stabilized() {
        let m = parse_body_composition_frame(&make_body_frame(0x02, 0xA2, Some(500), 15000, None))
            .unwrap();
        assert!(!m.stabilized);
    }

    #[test]
    fn timestamp_from_frame() {
        let when = NaiveDateTime::parse_from_str("2026-09-02 21:30", "%Y-%m-%d %H:%M").unwrap();
        let m = parse_body_composition_frame(&make_body_frame(
            0x02,
            0x22,
            Some(500),
            15000,
            Some(when),
        ))
        .unwrap();
        assert_eq!(m.timestamp, Some(when));
    }

    #[test]
    fn invalid_timestamp_is_none() {
        let mut frame = make_body_frame(0x02, 0x22, Some(500), 15000, None);
        frame[2] = 0;
        frame[3] = 0;
        let m = parse_body_composition_frame(&frame).unwrap();
        assert_eq!(m.timestamp, None);
    }

    #[test]
    fn unknown_unit_still_kg() {
        let m = parse_body_composition_frame(&make_body_frame(0x06, 0x22, Some(500), 15000, None))
            .unwrap();
        assert_eq!(m.unit, Unit::Kg);
        assert_eq!(m.weight_kg, 75.0);
    }

    #[test]
    fn short_frame_rejected() {
        assert!(parse_body_composition_frame(&[0x02, 0x22, 0x00]).is_none());
    }

    #[test]
    fn current_time_payload_layout() {
        let when =
            NaiveDateTime::parse_from_str("2026-09-03 14:05:09", "%Y-%m-%d %H:%M:%S").unwrap();
        // 2026-09-03 was a Thursday (ISO weekday 4).
        assert_eq!(
            current_time_payload(when),
            [0xEA, 0x07, 9, 3, 14, 5, 9, 4, 0, 0]
        );
    }

    // ── history entries: byte-identical 13-byte frames (spike-confirmed) ──
    //
    // Synthetic protocol fixtures: 75 kg, 500 ohm, arbitrary timestamps.
    const SPIKE_HISTORY_HEX: &str = "0226e80701010c250ff401983a";
    const SPIKE_LIVE_STABILIZED_HEX: &str = "0226e80701010c250ff401983a";
    const SPIKE_HISTORY_EARLIER_HEX: &str = "0226e80701010c2308f401983a";

    fn history_when() -> NaiveDateTime {
        NaiveDateTime::parse_from_str("2026-09-02 21:30", "%Y-%m-%d %H:%M").unwrap()
    }

    #[test]
    fn history_matches_the_equivalent_live_frame() {
        // Same bytes through both parsers → same measurement. This is the
        // invariant the spike established; any layout drift breaks it loudly.
        let when = history_when();
        let frame = make_body_frame(0x02, 0x22, Some(500), 15000, Some(when));
        let live = parse_body_composition_frame(&frame).unwrap();
        let stored = parse_history_entry(&frame).unwrap();
        assert_eq!(stored, live);
        assert!(stored.stabilized);
    }

    #[test]
    fn history_seconds_byte_does_not_enter_the_timestamp() {
        // Byte 8 is seconds (0-59 live); the frame parser ignores it, so two
        // entries differing only in seconds decode identically — the dedup
        // key stays minute-stable.
        let when = history_when();
        let mut a = make_body_frame(0x02, 0x22, Some(500), 15000, Some(when));
        let mut b = a.clone();
        a[8] = 5;
        b[8] = 47;
        assert_eq!(parse_history_entry(&a), parse_history_entry(&b));
        assert_eq!(parse_history_entry(&a).unwrap().timestamp, Some(when));
    }

    #[test]
    fn history_spike_stabilized_fixture() {
        let raw = hex::decode(SPIKE_HISTORY_HEX).unwrap();
        assert_eq!(raw.len(), HISTORY_ENTRY_LEN);
        let m = parse_history_entry(&raw).unwrap();
        assert!(m.stabilized);
        assert_eq!(m.unit, Unit::Kg);
        assert_eq!(m.weight_kg, 75.0);
        assert_eq!(m.impedance_ohm, Some(500));
        assert_eq!(
            m.timestamp,
            Some(
                NaiveDateTime::parse_from_str("2024-01-01 12:37", "%Y-%m-%d %H:%M").unwrap()
            )
        );
        // And the frame parser agrees byte-for-byte (delegation, not drift).
        assert_eq!(m, parse_body_composition_frame(&raw).unwrap());
    }

    #[test]
    fn history_spike_earlier_entry_fixture() {
        // An earlier synthetic history entry.
        let raw = hex::decode(SPIKE_HISTORY_EARLIER_HEX).unwrap();
        let m = parse_history_entry(&raw).unwrap();
        assert!(m.stabilized);
        assert_eq!(m.weight_kg, 75.0);
        assert_eq!(m.impedance_ohm, Some(500));
        assert_eq!(
            m.timestamp,
            Some(
                NaiveDateTime::parse_from_str("2024-01-01 12:35", "%Y-%m-%d %H:%M").unwrap()
            )
        );
    }

    #[test]
    fn history_spike_entry_matches_its_live_twin() {
        // The same synthetic frame received live and via history
        // decodes to the same weight / impedance / minute.
        let live = hex::decode(SPIKE_LIVE_STABILIZED_HEX).unwrap();
        let stored = hex::decode(SPIKE_HISTORY_HEX).unwrap();
        let a = parse_body_composition_frame(&live).unwrap();
        let b = parse_history_entry(&stored).unwrap();
        assert_eq!(a.weight_kg, b.weight_kg);
        assert_eq!(a.impedance_ohm, b.impedance_ohm);
        assert_eq!(a.timestamp, b.timestamp);
    }

    #[test]
    fn history_unstabilized_shape_still_gated() {
        // Stored entries should always be stabilized; the parser still
        // reports shape honestly so capture gates on it like live frames.
        let mut frame = make_body_frame(0x02, 0x02, Some(500), 15000, Some(history_when()));
        frame[8] = 31;
        let m = parse_history_entry(&frame).unwrap();
        assert!(!m.stabilized);
    }

    #[test]
    fn history_wrong_lengths_rejected() {
        assert!(parse_history_entry(&[0u8; 5]).is_none());
        // The documented-but-unseen 12-byte variant is NOT accepted: this
        // firmware sends 13 bytes, and accepting both would risk decoding
        // one layout as the other.
        assert!(parse_history_entry(&[0u8; 12]).is_none());
        assert!(parse_history_entry(&[]).is_none());
    }
}

//! Weight-range based profile assignment and metric resolution.
//!
//! The scale cannot identify who is standing on it; assignment is a heuristic.
//! Weight windows are the guardrails: zero matches stay guest, a single match
//! wins with no further I/O. Overlapping windows are the only case that
//! consults history — each candidate's most recent measurement strictly before
//! the current one scores the fit (weight first, impedance second); a tie, a
//! thin margin, or missing history stays guest, never a guess — corrections
//! are the sister project's job.
//! This module is also the one home for turning a profile into body metrics:
//! the invalid-sex policy, the metrics-error policy, and the storage policy
//! live here once, not at every call site.

use std::collections::BTreeMap;

use chrono::{DateTime, FixedOffset, NaiveDate};

use crate::metrics::{MeasurementMetrics, Sex, age_years, compute_body_metrics};

#[derive(Debug, Clone)]
pub struct Profile {
    pub id: i64,
    pub name: String,
    pub sex: String,
    pub height_cm: f64,
    pub dob: NaiveDate,
    pub weight_min: Option<f64>,
    pub weight_max: Option<f64>,
}

impl Profile {
    pub fn sex(&self) -> Option<Sex> {
        Sex::parse(&self.sex)
    }

    /// Fractional age in years as of the given date — the *measurement date*
    /// (ADR-0001), never the compute date, so replay and recompute are
    /// deterministic.
    pub fn age_years(&self, on: NaiveDate) -> f64 {
        age_years(self.dob, on)
    }
}

/// Profiles whose (exclusive) window contains the weight; `None`-bounded edges
/// are unbounded. Preserves profile order.
fn window_matches(profiles: &[Profile], weight_kg: f64) -> Vec<&Profile> {
    profiles
        .iter()
        .filter(|profile| {
            let above_min = profile.weight_min.is_none_or(|min| weight_kg > min);
            let below_max = profile.weight_max.is_none_or(|max| weight_kg < max);
            above_min && below_max
        })
        .collect()
}

/// Single-window probe used by the unit tests below: the first (and only)
/// match wins, zero or overlapping matches yield `None`. Live capture uses
/// [`resolve`], which breaks overlaps via history.
#[cfg(test)]
fn assign(profiles: &[Profile], weight_kg: f64) -> Option<&Profile> {
    let matches = window_matches(profiles, weight_kg);
    if matches.len() == 1 {
        Some(matches[0])
    } else {
        None
    }
}

/// Resolve one profile into body metrics. An invalid sex or a metrics error
/// yields `None` with a warning — a measurement is never dropped over its
/// metrics. `on_date` is the measurement date that ages the profile.
pub fn metrics_for(
    profile: &Profile,
    weight_kg: f64,
    impedance_ohm: Option<u16>,
    on_date: NaiveDate,
) -> Option<MeasurementMetrics> {
    let Some(sex) = profile.sex() else {
        tracing::warn!(
            "profile {} has invalid sex {:?}; metrics skipped",
            profile.name,
            profile.sex
        );
        return None;
    };
    match compute_body_metrics(
        weight_kg,
        impedance_ohm,
        sex,
        profile.height_cm,
        profile.age_years(on_date),
    ) {
        Ok(metrics) => Some(metrics),
        Err(error) => {
            tracing::warn!("body metrics skipped: {error}");
            None
        }
    }
}

/// One past measurement behind the history seam: the raw signals the scale
/// measures, plus when they were measured. Deliberately mirrors the dedup key
/// triple so future signals need no seam change.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HistoryPoint {
    pub weight_kg: f64,
    pub impedance_ohm: Option<i32>,
    pub measured_at: DateTime<FixedOffset>,
}

/// The weigh-in under decision, built by capture after the impedance-attach
/// rule and the clock decision; never derived inside this module.
#[derive(Debug, Clone, Copy)]
pub struct WeighIn {
    pub weight_kg: f64,
    pub impedance_ohm: Option<u16>,
    /// Dedup-key time from the clock decision (scale or receiver clock).
    pub measured_at: DateTime<FixedOffset>,
    /// The date that ages a profile (ADR-0001).
    pub measurement_date: NaiveDate,
}

/// Which derived calculations reach the database. The single home for both
/// the TOML shape (`[metrics] store = "all" | "weight-only" | "none"`) and
/// the runtime semantics — the representation is the meaning, so no
/// conversion layer. Scoring always sees the full transient set; only
/// persistence is trimmed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize)]
pub enum MetricsPolicy {
    /// Store all 12 metric columns (today's behavior).
    #[default]
    #[serde(rename = "all")]
    All,
    /// Store only the 4 weight-only columns; NULL the 8 impedance-derived.
    #[serde(rename = "weight-only")]
    WeightOnly,
    /// Store no metrics; still compute transiently for the tie-break.
    #[serde(rename = "none")]
    None,
}

impl MetricsPolicy {
    pub fn keep_all() -> Self {
        MetricsPolicy::All
    }

    pub fn weight_only() -> Self {
        MetricsPolicy::WeightOnly
    }

    pub fn none() -> Self {
        MetricsPolicy::None
    }

    /// Project transient metrics to storable metrics. `None` input stays
    /// `None`; [`MetricsPolicy::None`] maps everything to `None`.
    pub fn apply(&self, metrics: Option<MeasurementMetrics>) -> Option<MeasurementMetrics> {
        match self {
            MetricsPolicy::All => metrics,
            MetricsPolicy::None => None,
            MetricsPolicy::WeightOnly => {
                let mut metrics = metrics?;
                metrics.body_fat_pct = None;
                metrics.water_pct = None;
                metrics.bone_mass_kg = None;
                metrics.muscle_mass_kg = None;
                metrics.protein_pct = None;
                metrics.lean_body_mass_kg = None;
                metrics.metabolic_age = None;
                metrics.body_type = None;
                Some(metrics)
            }
        }
    }
}

/// Overlap context for the rare path. Constructed only by [`resolve`] on
/// overlap (`candidates.len() >= 2`); callers pass it through untouched.
#[derive(Debug)]
pub struct TieContext<'a> {
    pub candidates: Vec<&'a Profile>,
    pub weigh_in: WeighIn,
}

/// Fast-path outcome. `Done` is complete (no further I/O); `NeedsHistory` is
/// incomplete by design — the caller fetches history, then calls
/// [`resolve_with_history`].
#[derive(Debug)]
pub enum Resolution<'a> {
    Done(Option<&'a Profile>, Option<MeasurementMetrics>),
    NeedsHistory(TieContext<'a>),
}

/// How many past measurements per candidate the tie-break may see. Scoring
/// uses the newest; the cap bounds I/O and keeps the seam stable if scoring
/// later weighs more than one point.
pub const HISTORY_LIMIT: u32 = 5;

/// Minimum best-minus-runner-up margin (kilogram-equivalent) for a tie-break
/// win. Below it the overlap stays guest: too close to call is never a guess.
const MIN_MARGIN: f64 = 0.3;

/// Impedance normalization: this many ohms count as one kilogram in the
/// tie-break score. Heuristic, intentionally not a config knob — surprising
/// outcomes are diagnosed via debug-level score logs, not tuning.
const IMPEDANCE_SCALE_OHM: f64 = 500.0;

/// Classify a weigh-in against the weight windows. Zero matches → guest and
/// single matches → profile plus policy-filtered metrics complete with no
/// I/O; overlapping windows return [`Resolution::NeedsHistory`] and compute
/// nothing. Pure and sync.
pub fn resolve<'a>(
    profiles: &'a [Profile],
    weigh_in: WeighIn,
    policy: &MetricsPolicy,
) -> Resolution<'a> {
    let matches = window_matches(profiles, weigh_in.weight_kg);
    match matches.len() {
        0 => Resolution::Done(None, None),
        1 => {
            let profile = matches[0];
            let metrics = metrics_for(
                profile,
                weigh_in.weight_kg,
                weigh_in.impedance_ohm,
                weigh_in.measurement_date,
            );
            Resolution::Done(Some(profile), policy.apply(metrics))
        }
        _ => Resolution::NeedsHistory(TieContext {
            candidates: matches,
            weigh_in,
        }),
    }
}

/// Break an overlap using history strictly before the current measurement.
/// Each candidate scores its newest point with `measured_at < before`; a
/// candidate with no such point, an exact tie, or a margin below
/// `MIN_MARGIN` resolves to guest. The current frame and its re-broadcasts
/// share one dedup-key time, so neither ever observes itself and replay sees
/// the identical history prefix regardless of wall-clock replay time.
/// Deterministic in (candidates, current, histories, measurement date).
pub fn resolve_with_history<'a>(
    tie: &TieContext<'a>,
    histories: &BTreeMap<i64, Vec<HistoryPoint>>,
    policy: &MetricsPolicy,
) -> (Option<&'a Profile>, Option<MeasurementMetrics>) {
    let mut scored: Vec<(&'a Profile, f64)> = Vec::with_capacity(tie.candidates.len());
    for candidate in tie.candidates.iter().copied() {
        let recent = histories
            .get(&candidate.id)
            .into_iter()
            .flatten()
            .filter(|point| point.measured_at < tie.weigh_in.measured_at)
            .max_by(|a, b| a.measured_at.cmp(&b.measured_at));
        // No past to compare against: a fair comparison is impossible, so the
        // overlap stays guest rather than rewarding the profile with history.
        let Some(recent) = recent else {
            return (None, None);
        };
        scored.push((candidate, score(&tie.weigh_in, recent)));
    }
    scored.sort_by(|a, b| {
        a.1.partial_cmp(&b.1)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let Some((winner, winner_score)) = scored.first().copied() else {
        return (None, None);
    };
    if scored.len() > 1 {
        let runner_up = scored[1].1;
        if runner_up - winner_score < MIN_MARGIN {
            tracing::debug!(
                "tie-break unresolved (margin {margin:.3} below {MIN_MARGIN:.3}); recording as guest",
                margin = runner_up - winner_score,
            );
            return (None, None);
        }
    }
    tracing::debug!(
        "tie-break winner {} (score {winner_score:.3})",
        winner.name,
    );
    let metrics = metrics_for(
        winner,
        tie.weigh_in.weight_kg,
        tie.weigh_in.impedance_ohm,
        tie.weigh_in.measurement_date,
    );
    (Some(winner), policy.apply(metrics))
}

/// Fit of the current weigh-in against one candidate's most recent past
/// measurement, in kilogram-equivalent (lower wins). Raw signals only:
/// derived metrics are a deterministic function of (weight, impedance,
/// profile), so scoring raw captures the same information without
/// recomputing metrics per candidate — impedance still decides ties even
/// when derived storage is disabled.
fn score(current: &WeighIn, recent: &HistoryPoint) -> f64 {
    let weight_dist = (current.weight_kg - recent.weight_kg).abs();
    let impedance_dist = match (current.impedance_ohm, recent.impedance_ohm) {
        (Some(now), Some(was)) => (f64::from(now) - f64::from(was)).abs() / IMPEDANCE_SCALE_OHM,
        _ => 0.0,
    };
    weight_dist + impedance_dist
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn profile(id: i64, min: Option<f64>, max: Option<f64>) -> Profile {
        Profile {
            id,
            name: format!("p{id}"),
            sex: "male".into(),
            height_cm: 175.0,
            dob: NaiveDate::from_ymd_opt(1996, 1, 1).unwrap(),
            weight_min: min,
            weight_max: max,
        }
    }

    #[test]
    fn first_matching_window_wins() {
        let profiles = vec![
            profile(1, Some(30.0), Some(80.0)),
            profile(2, Some(80.0), Some(150.0)),
        ];
        assert_eq!(assign(&profiles, 70.0).unwrap().id, 1);
        assert_eq!(assign(&profiles, 85.0).unwrap().id, 2);
    }

    #[test]
    fn boundaries_are_exclusive() {
        let profiles = vec![
            profile(1, Some(30.0), Some(80.0)),
            profile(2, Some(80.0), Some(150.0)),
        ];
        // Exactly on the shared edge: falls in no window (both exclude it).
        assert!(assign(&profiles, 80.0).is_none());
    }

    #[test]
    fn unbounded_edges() {
        let profiles = vec![profile(1, None, Some(80.0)), profile(2, Some(80.0), None)];
        assert_eq!(assign(&profiles, 20.0).unwrap().id, 1);
        assert_eq!(assign(&profiles, 200.0).unwrap().id, 2);
    }

    #[test]
    fn no_match_is_guest() {
        let profiles = vec![profile(1, Some(30.0), Some(80.0))];
        assert!(assign(&profiles, 90.0).is_none());
    }

    #[test]
    fn overlapping_windows_are_ambiguous() {
        let profiles = vec![
            profile(1, Some(30.0), Some(80.0)),
            profile(2, Some(40.0), Some(90.0)),
        ];
        assert!(assign(&profiles, 50.0).is_none());
        assert!(assign(&profiles, 35.0).is_some());
    }

    fn on_date() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 9, 3).unwrap()
    }

    fn at(day: u32, hour: u32, minute: u32) -> DateTime<FixedOffset> {
        FixedOffset::east_opt(0)
            .unwrap()
            .with_ymd_and_hms(2026, 9, day, hour, minute, 0)
            .unwrap()
    }

    fn weigh_in(
        weight_kg: f64,
        impedance_ohm: Option<u16>,
        measured_at: DateTime<FixedOffset>,
    ) -> WeighIn {
        WeighIn {
            weight_kg,
            impedance_ohm,
            measured_at,
            measurement_date: on_date(),
        }
    }

    fn point(
        weight_kg: f64,
        impedance_ohm: Option<i32>,
        measured_at: DateTime<FixedOffset>,
    ) -> HistoryPoint {
        HistoryPoint {
            weight_kg,
            impedance_ohm,
            measured_at,
        }
    }

    fn histories(entries: Vec<(i64, Vec<HistoryPoint>)>) -> BTreeMap<i64, Vec<HistoryPoint>> {
        entries.into_iter().collect()
    }

    fn overlapping() -> Vec<Profile> {
        vec![
            profile(1, Some(30.0), Some(80.0)),
            profile(2, Some(40.0), Some(90.0)),
        ]
    }

    #[test]
    fn age_is_evaluated_as_of_the_given_date() {
        let mut p = profile(1, None, None);
        p.dob = NaiveDate::from_ymd_opt(1996, 1, 1).unwrap();
        let on = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        // ~30th birthday (age is fractional: days / 365.25)
        assert!((p.age_years(on) - 30.0).abs() < 0.01);
        // deterministic: the same measurement date always yields the same age
        assert_eq!(p.age_years(on), p.age_years(on));
        // it tracks the measurement date, nothing else
        assert!(p.age_years(on.pred_opt().unwrap()) < p.age_years(on));
    }

    #[test]
    fn metrics_are_skipped_for_invalid_sex() {
        let mut p = profile(1, Some(40.0), Some(80.0));
        p.sex = "unspecified".into();
        assert!(metrics_for(&p, 75.0, Some(500), on_date()).is_none());
    }

    #[test]
    fn resolve_zero_match_is_guest_without_metrics() {
        let profiles = vec![profile(1, Some(40.0), Some(80.0))];
        let Resolution::Done(assigned, metrics) = resolve(
            &profiles,
            weigh_in(90.0, Some(500), at(3, 12, 0)),
            &MetricsPolicy::keep_all(),
        ) else {
            panic!("zero matches must complete without history");
        };
        assert!(assigned.is_none());
        assert!(metrics.is_none());
    }

    #[test]
    fn resolve_single_match_computes_metrics() {
        let profiles = vec![profile(1, Some(40.0), Some(80.0))];
        let Resolution::Done(assigned, metrics) = resolve(
            &profiles,
            weigh_in(75.0, Some(500), at(3, 12, 0)),
            &MetricsPolicy::keep_all(),
        ) else {
            panic!("single match must complete without history");
        };
        assert_eq!(assigned.unwrap().id, 1);
        let metrics = metrics.unwrap();
        assert!(metrics.body_fat_pct.is_some());
        assert!(metrics.body_type.is_some());
    }

    #[test]
    fn resolve_single_match_keeps_profile_when_metrics_fail() {
        let mut bad = profile(1, Some(40.0), Some(80.0));
        bad.sex = "unspecified".into();
        let Resolution::Done(assigned, metrics) = resolve(
            std::slice::from_ref(&bad),
            weigh_in(75.0, Some(500), at(3, 12, 0)),
            &MetricsPolicy::keep_all(),
        ) else {
            panic!("single match must complete without history");
        };
        // A measurement is never dropped over its metrics.
        assert_eq!(assigned.unwrap().id, 1);
        assert!(metrics.is_none());
    }

    #[test]
    fn resolve_weight_only_policy_strips_derived_but_keeps_weight_only() {
        let profiles = vec![profile(1, Some(40.0), Some(80.0))];
        let Resolution::Done(assigned, metrics) = resolve(
            &profiles,
            weigh_in(75.0, Some(500), at(3, 12, 0)),
            &MetricsPolicy::weight_only(),
        ) else {
            panic!("single match must complete without history");
        };
        assert_eq!(assigned.unwrap().id, 1);
        let metrics = metrics.unwrap();
        assert!(metrics.bmi > 0.0);
        assert!(metrics.bmr_kcal > 0.0);
        assert!(metrics.body_fat_pct.is_none());
        assert!(metrics.water_pct.is_none());
        assert!(metrics.muscle_mass_kg.is_none());
        assert!(metrics.metabolic_age.is_none());
        assert!(metrics.body_type.is_none());
    }

    #[test]
    fn resolve_overlap_needs_history() {
        let profiles = overlapping();
        let Resolution::NeedsHistory(tie) = resolve(
            &profiles,
            weigh_in(50.0, Some(500), at(3, 12, 0)),
            &MetricsPolicy::keep_all(),
        ) else {
            panic!("overlap must defer to history");
        };
        assert_eq!(tie.candidates.len(), 2);
    }

    #[test]
    fn tie_break_picks_the_nearer_history() {
        let profiles = overlapping();
        let now = at(3, 12, 0);
        let Resolution::NeedsHistory(tie) = resolve(
            &profiles,
            weigh_in(66.9, Some(505), now),
            &MetricsPolicy::keep_all(),
        ) else {
            panic!("overlap must defer to history");
        };
        let past = histories(vec![
            (1, vec![point(66.5, Some(500), at(2, 12, 0))]),
            (2, vec![point(72.0, Some(520), at(2, 12, 0))]),
        ]);
        let (winner, metrics) = resolve_with_history(&tie, &past, &MetricsPolicy::keep_all());
        assert_eq!(winner.unwrap().id, 1);
        assert!(metrics.is_some());
    }

    #[test]
    fn tie_break_uses_impedance_when_weight_is_equidistant() {
        let profiles = overlapping();
        let now = at(3, 12, 0);
        let Resolution::NeedsHistory(tie) = resolve(
            &profiles,
            weigh_in(70.0, Some(510), now),
            &MetricsPolicy::keep_all(),
        ) else {
            panic!("overlap must defer to history");
        };
        let past = histories(vec![
            (1, vec![point(70.0, Some(505), at(2, 12, 0))]),
            (2, vec![point(70.0, Some(700), at(2, 12, 0))]),
        ]);
        let (winner, _) = resolve_with_history(&tie, &past, &MetricsPolicy::keep_all());
        assert_eq!(winner.unwrap().id, 1);
    }

    #[test]
    fn tie_break_scores_newest_point_not_oldest() {
        let profiles = overlapping();
        let now = at(3, 12, 0);
        let Resolution::NeedsHistory(tie) = resolve(
            &profiles,
            weigh_in(67.0, None, now),
            &MetricsPolicy::keep_all(),
        ) else {
            panic!("overlap must defer to history");
        };
        // Candidate 1 has a stale near point but drifted far since; candidate
        // 2's latest is nearer. The newest point decides.
        let past = histories(vec![
            (
                1,
                vec![
                    point(67.0, None, at(1, 12, 0)),
                    point(80.0, None, at(2, 12, 0)),
                ],
            ),
            (2, vec![point(67.5, None, at(2, 12, 0))]),
        ]);
        let (winner, _) = resolve_with_history(&tie, &past, &MetricsPolicy::keep_all());
        assert_eq!(winner.unwrap().id, 2);
    }

    #[test]
    fn tie_break_exact_tie_stays_guest() {
        let profiles = overlapping();
        let now = at(3, 12, 0);
        let Resolution::NeedsHistory(tie) = resolve(
            &profiles,
            weigh_in(70.0, Some(500), now),
            &MetricsPolicy::keep_all(),
        ) else {
            panic!("overlap must defer to history");
        };
        let past = histories(vec![
            (1, vec![point(70.0, Some(500), at(2, 12, 0))]),
            (2, vec![point(70.0, Some(500), at(2, 12, 0))]),
        ]);
        let (winner, metrics) = resolve_with_history(&tie, &past, &MetricsPolicy::keep_all());
        assert!(winner.is_none());
        assert!(metrics.is_none());
    }

    #[test]
    fn tie_break_thin_margin_stays_guest() {
        let profiles = overlapping();
        let now = at(3, 12, 0);
        let Resolution::NeedsHistory(tie) = resolve(
            &profiles,
            weigh_in(70.0, None, now),
            &MetricsPolicy::keep_all(),
        ) else {
            panic!("overlap must defer to history");
        };
        // Scores 0.0 vs 0.2: below the margin, too close to call.
        let past = histories(vec![
            (1, vec![point(70.0, None, at(2, 12, 0))]),
            (2, vec![point(70.2, None, at(2, 12, 0))]),
        ]);
        let (winner, _) = resolve_with_history(&tie, &past, &MetricsPolicy::keep_all());
        assert!(winner.is_none());
    }

    #[test]
    fn tie_break_without_history_stays_guest() {
        let profiles = overlapping();
        let now = at(3, 12, 0);
        let Resolution::NeedsHistory(tie) = resolve(
            &profiles,
            weigh_in(70.0, Some(500), now),
            &MetricsPolicy::keep_all(),
        ) else {
            panic!("overlap must defer to history");
        };
        let (winner, metrics) =
            resolve_with_history(&tie, &histories(vec![]), &MetricsPolicy::keep_all());
        assert!(winner.is_none());
        assert!(metrics.is_none());
    }

    #[test]
    fn tie_break_one_sided_history_stays_guest() {
        let profiles = overlapping();
        let now = at(3, 12, 0);
        let Resolution::NeedsHistory(tie) = resolve(
            &profiles,
            weigh_in(70.0, Some(500), now),
            &MetricsPolicy::keep_all(),
        ) else {
            panic!("overlap must defer to history");
        };
        // Only one candidate has past: no fair comparison exists.
        let past = histories(vec![(1, vec![point(70.0, Some(500), at(2, 12, 0))])]);
        let (winner, _) = resolve_with_history(&tie, &past, &MetricsPolicy::keep_all());
        assert!(winner.is_none());
    }

    #[test]
    fn tie_break_ignores_history_at_or_after_the_current_time() {
        let profiles = overlapping();
        let now = at(3, 12, 0);
        let Resolution::NeedsHistory(tie) = resolve(
            &profiles,
            weigh_in(70.0, Some(500), now),
            &MetricsPolicy::keep_all(),
        ) else {
            panic!("overlap must defer to history");
        };
        // A point stamped at the current dedup-key time (e.g. the row replay
        // is about to re-observe) must not score: the strict `<` keeps replay
        // deterministic.
        let past = histories(vec![
            (1, vec![point(70.0, Some(500), now)]),
            (2, vec![point(70.0, Some(500), now)]),
        ]);
        let (winner, _) = resolve_with_history(&tie, &past, &MetricsPolicy::keep_all());
        assert!(winner.is_none());
    }

    #[test]
    fn tie_break_is_deterministic() {
        let profiles = overlapping();
        let now = at(3, 12, 0);
        let Resolution::NeedsHistory(tie) = resolve(
            &profiles,
            weigh_in(66.9, Some(505), now),
            &MetricsPolicy::keep_all(),
        ) else {
            panic!("overlap must defer to history");
        };
        let past = histories(vec![
            (1, vec![point(66.5, Some(500), at(2, 12, 0))]),
            (2, vec![point(72.0, Some(520), at(2, 12, 0))]),
        ]);
        let first = resolve_with_history(&tie, &past, &MetricsPolicy::keep_all());
        let second = resolve_with_history(&tie, &past, &MetricsPolicy::keep_all());
        assert_eq!(first.0.map(|p| p.id), second.0.map(|p| p.id));
        assert_eq!(first.1, second.1);
    }

    #[test]
    fn tie_break_winner_metrics_follow_the_storage_policy() {
        let profiles = overlapping();
        let now = at(3, 12, 0);
        let Resolution::NeedsHistory(tie) = resolve(
            &profiles,
            weigh_in(66.9, Some(505), now),
            &MetricsPolicy::weight_only(),
        ) else {
            panic!("overlap must defer to history");
        };
        let past = histories(vec![
            (1, vec![point(66.5, Some(500), at(2, 12, 0))]),
            (2, vec![point(72.0, Some(520), at(2, 12, 0))]),
        ]);
        // Impedance decides the winner even though derived storage is off.
        let (winner, metrics) =
            resolve_with_history(&tie, &past, &MetricsPolicy::weight_only());
        assert_eq!(winner.unwrap().id, 1);
        let metrics = metrics.unwrap();
        assert!(metrics.bmi > 0.0);
        assert!(metrics.body_fat_pct.is_none());
        assert!(metrics.body_type.is_none());
    }

    #[test]
    fn policy_apply_is_identity_when_keeping_all() {
        let single = profile(1, Some(40.0), Some(80.0));
        let full = metrics_for(&single, 75.0, Some(500), on_date()).unwrap();
        let policy = MetricsPolicy::keep_all();
        assert_eq!(policy.apply(Some(full.clone())), Some(full));
        assert_eq!(policy.apply(None), None);
    }

    #[test]
    fn policy_none_stores_nothing() {
        let single = profile(1, Some(40.0), Some(80.0));
        let full = metrics_for(&single, 75.0, Some(500), on_date()).unwrap();
        assert_eq!(MetricsPolicy::none().apply(Some(full)), None);
    }

    #[test]
    fn policy_deserializes_from_config_strings() {
        assert_eq!(
            serde_json::from_str::<MetricsPolicy>(r#""all""#).unwrap(),
            MetricsPolicy::All
        );
        assert_eq!(
            serde_json::from_str::<MetricsPolicy>(r#""weight-only""#).unwrap(),
            MetricsPolicy::WeightOnly
        );
        assert_eq!(
            serde_json::from_str::<MetricsPolicy>(r#""none""#).unwrap(),
            MetricsPolicy::None
        );
    }
}

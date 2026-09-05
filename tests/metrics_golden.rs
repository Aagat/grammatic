//! Golden tests: metrics getters must match the committed fixtures
//! bit-for-bit on full-precision outputs.

use chrono::NaiveDate;
use grammatic::metrics::{BodyMetrics, Sex, age_years};
use serde::Deserialize;

#[derive(Deserialize, Debug)]
struct GoldenCase {
    weight: f64,
    impedance: Option<u16>,
    sex: String,
    height: f64,
    age: f64,
    expected: GoldenExpected,
}

#[derive(Deserialize, Debug)]
struct GoldenExpected {
    bmi: f64,
    bmr_kcal: f64,
    visceral_fat: f64,
    ideal_weight_kg: f64,
    body_fat_pct: Option<f64>,
    water_pct: Option<f64>,
    bone_mass_kg: Option<f64>,
    muscle_mass_kg: Option<f64>,
    protein_pct: Option<f64>,
    lean_body_mass_kg: Option<f64>,
    // Fixtures store metabolic age rounded to an integer.
    metabolic_age: Option<f64>,
    body_type: Option<String>,
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9
}

#[test]
fn metrics_match_golden_fixtures() {
    let raw = include_str!("fixtures/metrics_golden.json");
    let cases: Vec<GoldenCase> = serde_json::from_str(raw).expect("valid golden fixture");
    assert!(cases.len() > 1000, "fixture should be substantial");

    for case in &cases {
        let sex = Sex::parse(&case.sex).expect("valid sex in fixture");
        // Absent impedance is treated as 0 (weight-only metrics).
        let impedance = case.impedance.map(i32::from).unwrap_or(0);
        let metrics = BodyMetrics::new(case.weight, case.height, case.age, sex, impedance)
            .unwrap_or_else(|e| panic!("BodyMetrics::new failed for {case:?}: {e}"));
        let expected = &case.expected;

        assert!(
            close(metrics.bmi(), expected.bmi),
            "bmi mismatch for {case:?}"
        );
        assert!(
            close(metrics.bmr(), expected.bmr_kcal),
            "bmr mismatch for {case:?}"
        );
        assert!(
            close(metrics.visceral_fat(), expected.visceral_fat),
            "visceral_fat mismatch for {case:?}"
        );
        assert!(
            close(metrics.ideal_weight(), expected.ideal_weight_kg),
            "ideal_weight mismatch for {case:?}"
        );

        let impedance_derived = case.impedance.is_some() && case.impedance != Some(0);
        assert_eq!(
            impedance_derived,
            expected.body_fat_pct.is_some(),
            "impedance-derived presence for {case:?}"
        );

        if let Some(expected) = expected.body_fat_pct {
            assert!(
                close(metrics.fat_percentage(), expected),
                "body_fat mismatch for {case:?}"
            );
        }
        if let Some(expected) = expected.water_pct {
            assert!(
                close(metrics.water_percentage(), expected),
                "water mismatch for {case:?}"
            );
        }
        if let Some(expected) = expected.bone_mass_kg {
            assert!(
                close(metrics.bone_mass(), expected),
                "bone_mass mismatch for {case:?}"
            );
        }
        if let Some(expected) = expected.muscle_mass_kg {
            assert!(
                close(metrics.muscle_mass(), expected),
                "muscle_mass mismatch for {case:?}"
            );
        }
        if let Some(expected) = expected.protein_pct {
            assert!(
                close(metrics.protein_percentage(), expected),
                "protein mismatch for {case:?}"
            );
        }
        if let Some(expected) = expected.lean_body_mass_kg {
            assert!(
                close(metrics.lbm_coefficient(), expected),
                "lbm mismatch for {case:?}"
            );
        }
        if let Some(expected) = expected.metabolic_age {
            let rounded = grammatic::metrics::round_metabolic_age(metrics.metabolic_age());
            assert!(
                close(f64::from(rounded), expected),
                "metabolic_age mismatch for {case:?}: {rounded} != {expected}"
            );
        }
        if let Some(expected) = &expected.body_type {
            assert_eq!(
                metrics.body_type(),
                expected,
                "body_type mismatch for {case:?}"
            );
        }
    }
}

#[test]
fn age_years_uses_365_25_day_year() {
    // (today - born).days / 365.25
    let dob = NaiveDate::from_ymd_opt(1996, 1, 1).unwrap();
    let today = NaiveDate::from_ymd_opt(2026, 9, 3).unwrap();
    let days = (today - dob).num_days() as f64;
    assert!(close(age_years(dob, today), days / 365.25));
}

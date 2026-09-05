//! Body-composition estimates derived from weight, impedance, sex, height and age.
//!
//! Mi Fit / Holtek formulas, following the reverse-engineering in
//! lolouk44/xiaomi_mi_scale.
//! These are estimates, not medical data.

use chrono::NaiveDate;

use crate::parser::round2;

const BODY_TYPE_LABELS: [&str; 9] = [
    "Obese",
    "Overweight",
    "Thick-set",
    "Lack-exercise",
    "Balanced",
    "Balanced-muscular",
    "Skinny",
    "Balanced-skinny",
    "Skinny-muscular",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sex {
    Male,
    Female,
}

impl Sex {
    pub fn parse(s: &str) -> Option<Sex> {
        match s {
            "male" => Some(Sex::Male),
            "female" => Some(Sex::Female),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Sex::Male => "male",
            Sex::Female => "female",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MeasurementMetrics {
    // Always computed (weight-only).
    pub bmi: f64,
    pub bmr_kcal: f64,
    pub visceral_fat: f64,
    pub ideal_weight_kg: f64,
    // Computed only when impedance is present.
    pub body_fat_pct: Option<f64>,
    pub water_pct: Option<f64>,
    pub bone_mass_kg: Option<f64>,
    pub muscle_mass_kg: Option<f64>,
    pub protein_pct: Option<f64>,
    pub lean_body_mass_kg: Option<f64>,
    pub metabolic_age: Option<i32>,
    pub body_type: Option<String>,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum MetricsError {
    #[error("unsupported sex: {0}")]
    UnsupportedSex(String),
    #[error("profile height_cm missing or out of range (30-220)")]
    InvalidHeight,
    #[error("computed age out of range: {age:.1}")]
    InvalidAge { age: f64 },
    #[error("height over 220 cm (scale is sleeping or profile is wrong)")]
    HeightTooTall,
    #[error("weight below 10 kg or above 200 kg")]
    WeightOutOfRange,
    #[error("age above 99 years")]
    AgeTooHigh,
    #[error("impedance above 3000 ohm")]
    ImpedanceTooHigh,
}

fn clamp(value: f64, minimum: f64, maximum: f64) -> f64 {
    value.max(minimum).min(maximum)
}

/// Round half to even.
// Branches intentionally mirror round-half-to-even semantics; the diff < 0.5 and
// exact-tie arms share a result value on purpose.
#[allow(clippy::if_same_then_else)]
pub fn round_metabolic_age(value: f64) -> i32 {
    let floor = value.floor();
    let diff = value - floor;
    let rounded = if diff > 0.5 {
        floor + 1.0
    } else if diff < 0.5 {
        floor
    } else if (floor as i64) % 2 == 0 {
        floor
    } else {
        floor + 1.0
    };
    rounded as i32
}

/// Age in fractional years from a date of birth.
pub fn age_years(dob: NaiveDate, today: NaiveDate) -> f64 {
    (today - dob).num_days() as f64 / 365.25
}

fn fat_percentage_scale(age: f64, sex: Sex) -> [f64; 4] {
    // Brackets are [min, max); the last bracket is the fallback.
    const FEMALE: [[f64; 4]; 7] = [
        [12.0, 21.0, 30.0, 34.0],
        [15.0, 24.0, 33.0, 37.0],
        [18.0, 27.0, 36.0, 40.0],
        [20.0, 28.0, 37.0, 41.0],
        [21.0, 28.0, 35.0, 40.0],
        [22.0, 29.0, 36.0, 41.0],
        [23.0, 30.0, 37.0, 42.0],
    ];
    const MALE: [[f64; 4]; 7] = [
        [7.0, 16.0, 25.0, 30.0],
        [7.0, 16.0, 25.0, 30.0],
        [7.0, 16.0, 25.0, 30.0],
        [7.0, 16.0, 25.0, 30.0],
        [11.0, 17.0, 22.0, 27.0],
        [12.0, 18.0, 23.0, 28.0],
        [14.0, 20.0, 25.0, 30.0],
    ];
    const MAX_AGE: f64 = 100.0;
    let brackets = [12.0, 14.0, 16.0, 18.0, 40.0, 60.0, MAX_AGE];
    let table = match sex {
        Sex::Female => FEMALE,
        Sex::Male => MALE,
    };
    for (i, bound) in brackets.iter().enumerate() {
        if age < *bound {
            return table[i];
        }
    }
    table[table.len() - 1]
}

fn muscle_mass_scale(height: f64, sex: Sex) -> [f64; 2] {
    let (thresholds, table): ([f64; 3], [[f64; 2]; 3]) = match sex {
        Sex::Male => (
            [170.0, 160.0, 0.0],
            [[49.4, 59.5], [44.0, 52.5], [38.5, 46.6]],
        ),
        Sex::Female => (
            [160.0, 150.0, 0.0],
            [[36.5, 42.6], [32.9, 37.6], [29.1, 34.8]],
        ),
    };
    for (i, threshold) in thresholds.iter().enumerate() {
        if height >= *threshold {
            return table[i];
        }
    }
    table[table.len() - 1]
}

/// The Mi Fit / Holtek body metrics model.
#[derive(Debug, Clone, Copy)]
pub struct BodyMetrics {
    weight: f64,
    height: f64,
    age: f64,
    sex: Sex,
    impedance: i32,
}

impl BodyMetrics {
    pub fn new(
        weight: f64,
        height: f64,
        age: f64,
        sex: Sex,
        impedance: i32,
    ) -> Result<BodyMetrics, MetricsError> {
        if height > 220.0 {
            return Err(MetricsError::HeightTooTall);
        }
        if !(10.0..=200.0).contains(&weight) {
            return Err(MetricsError::WeightOutOfRange);
        }
        if age > 99.0 {
            return Err(MetricsError::AgeTooHigh);
        }
        if impedance > 3000 {
            return Err(MetricsError::ImpedanceTooHigh);
        }
        Ok(BodyMetrics {
            weight,
            height,
            age,
            sex,
            impedance,
        })
    }

    pub fn lbm_coefficient(&self) -> f64 {
        let mut lbm = (self.height * 9.058 / 100.0) * (self.height / 100.0);
        lbm += self.weight * 0.32 + 12.226;
        lbm -= self.impedance as f64 * 0.0068;
        lbm -= self.age * 0.0542;
        lbm
    }

    pub fn bmi(&self) -> f64 {
        clamp(
            self.weight / ((self.height / 100.0) * (self.height / 100.0)),
            10.0,
            90.0,
        )
    }

    pub fn bmr(&self) -> f64 {
        let bmr = match self.sex {
            Sex::Female => {
                let bmr = 864.6 + self.weight * 10.2036 - self.height * 0.39336 - self.age * 6.204;
                if bmr > 2996.0 { 5000.0 } else { bmr }
            }
            Sex::Male => {
                let bmr = 877.8 + self.weight * 14.916 - self.height * 0.726 - self.age * 8.976;
                if bmr > 2322.0 { 5000.0 } else { bmr }
            }
        };
        clamp(bmr, 500.0, 10000.0)
    }

    pub fn fat_percentage(&self) -> f64 {
        let constant = match self.sex {
            Sex::Female if self.age <= 49.0 => 9.25,
            Sex::Female => 7.25,
            Sex::Male => 0.8,
        };

        let lbm = self.lbm_coefficient();
        let coefficient = if self.sex == Sex::Male && self.weight < 61.0 {
            0.98
        } else if self.sex == Sex::Female && self.weight > 60.0 {
            if self.height > 160.0 {
                0.96 * 1.03
            } else {
                0.96
            }
        } else if self.sex == Sex::Female && self.weight < 50.0 {
            if self.height > 160.0 {
                1.02 * 1.03
            } else {
                1.02
            }
        } else {
            1.0
        };

        let mut fat_percentage = (1.0 - (((lbm - constant) * coefficient) / self.weight)) * 100.0;
        if fat_percentage > 63.0 {
            fat_percentage = 75.0;
        }
        clamp(fat_percentage, 5.0, 75.0)
    }

    pub fn water_percentage(&self) -> f64 {
        let mut water_percentage = (100.0 - self.fat_percentage()) * 0.7;
        let coefficient = if water_percentage <= 50.0 { 1.02 } else { 0.98 };
        if water_percentage * coefficient >= 65.0 {
            water_percentage = 75.0;
        }
        clamp(water_percentage * coefficient, 35.0, 75.0)
    }

    pub fn bone_mass(&self) -> f64 {
        let base = match self.sex {
            Sex::Female => 0.245691014,
            Sex::Male => 0.18016894,
        };
        let mut bone_mass = -(base - (self.lbm_coefficient() * 0.05158));
        bone_mass = if bone_mass > 2.2 {
            bone_mass + 0.1
        } else {
            bone_mass - 0.1
        };
        // Thresholds (5.1 vs 5.2, 84 vs 93.5).
        #[allow(clippy::if_same_then_else)]
        if self.sex == Sex::Female && bone_mass > 5.1 {
            bone_mass = 8.0;
        } else if self.sex == Sex::Male && bone_mass > 5.2 {
            bone_mass = 8.0;
        }
        clamp(bone_mass, 0.5, 8.0)
    }

    pub fn muscle_mass(&self) -> f64 {
        let mut muscle_mass =
            self.weight - ((self.fat_percentage() * 0.01) * self.weight) - self.bone_mass();
        #[allow(clippy::if_same_then_else)]
        if self.sex == Sex::Female && muscle_mass >= 84.0 {
            muscle_mass = 120.0;
        } else if self.sex == Sex::Male && muscle_mass >= 93.5 {
            muscle_mass = 120.0;
        }
        clamp(muscle_mass, 10.0, 120.0)
    }

    pub fn visceral_fat(&self) -> f64 {
        let vfal = match self.sex {
            Sex::Female => {
                if self.weight > -(13.0 - (self.height * 0.5)) {
                    let subsubcalc =
                        ((self.height * 1.45) + (self.height * 0.1158) * self.height) - 120.0;
                    let subcalc = self.weight * 500.0 / subsubcalc;
                    (subcalc - 6.0) + (self.age * 0.07)
                } else {
                    let subcalc = 0.691 + (self.height * -0.0024) + (self.height * -0.0024);
                    -((self.height * 0.027) - (subcalc * self.weight)) + (self.age * 0.07)
                        - self.age
                }
            }
            Sex::Male => {
                if self.height < self.weight * 1.6 {
                    let subcalc = -((self.height * 0.4) - (self.height * (self.height * 0.0826)));
                    ((self.weight * 305.0) / (subcalc + 48.0)) - 2.9 + (self.age * 0.15)
                } else {
                    let subcalc = 0.765 + self.height * -0.0015;
                    -((self.height * 0.143) - (self.weight * subcalc)) + (self.age * 0.15) - 5.0
                }
            }
        };
        clamp(vfal, 1.0, 50.0)
    }

    pub fn ideal_weight(&self) -> f64 {
        match self.sex {
            Sex::Female => (self.height - 70.0) * 0.6,
            Sex::Male => (self.height - 80.0) * 0.7,
        }
    }

    pub fn protein_percentage(&self) -> f64 {
        let protein_percentage = (self.muscle_mass() / self.weight) * 100.0;
        let protein_percentage = protein_percentage - self.water_percentage();
        clamp(protein_percentage, 5.0, 32.0)
    }

    pub fn body_type(&self) -> &'static str {
        let fat_scale = fat_percentage_scale(self.age, self.sex);
        let factor = if self.fat_percentage() > fat_scale[2] {
            0
        } else if self.fat_percentage() < fat_scale[1] {
            2
        } else {
            1
        };

        let muscle_scale = muscle_mass_scale(self.height, self.sex);
        let index = if self.muscle_mass() > muscle_scale[1] {
            2 + (factor * 3)
        } else if self.muscle_mass() < muscle_scale[0] {
            factor * 3
        } else {
            1 + (factor * 3)
        };
        BODY_TYPE_LABELS[index]
    }

    pub fn metabolic_age(&self) -> f64 {
        let metabolic_age = match self.sex {
            Sex::Female => {
                (self.height * -1.1165)
                    + (self.weight * 1.5784)
                    + (self.age * 0.4615)
                    + (self.impedance as f64 * 0.0415)
                    + 83.2548
            }
            Sex::Male => {
                (self.height * -0.7471)
                    + (self.weight * 0.9161)
                    + (self.age * 0.4184)
                    + (self.impedance as f64 * 0.0517)
                    + 54.2267
            }
        };
        clamp(metabolic_age, 15.0, 80.0)
    }
}

/// Compute the metric set stored alongside a measurement: values rounded to
/// 2 decimals (half to even), metabolic age to an integer.
pub fn compute_body_metrics(
    weight_kg: f64,
    impedance_ohm: Option<u16>,
    sex: Sex,
    height_cm: f64,
    age: f64,
) -> Result<MeasurementMetrics, MetricsError> {
    if !(30.0..=220.0).contains(&height_cm) {
        return Err(MetricsError::InvalidHeight);
    }
    if !(1.0..=99.0).contains(&age) {
        return Err(MetricsError::InvalidAge { age });
    }
    // Absent and zero impedance are equivalent (weight-only metrics).
    let effective_impedance = impedance_ohm.filter(|value| *value != 0);
    let metrics = BodyMetrics::new(
        weight_kg,
        height_cm,
        age,
        sex,
        effective_impedance.map(i32::from).unwrap_or(0),
    )?;

    let mut result = MeasurementMetrics {
        bmi: round2(metrics.bmi()),
        bmr_kcal: round2(metrics.bmr()),
        visceral_fat: round2(metrics.visceral_fat()),
        ideal_weight_kg: round2(metrics.ideal_weight()),
        body_fat_pct: None,
        water_pct: None,
        bone_mass_kg: None,
        muscle_mass_kg: None,
        protein_pct: None,
        lean_body_mass_kg: None,
        metabolic_age: None,
        body_type: None,
    };
    if effective_impedance.is_some() {
        result.body_fat_pct = Some(round2(metrics.fat_percentage()));
        result.water_pct = Some(round2(metrics.water_percentage()));
        result.bone_mass_kg = Some(round2(metrics.bone_mass()));
        result.muscle_mass_kg = Some(round2(metrics.muscle_mass()));
        result.protein_pct = Some(round2(metrics.protein_percentage()));
        result.lean_body_mass_kg = Some(round2(metrics.lbm_coefficient()));
        result.metabolic_age = Some(round_metabolic_age(metrics.metabolic_age()));
        result.body_type = Some(metrics.body_type().to_string());
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_with_impedance() {
        let metrics = compute_body_metrics(75.0, Some(500), Sex::Male, 175.0, 30.0).unwrap();
        assert_eq!(metrics.bmi, 24.49);
        assert!((5.0..=75.0).contains(&metrics.body_fat_pct.unwrap()));
        assert!((35.0..=75.0).contains(&metrics.water_pct.unwrap()));
        assert!((10.0..=120.0).contains(&metrics.muscle_mass_kg.unwrap()));
        assert!(BODY_TYPE_LABELS.contains(&metrics.body_type.as_deref().unwrap()));
        assert!((15..=80).contains(&metrics.metabolic_age.unwrap()));
    }

    #[test]
    fn metrics_weight_only() {
        let metrics = compute_body_metrics(75.0, None, Sex::Male, 175.0, 30.0).unwrap();
        assert_eq!(metrics.bmi, 24.49);
        assert_eq!(metrics.body_fat_pct, None);
        assert_eq!(metrics.metabolic_age, None);
    }

    #[test]
    fn metrics_zero_impedance_means_weight_only() {
        let metrics = compute_body_metrics(75.0, Some(0), Sex::Male, 175.0, 30.0).unwrap();
        assert_eq!(metrics.body_fat_pct, None);
    }

    #[test]
    fn metrics_invalid_profile() {
        let result = compute_body_metrics(75.0, Some(500), Sex::Male, 500.0, 30.0);
        assert!(matches!(result, Err(MetricsError::InvalidHeight)));
    }

    #[test]
    fn round_half_even() {
        // round-half-to-even: round(2.5) == 2, round(3.5) == 4, round(-2.5) == -2.
        assert_eq!(round_metabolic_age(2.5), 2);
        assert_eq!(round_metabolic_age(3.5), 4);
        assert_eq!(round_metabolic_age(0.5), 0);
        assert_eq!(round_metabolic_age(1.5), 2);
        assert_eq!(round_metabolic_age(30.5), 30);
    }
}

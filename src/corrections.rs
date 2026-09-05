//! Profile and Measurement corrections: validation, mutation, and recompute
//! commit together. HTTP callers and database tests use the same interface.
use crate::{profile::MetricsPolicy, store};
use chrono::{DateTime, NaiveDate, Utc};
use serde::Deserialize;
use sqlx::PgPool;

#[derive(Debug, thiserror::Error)]
pub enum CorrectionError {
    #[error("{0}")]
    InvalidInput(&'static str),
    #[error("Record no longer exists.")]
    NotFound,
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error("Metric computation failed. No changes were saved.")]
    Recompute(#[source] anyhow::Error),
}
pub type Result<T> = std::result::Result<T, CorrectionError>;
fn bad(message: &'static str) -> CorrectionError {
    CorrectionError::InvalidInput(message)
}
fn found(count: u64) -> Result<()> {
    if count == 0 {
        Err(CorrectionError::NotFound)
    } else {
        Ok(())
    }
}

#[derive(Deserialize)]
pub struct ProfileInput {
    pub name: String,
    pub sex: String,
    pub height_cm: f64,
    pub dob: NaiveDate,
    pub weight_min: Option<f64>,
    pub weight_max: Option<f64>,
}
impl ProfileInput {
    fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() || self.name.len() > 100 {
            return Err(bad("Name must contain 1–100 characters."));
        }
        if !["male", "female"].contains(&self.sex.as_str())
            || !(30.0..=220.0).contains(&self.height_cm)
        {
            return Err(bad("Choose a sex and height between 30 and 220 cm."));
        }
        if self.dob > Utc::now().date_naive()
            || self.dob < NaiveDate::from_ymd_opt(1900, 1, 1).unwrap()
        {
            return Err(bad("Enter a valid date of birth."));
        }
        if self
            .weight_min
            .into_iter()
            .chain(self.weight_max)
            .any(|v| !(0.0..=300.0).contains(&v))
            || matches!((self.weight_min,self.weight_max),(Some(a),Some(b)) if a >= b)
        {
            return Err(bad(
                "Weight bounds must be 0–300 kg, with minimum below maximum.",
            ));
        }
        Ok(())
    }
}

#[derive(Deserialize)]
pub struct MeasurementInput {
    pub measured_at: DateTime<Utc>,
    pub weight_kg: f64,
    pub impedance_ohm: Option<i32>,
    pub profile_id: Option<i64>,
}
impl MeasurementInput {
    fn validate(&self) -> Result<()> {
        if !(1.0..=300.0).contains(&self.weight_kg) {
            return Err(bad("Weight must be between 1 and 300 kg."));
        }
        if self.impedance_ohm.is_some_and(|v| !(1..=3000).contains(&v)) {
            return Err(bad("Impedance must be between 1 and 3000 ohms."));
        }
        if self.measured_at > Utc::now() + chrono::Duration::minutes(5) {
            return Err(bad("Measurement time cannot be in the future."));
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct Corrections {
    pool: PgPool,
    policy: MetricsPolicy,
}
impl Corrections {
    pub fn new(pool: PgPool, policy: MetricsPolicy) -> Self {
        Self { pool, policy }
    }
    async fn recompute(&self, connection: &mut sqlx::PgConnection, id: Option<i64>) -> Result<()> {
        store::recompute_metrics_on(connection, id, &self.policy)
            .await
            .map_err(CorrectionError::Recompute)?;
        Ok(())
    }
    pub async fn create_profile(&self, p: ProfileInput) -> Result<i64> {
        p.validate()?;
        let (id,): (i64,) = sqlx::query_as("INSERT INTO profiles(name,sex,height_cm,dob,weight_min,weight_max) VALUES($1,$2,$3,$4,$5,$6) RETURNING id")
        .bind(p.name.trim()).bind(p.sex).bind(p.height_cm).bind(p.dob).bind(p.weight_min).bind(p.weight_max).fetch_one(&self.pool).await?;
        Ok(id)
    }
    pub async fn update_profile(&self, id: i64, p: ProfileInput) -> Result<i64> {
        let mut tx = self.pool.begin().await?;
        p.validate()?;
        let result = sqlx::query("UPDATE profiles SET name=$2,sex=$3,height_cm=$4,dob=$5,weight_min=$6,weight_max=$7 WHERE id=$1")
        .bind(id).bind(p.name.trim()).bind(p.sex).bind(p.height_cm).bind(p.dob).bind(p.weight_min).bind(p.weight_max).execute(&mut *tx).await?;
        found(result.rows_affected())?;
        self.recompute(&mut tx, None).await?;
        tx.commit().await?;
        Ok(id)
    }
    pub async fn delete_profile(&self, id: i64) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        found(
            sqlx::query("DELETE FROM profiles WHERE id=$1")
                .bind(id)
                .execute(&mut *tx)
                .await?
                .rows_affected(),
        )?;
        self.recompute(&mut tx, None).await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn create_measurement(&self, m: MeasurementInput) -> Result<i64> {
        let mut tx = self.pool.begin().await?;
        m.validate()?;
        let (id,): (i64,) = sqlx::query_as("INSERT INTO measurements(measured_at,received_at,clock_source,weight_kg,impedance_ohm,profile_id,unit) VALUES($1,now(),'receiver',$2,$3,$4,'kg') RETURNING id")
        .bind(m.measured_at).bind(m.weight_kg).bind(m.impedance_ohm).bind(m.profile_id).fetch_one(&mut *tx).await?;
        self.recompute(&mut tx, Some(id)).await?;
        tx.commit().await?;
        Ok(id)
    }
    pub async fn update_measurement(&self, id: i64, m: MeasurementInput) -> Result<i64> {
        let mut tx = self.pool.begin().await?;
        m.validate()?;
        found(sqlx::query("UPDATE measurements SET measured_at=$2,weight_kg=$3,impedance_ohm=$4,profile_id=$5 WHERE id=$1")
        .bind(id).bind(m.measured_at).bind(m.weight_kg).bind(m.impedance_ohm).bind(m.profile_id).execute(&mut *tx).await?.rows_affected())?;
        self.recompute(&mut tx, Some(id)).await?;
        tx.commit().await?;
        Ok(id)
    }
    pub async fn delete_measurement(&self, id: i64) -> Result<()> {
        found(
            sqlx::query("DELETE FROM measurements WHERE id=$1")
                .bind(id)
                .execute(&self.pool)
                .await?
                .rows_affected(),
        )?;
        Ok(())
    }
}

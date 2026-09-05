//! Postgres persistence — the production `MeasurementSink` adapter.
//!
//! Capture persistence owns sibling serialization, enrichment, and dedup.
//! Callers observe one durable outcome, without coordinating SQL operations.

use anyhow::Context;
use chrono::{DateTime, FixedOffset, NaiveDate};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

use crate::capture::{MeasurementRecord, MeasurementSink, PersistOutcome};
use crate::metrics::{MeasurementMetrics, Sex};
use crate::profile::{HistoryPoint, MetricsPolicy, Profile};

pub async fn connect(url: &str) -> anyhow::Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .connect(url)
        .await
        .context("connecting to Postgres (is the server up? is database.url right?)")?;
    sqlx::migrate!()
        .run(&pool)
        .await
        .context("running schema migrations")?;
    Ok(pool)
}

/// The 12 metric columns in schema order — the single home for the column
/// list and bind order both `INSERT` and `UPDATE` follow. `None` maps to 12
/// NULLs, so trimmed storage policies need no schema change.
#[derive(Debug, Clone, Default)]
struct MetricNullables {
    bmi: Option<f64>,
    bmr_kcal: Option<f64>,
    visceral_fat: Option<f64>,
    ideal_weight_kg: Option<f64>,
    body_fat_pct: Option<f64>,
    water_pct: Option<f64>,
    bone_mass_kg: Option<f64>,
    muscle_mass_kg: Option<f64>,
    protein_pct: Option<f64>,
    lean_body_mass_kg: Option<f64>,
    metabolic_age: Option<i32>,
    body_type: Option<String>,
}

impl From<Option<&MeasurementMetrics>> for MetricNullables {
    fn from(metrics: Option<&MeasurementMetrics>) -> Self {
        let Some(m) = metrics else {
            return MetricNullables::default();
        };
        MetricNullables {
            bmi: Some(m.bmi),
            bmr_kcal: Some(m.bmr_kcal),
            visceral_fat: Some(m.visceral_fat),
            ideal_weight_kg: Some(m.ideal_weight_kg),
            body_fat_pct: m.body_fat_pct,
            water_pct: m.water_pct,
            bone_mass_kg: m.bone_mass_kg,
            muscle_mass_kg: m.muscle_mass_kg,
            protein_pct: m.protein_pct,
            lean_body_mass_kg: m.lean_body_mass_kg,
            metabolic_age: m.metabolic_age,
            body_type: m.body_type.clone(),
        }
    }
}

type PgQuery<'q> = sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>;

fn bind_metric_columns<'q>(query: PgQuery<'q>, metrics: &MetricNullables) -> PgQuery<'q> {
    query
        .bind(metrics.bmi)
        .bind(metrics.bmr_kcal)
        .bind(metrics.visceral_fat)
        .bind(metrics.ideal_weight_kg)
        .bind(metrics.body_fat_pct)
        .bind(metrics.water_pct)
        .bind(metrics.bone_mass_kg)
        .bind(metrics.muscle_mass_kg)
        .bind(metrics.protein_pct)
        .bind(metrics.lean_body_mass_kg)
        .bind(metrics.metabolic_age)
        .bind(metrics.body_type.clone())
}

/// Insert a measurement; returns false when the dedup constraint suppressed
/// it (re-broadcast of the same final frame).
async fn insert_measurement(
    pool: &mut sqlx::PgConnection,
    record: &MeasurementRecord,
) -> anyhow::Result<bool> {
    let metrics = MetricNullables::from(record.metrics.as_ref());
    let query = sqlx::query(
        r#"
        INSERT INTO measurements (
            measured_at, clock_source, received_at, weight_kg, impedance_ohm,
            profile_id, unit, raw_frame, rssi,
            bmi, bmr_kcal, visceral_fat, ideal_weight_kg,
            body_fat_pct, water_pct, bone_mass_kg, muscle_mass_kg, protein_pct,
            lean_body_mass_kg, metabolic_age, body_type
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14,
                  $15, $16, $17, $18, $19, $20, $21)
        ON CONFLICT (measured_at, weight_kg, COALESCE(impedance_ohm, -1)) DO NOTHING
        "#,
    )
    .bind(record.measured_at)
    .bind(&record.clock_source)
    .bind(record.received_at)
    .bind(record.weight_kg)
    .bind(record.impedance_ohm)
    .bind(record.profile_id)
    .bind(record.unit.as_str())
    .bind(&record.raw_frame)
    .bind(record.rssi);
    let result = bind_metric_columns(query, &metrics).execute(pool).await?;
    Ok(result.rows_affected() > 0)
}

#[derive(Debug, sqlx::FromRow)]
struct ProfileRow {
    id: i64,
    name: String,
    sex: String,
    height_cm: f64,
    dob: NaiveDate,
    weight_min: Option<f64>,
    weight_max: Option<f64>,
}

impl From<ProfileRow> for Profile {
    fn from(row: ProfileRow) -> Profile {
        Profile {
            id: row.id,
            name: row.name,
            sex: row.sex,
            height_cm: row.height_cm,
            dob: row.dob,
            weight_min: row.weight_min,
            weight_max: row.weight_max,
        }
    }
}

pub async fn list_profiles<'e>(
    pool: impl sqlx::Executor<'e, Database = sqlx::Postgres>,
) -> anyhow::Result<Vec<Profile>> {
    let rows: Vec<ProfileRow> = sqlx::query_as(
        r#"
        SELECT id, name, sex, height_cm, dob, weight_min, weight_max
        FROM profiles ORDER BY id
        "#,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

/// Upgrade a weight-only sibling row with a later impedance-bearing
/// record. Returns the upgraded row id (plus the superseded frame hex for
/// the log), or `None` when there is no weight-only sibling. The dedup
/// constraint guarantees at most one weight-only row per
/// `(measured_at, weight_kg)`, so the target is unique. First observation
/// wins for `received_at`/`rssi`; everything else becomes the new record's.
async fn enrich_with_impedance(
    pool: &mut sqlx::PgConnection,
    record: &MeasurementRecord,
) -> anyhow::Result<Option<crate::capture::Enrichment>> {
    let Some(impedance) = record.impedance_ohm else {
        return Ok(None);
    };
    let metrics = MetricNullables::from(record.metrics.as_ref());
    // The caller holds the transaction's sibling lock through commit.
    let sibling: Option<(i64, String)> = sqlx::query_as(
        r#"
        SELECT id, raw_frame FROM measurements
        WHERE measured_at = $1 AND weight_kg = $2 AND impedance_ohm IS NULL
        "#,
    )
    .bind(record.measured_at)
    .bind(record.weight_kg)
    .fetch_optional(&mut *pool)
    .await?;
    let Some((id, superseded_raw_frame)) = sibling else {
        return Ok(None);
    };
    let query = sqlx::query(
        r#"
        UPDATE measurements SET
            impedance_ohm = $2, profile_id = $3, unit = $4, raw_frame = $5,
            clock_source = $6,
            bmi = $7, bmr_kcal = $8, visceral_fat = $9, ideal_weight_kg = $10,
            body_fat_pct = $11, water_pct = $12, bone_mass_kg = $13,
            muscle_mass_kg = $14, protein_pct = $15, lean_body_mass_kg = $16,
            metabolic_age = $17, body_type = $18
        WHERE id = $1 AND impedance_ohm IS NULL
        "#,
    )
    .bind(id)
    .bind(impedance)
    .bind(record.profile_id)
    .bind(record.unit.as_str())
    .bind(&record.raw_frame)
    .bind(&record.clock_source);
    let updated = bind_metric_columns(query, &metrics)
        .execute(pool)
        .await?
        .rows_affected()
        > 0;
    Ok(updated.then_some(crate::capture::Enrichment {
        id,
        superseded_raw_frame,
    }))
}

/// The complete capture persistence operation. Serialize the timestamp/weight
/// group even when no row exists yet; row locks alone cannot protect absence.
/// Hash collisions only serialize unrelated groups. Different timestamps with
/// the same UTC instant share a key. The lock is released on commit or rollback.
pub async fn persist_measurement(
    pool: &PgPool,
    record: &MeasurementRecord,
) -> anyhow::Result<PersistOutcome> {
    let mut tx = pool.begin().await?;
    let weight = if record.weight_kg == 0.0 {
        0.0
    } else {
        record.weight_kg
    };
    let key = format!(
        "grammatic:{}:{}",
        record.measured_at.timestamp_micros(),
        weight
    );
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(key)
        .execute(&mut *tx)
        .await?;
    let duplicate: (bool,) = sqlx::query_as(
        "SELECT EXISTS(SELECT 1 FROM measurements WHERE measured_at=$1 AND weight_kg=$2
         AND ($3::integer IS NULL OR impedance_ohm=$3))",
    )
    .bind(record.measured_at)
    .bind(record.weight_kg)
    .bind(record.impedance_ohm)
    .fetch_one(&mut *tx)
    .await?;
    let outcome = if duplicate.0 {
        PersistOutcome::Duplicate
    } else if let Some(enriched) = enrich_with_impedance(&mut tx, record).await? {
        PersistOutcome::Enriched(enriched)
    } else if insert_measurement(&mut tx, record).await? {
        PersistOutcome::Inserted
    } else {
        PersistOutcome::Duplicate
    };
    tx.commit().await?;
    Ok(outcome)
}

impl MeasurementSink for PgPool {
    async fn list_profiles(&self) -> anyhow::Result<Vec<Profile>> {
        list_profiles(self).await
    }

    async fn persist_measurement(
        &self,
        record: &MeasurementRecord,
    ) -> anyhow::Result<PersistOutcome> {
        persist_measurement(self, record).await
    }

    async fn recent_history(
        &self,
        profile_id: i64,
        before: DateTime<FixedOffset>,
        limit: u32,
    ) -> anyhow::Result<Vec<HistoryPoint>> {
        recent_history(self, profile_id, before, limit).await
    }
}

#[derive(Debug, sqlx::FromRow)]
struct HistoryRow {
    weight_kg: f64,
    impedance_ohm: Option<i32>,
    measured_at: DateTime<FixedOffset>,
}

/// Past measurements of one profile strictly before `before`, newest first,
/// capped at `limit` — the tie-break view. The strict `<` keeps replay
/// deterministic: the current frame and its re-broadcasts share one dedup-key
/// time, so neither ever observes itself.
pub async fn recent_history(
    pool: &PgPool,
    profile_id: i64,
    before: DateTime<FixedOffset>,
    limit: u32,
) -> anyhow::Result<Vec<HistoryPoint>> {
    let rows: Vec<HistoryRow> = sqlx::query_as(
        r#"
        SELECT weight_kg, impedance_ohm, measured_at
        FROM measurements
        WHERE profile_id = $1 AND measured_at < $2
        ORDER BY measured_at DESC, id DESC
        LIMIT $3
        "#,
    )
    .bind(profile_id)
    .bind(before)
    .bind(i64::from(limit))
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| HistoryPoint {
            weight_kg: row.weight_kg,
            impedance_ohm: row.impedance_ohm,
            measured_at: row.measured_at,
        })
        .collect())
}

pub async fn add_profile(
    pool: &PgPool,
    name: &str,
    sex: Sex,
    height_cm: f64,
    dob: NaiveDate,
    weight_min: Option<f64>,
    weight_max: Option<f64>,
) -> anyhow::Result<i64> {
    let row: (i64,) = sqlx::query_as(
        r#"
        INSERT INTO profiles (name, sex, height_cm, dob, weight_min, weight_max)
        VALUES ($1, $2, $3, $4, $5, $6)
        RETURNING id
        "#,
    )
    .bind(name)
    .bind(sex.as_str())
    .bind(height_cm)
    .bind(dob)
    .bind(weight_min)
    .bind(weight_max)
    .fetch_one(pool)
    .await?;
    Ok(row.0)
}

pub async fn remove_profile(pool: &PgPool, name: &str) -> anyhow::Result<bool> {
    let result = sqlx::query("DELETE FROM profiles WHERE name = $1")
        .bind(name)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// One stored measurement's assignment inputs, plus its row id.
type RecomputeRow = (i64, DateTime<FixedOffset>, f64, Option<i32>, Option<i64>);

/// Recompute metric columns for existing rows from the current profiles.
/// Never re-assigns: the stored `profile_id` is pinned, only the columns are
/// re-derived. Rows without an assignable profile get their metric columns
/// cleared. Applies `policy` so recompute honors the storage setting.
pub async fn recompute_metrics(
    pool: &PgPool,
    only_id: Option<i64>,
    policy: &MetricsPolicy,
) -> anyhow::Result<usize> {
    let mut connection = pool.acquire().await?;
    recompute_metrics_on(&mut connection, only_id, policy).await
}

/// Transaction-aware recomputation for dashboard edits.
pub(crate) async fn recompute_metrics_on(
    connection: &mut sqlx::PgConnection,
    only_id: Option<i64>,
    policy: &MetricsPolicy,
) -> anyhow::Result<usize> {
    let profiles = list_profiles(&mut *connection).await?;
    let rows: Vec<RecomputeRow> = match only_id {
        Some(id) => {
            sqlx::query_as(
                "SELECT id, measured_at, weight_kg, impedance_ohm, profile_id
                 FROM measurements WHERE id = $1",
            )
            .bind(id)
            .fetch_all(&mut *connection)
            .await?
        }
        None => {
            sqlx::query_as(
                "SELECT id, measured_at, weight_kg, impedance_ohm, profile_id
                     FROM measurements",
            )
            .fetch_all(&mut *connection)
            .await?
        }
    };

    let mut updated = 0;
    for (id, measured_at, weight_kg, impedance_ohm, profile_id) in rows {
        let profile = profile_id.and_then(|pid| profiles.iter().find(|p| p.id == pid));
        // The measurement date ages the profile (ADR-0001), so recompute is
        // idempotent. An invalid sex or metrics error skips that row's
        // metrics with a warning instead of aborting the whole recompute.
        let metrics = profile.and_then(|profile| {
            crate::profile::metrics_for(
                profile,
                weight_kg,
                impedance_ohm.map(|v| v as u16),
                measured_at.date_naive(),
            )
        });
        update_metric_columns(&mut *connection, id, policy.apply(metrics).as_ref()).await?;
        updated += 1;
    }
    Ok(updated)
}

async fn update_metric_columns<'e>(
    pool: impl sqlx::Executor<'e, Database = sqlx::Postgres>,
    id: i64,
    metrics: Option<&MeasurementMetrics>,
) -> anyhow::Result<()> {
    let query = sqlx::query(
        r#"
        UPDATE measurements SET
            bmi = $2, bmr_kcal = $3, visceral_fat = $4, ideal_weight_kg = $5,
            body_fat_pct = $6, water_pct = $7, bone_mass_kg = $8, muscle_mass_kg = $9,
            protein_pct = $10, lean_body_mass_kg = $11, metabolic_age = $12, body_type = $13
        WHERE id = $1
        "#,
    )
    .bind(id);
    bind_metric_columns(query, &MetricNullables::from(metrics))
        .execute(pool)
        .await?;
    Ok(())
}

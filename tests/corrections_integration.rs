//! Test Correction workflows through their public interface and real Postgres.
use chrono::{NaiveDate, Utc};
use grammatic::corrections::{CorrectionError, Corrections, MeasurementInput, ProfileInput};
use grammatic::profile::MetricsPolicy;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

fn profile(height: f64) -> ProfileInput {
    ProfileInput {
        name: "Correction test".into(),
        sex: "male".into(),
        height_cm: height,
        dob: NaiveDate::from_ymd_opt(1990, 1, 1).unwrap(),
        weight_min: None,
        weight_max: None,
    }
}
fn measurement(profile_id: Option<i64>, weight: f64) -> MeasurementInput {
    MeasurementInput {
        measured_at: "2025-01-02T12:00:00Z".parse().unwrap(),
        weight_kg: weight,
        impedance_ohm: Some(500),
        profile_id,
    }
}

#[tokio::test]
async fn corrections_validate_recompute_preserve_evidence_and_roll_back() {
    let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
        return;
    };
    let admin = grammatic::store::connect(&url).await.unwrap();
    sqlx::query("DROP SCHEMA IF EXISTS correction_workflows CASCADE")
        .execute(&admin)
        .await
        .unwrap();
    sqlx::query("CREATE SCHEMA correction_workflows")
        .execute(&admin)
        .await
        .unwrap();
    let options = url
        .parse::<PgConnectOptions>()
        .unwrap()
        .options([("search_path", "correction_workflows")]);
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    sqlx::migrate!().run(&pool).await.unwrap();
    let corrections = Corrections::new(pool.clone(), MetricsPolicy::default());
    assert!(matches!(
        corrections.create_profile(profile(0.0)).await,
        Err(CorrectionError::InvalidInput(_))
    ));
    let id = corrections.create_profile(profile(175.0)).await.unwrap();
    let mid = corrections
        .create_measurement(measurement(Some(id), 70.0))
        .await
        .unwrap();
    let original_bmi: (f64,) = sqlx::query_as("SELECT bmi FROM measurements WHERE id=$1")
        .bind(mid)
        .fetch_one(&pool)
        .await
        .unwrap();
    corrections
        .update_profile(id, profile(180.0))
        .await
        .unwrap();
    let changed_bmi: (f64,) = sqlx::query_as("SELECT bmi FROM measurements WHERE id=$1")
        .bind(mid)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(changed_bmi.0 < original_bmi.0);

    assert!(matches!(
        corrections.update_profile(-1, profile(180.0)).await,
        Err(CorrectionError::NotFound)
    ));
    assert!(matches!(
        corrections
            .update_measurement(-1, measurement(None, 75.0))
            .await,
        Err(CorrectionError::NotFound)
    ));
    let mut future = measurement(None, 75.0);
    future.measured_at = Utc::now() + chrono::Duration::days(1);
    assert!(matches!(
        corrections.create_measurement(future).await,
        Err(CorrectionError::InvalidInput(_))
    ));
    assert!(matches!(
        corrections
            .create_measurement(measurement(Some(-1), 75.0))
            .await,
        Err(CorrectionError::Database(_))
    ));

    sqlx::query("UPDATE measurements SET raw_frame='captured', rssi=-60 WHERE id=$1")
        .bind(mid)
        .execute(&pool)
        .await
        .unwrap();
    let received: (chrono::DateTime<Utc>,) =
        sqlx::query_as("SELECT received_at FROM measurements WHERE id=$1")
            .bind(mid)
            .fetch_one(&pool)
            .await
            .unwrap();
    corrections
        .update_measurement(mid, measurement(Some(id), 71.0))
        .await
        .unwrap();
    let evidence: (String, Option<i16>, chrono::DateTime<Utc>) =
        sqlx::query_as("SELECT raw_frame,rssi,received_at FROM measurements WHERE id=$1")
            .bind(mid)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(evidence, ("captured".into(), Some(-60), received.0));

    // Fail the actual metric UPDATE after each workflow's initial write.
    sqlx::query("CREATE FUNCTION reject_recompute() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'test recompute failure'; END $$").execute(&pool).await.unwrap();
    sqlx::query("CREATE TRIGGER reject_recompute BEFORE UPDATE OF bmi ON measurements FOR EACH ROW EXECUTE FUNCTION reject_recompute()").execute(&pool).await.unwrap();
    assert!(matches!(
        corrections.update_profile(id, profile(190.0)).await,
        Err(CorrectionError::Recompute(_))
    ));
    let height: (f64,) = sqlx::query_as("SELECT height_cm FROM profiles WHERE id=$1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(height.0, 180.0);
    assert!(matches!(
        corrections
            .update_measurement(mid, measurement(Some(id), 80.0))
            .await,
        Err(CorrectionError::Recompute(_))
    ));
    let weight: (f64,) = sqlx::query_as("SELECT weight_kg FROM measurements WHERE id=$1")
        .bind(mid)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(weight.0, 71.0);
    assert!(matches!(
        corrections
            .create_measurement(measurement(Some(id), 80.0))
            .await,
        Err(CorrectionError::Recompute(_))
    ));
    let count: (i64,) = sqlx::query_as("SELECT count(*) FROM measurements")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count.0, 1);
    assert!(matches!(
        corrections.delete_profile(id).await,
        Err(CorrectionError::Recompute(_))
    ));
    let owner: (Option<i64>,) = sqlx::query_as("SELECT profile_id FROM measurements WHERE id=$1")
        .bind(mid)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(owner.0, Some(id));
    sqlx::query("DROP TRIGGER reject_recompute ON measurements")
        .execute(&pool)
        .await
        .unwrap();

    corrections.delete_profile(id).await.unwrap();
    let guest: (Option<i64>, Option<f64>) =
        sqlx::query_as("SELECT profile_id,bmi FROM measurements WHERE id=$1")
            .bind(mid)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(guest, (None, None));
    corrections.delete_measurement(mid).await.unwrap();
    assert!(matches!(
        corrections.delete_measurement(mid).await,
        Err(CorrectionError::NotFound)
    ));
    pool.close().await;
    sqlx::query("DROP SCHEMA correction_workflows CASCADE")
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}

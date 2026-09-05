//! End-to-end dedup check against a real Postgres.
//!
//! The dedup invariant spans three places that must agree — the capture
//! pipeline's minute-stable `measured_at`, the store's `ON CONFLICT` clause,
//! and the migration's unique index. Only a real database can pin all three.
//!
//! Self-skips unless `TEST_DATABASE_URL` points at a disposable database:
//!
//! ```sh
//! docker run --rm -d -p 5432:5432 -e POSTGRES_PASSWORD=mi postgres:16
//! TEST_DATABASE_URL=postgres://postgres:mi@localhost/postgres \
//!     cargo test --test dedup_integration
//! ```
//!
//! Each test runs in its own Postgres schema (same migrations, disjoint
//! tables): the tests share one process and one database, and parallel
//! threads must never see each other's rows.

use chrono::{DateTime, Datelike, FixedOffset, NaiveDate, Timelike, Utc};
use grammatic::capture::MeasurementRecord;
use grammatic::parser::Unit;
use grammatic::profile::MetricsPolicy;
use grammatic::store;

/// Fixed per-test schema names. `const` (not caller-supplied) so the
/// `sqlx::query!` audit lint can verify the dynamic DDL has no injection
/// surface — the names are identifiers baked into the binary.
macro_rules! isolated_pool {
    ($url:expr, $schema:ident) => {
        async {
            let admin = store::connect($url)
                .await
                .expect("connecting to TEST_DATABASE_URL");
            // Same migrations, disjoint tables per test: parallel threads must
            // never see each other's rows. `search_path` pins every unqualified
            // table reference (all of `store`'s SQL) into the schema.
            sqlx::query(concat!(
                "DROP SCHEMA IF EXISTS ",
                stringify!($schema),
                " CASCADE"
            ))
            .execute(&admin)
            .await
            .unwrap();
            sqlx::query(concat!("CREATE SCHEMA ", stringify!($schema)))
                .execute(&admin)
                .await
                .unwrap();
            admin.close().await;
            let mut options = $url.parse::<sqlx::postgres::PgConnectOptions>().unwrap();
            options = options.options([("search_path", stringify!($schema))]);
            let pool = sqlx::postgres::PgPoolOptions::new()
                .max_connections(5)
                .acquire_timeout(std::time::Duration::from_secs(5))
                .connect_with(options)
                .await
                .expect("connecting to isolated schema");
            sqlx::migrate!("./migrations")
                .run(&pool)
                .await
                .expect("migrating isolated schema");
            pool
        }
    };
}

macro_rules! drop_schema {
    ($pool:expr, $schema:ident) => {
        async {
            sqlx::query(concat!("DROP SCHEMA ", stringify!($schema), " CASCADE"))
                .execute($pool)
                .await
                .unwrap();
            $pool.close().await;
        }
    };
}

fn record(
    measured_at: DateTime<FixedOffset>,
    weight_kg: f64,
    impedance_ohm: Option<i32>,
) -> MeasurementRecord {
    MeasurementRecord {
        measured_at,
        clock_source: "scale".into(),
        received_at: measured_at,
        weight_kg,
        impedance_ohm,
        profile_id: None,
        unit: Unit::Kg,
        raw_frame: String::new(),
        rssi: None,
        metrics: None,
    }
}

#[tokio::test]
async fn re_broadcasts_collapse_and_distinct_weigh_ins_survive() {
    let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let pool = isolated_pool!(&url, dedup_rebroadcast).await;
    // Now-based key so reruns never collide with rows from earlier runs.
    let base: DateTime<FixedOffset> = Utc::now().fixed_offset();

    // Re-broadcast of the same final frame: the dedup key matches, the
    // second insert is suppressed.
    assert!(
        store::persist_measurement(&pool, &record(base, 77.7, Some(432)))
            .await
            .unwrap()
            .recorded()
    );
    assert!(
        !store::persist_measurement(&pool, &record(base, 77.7, Some(432)))
            .await
            .unwrap()
            .recorded()
    );

    // A distinct impedance at the same time and weight is a distinct key.
    assert!(
        store::persist_measurement(&pool, &record(base, 77.7, Some(433)))
            .await
            .unwrap()
            .recorded()
    );

    // Missing impedance: NULLs must not be distinct (COALESCE(-1) index).
    let no_impedance = base + chrono::Duration::minutes(1);
    assert!(
        store::persist_measurement(&pool, &record(no_impedance, 77.7, None))
            .await
            .unwrap()
            .recorded()
    );
    assert!(
        !store::persist_measurement(&pool, &record(no_impedance, 77.7, None))
            .await
            .unwrap()
            .recorded()
    );

    drop_schema!(&pool, dedup_rebroadcast).await;
}

#[tokio::test]
async fn advertisement_and_history_paths_share_one_dedup_key() {
    let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let pool = isolated_pool!(&url, dedup_adv_history).await;
    // Now-based key so reruns never collide with rows from earlier runs.
    let base: DateTime<FixedOffset> = Utc::now().fixed_offset();

    // Same weigh-in recorded once via the live stabilized frame...
    assert!(
        store::persist_measurement(&pool, &record(base, 75.0, Some(500)))
            .await
            .unwrap()
            .recorded()
    );
    // ...then re-delivered by a history pull (seconds dropped, minute-exact
    // key): collapses onto the same row.
    assert!(
        !store::persist_measurement(&pool, &record(base, 75.0, Some(500)))
            .await
            .unwrap()
            .recorded()
    );

    // History entry with its own impedance carries it (no attach rule on
    // this path).
    let other = base + chrono::Duration::minutes(1);
    assert!(
        store::persist_measurement(&pool, &record(other, 75.0, None))
            .await
            .unwrap()
            .recorded()
    );

    // Soak 2026-09-04: the pull enriches the weight-only live sibling
    // instead of inserting a second row; the superseded frame hex is
    // returned for the log.
    let grammatic::capture::PersistOutcome::Enriched(enriched) =
        store::persist_measurement(&pool, &record(other, 75.0, Some(500)))
            .await
            .unwrap()
    else {
        panic!("weight-only sibling should be enriched")
    };
    assert_eq!(enriched.superseded_raw_frame, "");
    let row: (Option<i32>,) = sqlx::query_as(
        "SELECT impedance_ohm FROM measurements WHERE measured_at = $1 AND weight_kg = $2",
    )
    .bind(other)
    .bind(75.0)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.0, Some(500));
    assert!(
        !store::persist_measurement(&pool, &record(other, 75.0, Some(500)))
            .await
            .unwrap()
            .recorded()
    );
    let later = base + chrono::Duration::minutes(2);
    assert!(
        store::persist_measurement(&pool, &record(later, 75.0, Some(500)))
            .await
            .unwrap()
            .recorded()
    );
    assert!(
        !store::persist_measurement(&pool, &record(later, 75.0, None))
            .await
            .unwrap()
            .recorded()
    );
    assert!(
        store::persist_measurement(&pool, &record(later, 76.0, None))
            .await
            .unwrap()
            .recorded()
    );

    drop_schema!(&pool, dedup_adv_history).await;
}

#[tokio::test]
async fn capture_records_history_entries_end_to_end() {
    let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    use grammatic::capture::Capture;
    use grammatic::spool::Spool;

    let pool = isolated_pool!(&url, dedup_capture_e2e).await;
    let spool = std::sync::Arc::new(Spool::new(
        std::env::temp_dir().join(format!(
            "grammatic-dedup-history-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        )),
        4096,
    ));
    let capture = Capture::with_metrics_policy(pool.clone(), spool, MetricsPolicy::keep_all());

    // 13-byte history entry (spike-confirmed: byte-identical frame): unit
    // 0x02, flags 0x22, clock now-ish (minute resolution), seconds 44 in
    // byte 8 (dropped from the key), impedance 500, 75 kg.
    let received = chrono::Local::now();
    let frame_ts = received.naive_local();
    let mut entry = vec![0u8; 13];
    entry[0] = 0x02;
    entry[1] = 0x22;
    entry[2] = (frame_ts.year() & 0xFF) as u8;
    entry[3] = ((frame_ts.year() >> 8) & 0xFF) as u8;
    entry[4] = frame_ts.month() as u8;
    entry[5] = frame_ts.day() as u8;
    entry[6] = frame_ts.hour() as u8;
    entry[7] = frame_ts.minute() as u8;
    entry[8] = 44;
    entry[9] = (500 & 0xFF) as u8;
    entry[10] = (500 >> 8) as u8;
    entry[11] = (15000 & 0xFF) as u8;
    entry[12] = (15000 >> 8) as u8;

    let result = capture
        .handle_history_entry(&entry, received, None)
        .await
        .expect("history entry should parse");
    assert!(result.recorded);

    // Re-pull collapses via the dedup constraint.
    let again = capture
        .handle_history_entry(&entry, received, None)
        .await
        .expect("history entry should parse");
    assert!(!again.recorded);

    drop_schema!(&pool, dedup_capture_e2e).await;
}

#[tokio::test]
async fn recent_history_is_strictly_before_and_newest_first() {
    let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let pool = isolated_pool!(&url, dedup_recent_history).await;
    let profile = store::add_profile(
        &pool,
        &format!("history-probe-{}", Utc::now().timestamp_millis()),
        grammatic::metrics::Sex::Male,
        175.0,
        NaiveDate::from_ymd_opt(1996, 1, 1).unwrap(),
        Some(30.0),
        Some(150.0),
    )
    .await
    .unwrap();
    // Now-based key so reruns never collide with rows from earlier runs.
    let base: DateTime<FixedOffset> = Utc::now().fixed_offset();

    for (offset_mins, weight) in [(0, 70.0), (1, 71.0), (2, 72.0)] {
        let at = base + chrono::Duration::minutes(offset_mins);
        assert!(
            store::persist_measurement(
                &pool,
                &MeasurementRecord {
                    measured_at: at,
                    clock_source: "scale".into(),
                    received_at: at,
                    weight_kg: weight,
                    impedance_ohm: Some(500),
                    profile_id: Some(profile),
                    unit: Unit::Kg,
                    raw_frame: String::new(),
                    rssi: None,
                    metrics: None,
                },
            )
            .await
            .unwrap()
            .recorded()
        );
    }

    // Capped at 2 and newest first.
    let history = store::recent_history(&pool, profile, base + chrono::Duration::minutes(3), 2)
        .await
        .unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].weight_kg, 72.0);
    assert_eq!(history[1].weight_kg, 71.0);

    // The bound is strict: a point stamped at the current dedup-key time is
    // invisible, so a re-broadcast never observes the row it re-observes.
    let strict = store::recent_history(&pool, profile, base + chrono::Duration::minutes(2), 5)
        .await
        .unwrap();
    assert_eq!(strict.len(), 2);
    assert_eq!(strict[0].weight_kg, 71.0);

    // Recompute honors the storage policy: "none" clears the columns it
    // would otherwise fill. Scoped to this schema, exactly the 3 rows
    // above exist.
    let recomputed = store::recompute_metrics(&pool, None, &MetricsPolicy::None)
        .await
        .unwrap();
    assert_eq!(recomputed, 3);

    drop_schema!(&pool, dedup_recent_history).await;
}

#[tokio::test]
async fn concurrent_siblings_converge_and_conflicting_impedance_survives() {
    let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
        return;
    };
    let pool = isolated_pool!(&url, dedup_concurrent).await;
    let base = Utc::now().fixed_offset();
    for round in 0..8 {
        let at = base + chrono::Duration::minutes(round);
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(12));
        let mut tasks = Vec::new();
        for index in 0..12 {
            let pool = pool.clone();
            let barrier = barrier.clone();
            let row = record(
                at,
                75.0,
                match index % 3 {
                    0 => None,
                    1 => Some(500),
                    _ => Some(501),
                },
            );
            tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                store::persist_measurement(&pool, &row).await.unwrap()
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }
        let rows: Vec<(Option<i32>,)> = sqlx::query_as(
            "SELECT impedance_ohm FROM measurements WHERE measured_at=$1 ORDER BY impedance_ohm",
        )
        .bind(at)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(rows, vec![(Some(500),), (Some(501),)]);
    }
    drop_schema!(&pool, dedup_concurrent).await;
}

#[tokio::test]
async fn persistence_preserves_first_observation_and_rolls_back_failures() {
    let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
        return;
    };
    let pool = isolated_pool!(&url, dedup_first_observation).await;
    let at = Utc::now().fixed_offset();
    let mut first = record(at, 75.0, None);
    first.raw_frame = "original".into();
    first.rssi = Some(-60);
    store::persist_measurement(&pool, &first).await.unwrap();
    let mut later = record(at, 75.0, Some(500));
    later.received_at = at + chrono::Duration::minutes(5);
    later.rssi = Some(-30);
    later.raw_frame = "history".into();
    later.profile_id = Some(-999);
    assert!(store::persist_measurement(&pool, &later).await.is_err());
    later.profile_id = None;
    let grammatic::capture::PersistOutcome::Enriched(outcome) =
        store::persist_measurement(&pool, &later).await.unwrap()
    else {
        panic!("failed write must leave the weight-only row intact")
    };
    assert_eq!(outcome.superseded_raw_frame, "original");
    let row: (DateTime<FixedOffset>, Option<i16>, String) =
        sqlx::query_as("SELECT received_at,rssi,raw_frame FROM measurements WHERE id=$1")
            .bind(outcome.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        row.0.timestamp_micros(),
        first.received_at.timestamp_micros()
    );
    assert_eq!(row.1, first.rssi);
    assert_eq!(row.2, "history");
    drop_schema!(&pool, dedup_first_observation).await;
}

//! Same-origin dashboard API. All mutations validate inputs and recompute metrics.
use crate::corrections::{CorrectionError, Corrections, MeasurementInput, ProfileInput};
use crate::profile::MetricsPolicy;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use serde_json::{Value, json};
use sqlx::PgPool;

#[derive(Clone)]
struct AppState {
    pool: PgPool,
    policy: MetricsPolicy,
    corrections: Corrections,
}
struct ApiError(StatusCode, String);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1}))).into_response()
    }
}
impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        if let Some(db) = e.as_database_error() {
            if db.is_unique_violation() {
                return Self(
                    StatusCode::CONFLICT,
                    "This profile or measurement already exists.".into(),
                );
            }
            if db.is_foreign_key_violation() {
                return bad("The selected profile no longer exists.");
            }
        }
        tracing::error!(error = %e, "dashboard database operation failed");
        Self(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Database operation failed. Please retry.".into(),
        )
    }
}
fn bad(message: &str) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, message.into())
}
type Result<T> = std::result::Result<T, ApiError>;

pub async fn serve(
    pool: PgPool,
    policy: MetricsPolicy,
    bind: &str,
    frontend: std::path::PathBuf,
) -> anyhow::Result<()> {
    let app = Router::new()
        .route("/api/health", get(health))
        .route("/api/profiles", get(profiles).post(create_profile))
        .route(
            "/api/profiles/{id}",
            axum::routing::put(update_profile).delete(delete_profile),
        )
        .route(
            "/api/measurements",
            get(measurements).post(create_measurement),
        )
        .route(
            "/api/measurements/{id}",
            axum::routing::put(update_measurement).delete(delete_measurement),
        )
        .route(
            "/api/{*path}",
            get(|| async {
                (
                    StatusCode::NOT_FOUND,
                    Json(json!({"error":"Unknown API route"})),
                )
            }),
        )
        .fallback_service(tower_http::services::ServeDir::new(&frontend).fallback(
            tower_http::services::ServeFile::new(frontend.join("index.html")),
        ))
        .layer(tower_http::set_header::SetResponseHeaderLayer::overriding(
            axum::http::header::CACHE_CONTROL,
            axum::http::HeaderValue::from_static("no-store"),
        ))
        .with_state(AppState {
            corrections: Corrections::new(pool.clone(), policy),
            pool,
            policy,
        });
    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!(%bind, "dashboard listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
async fn health(State(s): State<AppState>) -> Result<Json<Value>> {
    sqlx::query("SELECT 1").execute(&s.pool).await?;
    Ok(Json(
        json!({"database":"connected", "scale":"Mi Body Composition Scale 2", "metrics_policy":format!("{:?}", s.policy)}),
    ))
}
async fn profiles(State(s): State<AppState>) -> Result<Json<Value>> {
    let rows: Vec<(Value,)> = sqlx::query_as("SELECT to_jsonb(p) FROM profiles p ORDER BY id")
        .fetch_all(&s.pool)
        .await?;
    Ok(Json(Value::Array(rows.into_iter().map(|r| r.0).collect())))
}
async fn measurements(State(s): State<AppState>) -> Result<Json<Value>> {
    let rows: Vec<(Value,)> =
        sqlx::query_as("SELECT to_jsonb(m) FROM measurements m ORDER BY measured_at DESC, id DESC")
            .fetch_all(&s.pool)
            .await?;
    Ok(Json(Value::Array(rows.into_iter().map(|r| r.0).collect())))
}
impl From<CorrectionError> for ApiError {
    fn from(error: CorrectionError) -> Self {
        match error {
            CorrectionError::InvalidInput(message) => bad(message),
            CorrectionError::NotFound => {
                Self(StatusCode::NOT_FOUND, "Record no longer exists.".into())
            }
            CorrectionError::Database(error) => error.into(),
            CorrectionError::Recompute(error) => {
                tracing::error!(%error, "recompute failed");
                Self(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Metric computation failed. No changes were saved.".into(),
                )
            }
        }
    }
}
async fn create_profile(
    State(s): State<AppState>,
    Json(input): Json<ProfileInput>,
) -> Result<Json<Value>> {
    let id = s.corrections.create_profile(input).await?;
    Ok(Json(json!({"id":id})))
}
async fn update_profile(
    State(s): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<ProfileInput>,
) -> Result<Json<Value>> {
    let id = s.corrections.update_profile(id, input).await?;
    Ok(Json(json!({"id":id})))
}
async fn delete_profile(State(s): State<AppState>, Path(id): Path<i64>) -> Result<StatusCode> {
    s.corrections.delete_profile(id).await?;
    Ok(StatusCode::NO_CONTENT)
}
async fn create_measurement(
    State(s): State<AppState>,
    Json(input): Json<MeasurementInput>,
) -> Result<Json<Value>> {
    let id = s.corrections.create_measurement(input).await?;
    Ok(Json(json!({"id":id})))
}
async fn update_measurement(
    State(s): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<MeasurementInput>,
) -> Result<Json<Value>> {
    let id = s.corrections.update_measurement(id, input).await?;
    Ok(Json(json!({"id":id})))
}
async fn delete_measurement(State(s): State<AppState>, Path(id): Path<i64>) -> Result<StatusCode> {
    s.corrections.delete_measurement(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

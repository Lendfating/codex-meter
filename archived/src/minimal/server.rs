use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Html,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;
use thiserror::Error;

use super::{
    build_report, ccusage::CcusageCollector, db::DbError, jsonl::JsonlError, refresh_rollups,
    Database, JsonlCollector,
};

const MINIMAL_TABLE_COUNT_SQL: &str = "SELECT COUNT(*) FROM sqlite_master
    WHERE type = 'table' AND name IN
    ('source_jsonl','source_app_server','source_ccusage','usage_daily','usage_minute','usage_session','capacities_v2')";

const INDEX_HTML: &str = include_str!("../../web/index.html");

#[derive(Clone)]
pub struct AppState {
    pub database: Database,
    pub collector: JsonlCollector,
    pub ccusage: CcusageCollector,
}

#[derive(Debug, Error)]
pub enum ServerError {
    #[error("database error: {0}")]
    Database(#[from] DbError),
    #[error("JSONL error: {0}")]
    Jsonl(#[from] JsonlError),
    #[error("report error: {0}")]
    Report(#[from] super::report::ReportError),
    #[error("rollup error: {0}")]
    Rollup(#[from] super::rollup::RollupError),
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("query error: {0}")]
    Sqlx(#[from] sqlx::Error),
}

impl axum::response::IntoResponse for ServerError {
    fn into_response(self) -> axum::response::Response {
        let status = match self {
            Self::InvalidRequest(_) => StatusCode::BAD_REQUEST,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, Json(json!({"error": self.to_string()}))).into_response()
    }
}

#[derive(Debug, Deserialize)]
pub struct ReportQuery {
    pub date: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CapacityInput {
    pub plan_code: String,
    pub credit: Option<f64>,
    pub effective_from_ms: Option<i64>,
    pub status: Option<String>,
    pub note: Option<String>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/health", get(health))
        .route("/api/report", get(report))
        .route("/api/refresh", post(refresh))
        .route("/api/capacities", post(save_capacity))
        .with_state(state)
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn health(State(state): State<AppState>) -> Result<Json<Value>, ServerError> {
    let tables: i64 = sqlx::query_scalar(MINIMAL_TABLE_COUNT_SQL)
        .fetch_one(state.database.pool())
        .await?;
    Ok(Json(json!({
        "status": "ok",
        "schema": "minimal-seven",
        "tables": tables,
        "jsonl_home": state.collector.home_display()
    })))
}

async fn report(
    State(state): State<AppState>,
    Query(query): Query<ReportQuery>,
) -> Result<Json<Value>, ServerError> {
    Ok(Json(
        build_report(&state.database, query.date.as_deref()).await?,
    ))
}

async fn refresh(State(state): State<AppState>) -> Result<Json<Value>, ServerError> {
    let scan = state.collector.scan_once(&state.database).await?;
    let rollup = refresh_rollups(&state.database).await?;
    let validation = if state.ccusage.run_on_refresh() {
        match state.ccusage.run_once(&state.database).await {
            Ok(summary) => {
                serde_json::to_value(summary).unwrap_or_else(|_| json!({"status": "ok"}))
            }
            Err(error) => json!({"status": "failed", "error": error.to_string()}),
        }
    } else {
        Value::Null
    };
    let report = build_report(&state.database, None).await?;
    Ok(Json(json!({
        "status": "ok",
        "scan": {
            "files": scan.files_scanned,
            "changed": scan.files_changed,
            "lines": scan.complete_lines,
            "events": scan.inserted_events,
            "duplicates": scan.duplicate_events,
            "quota_samples": scan.inserted_quota_samples
        },
        "validation": validation,
        "rollup": {
            "days": rollup.days,
            "minutes": rollup.minutes,
            "sessions": rollup.sessions,
            "windows": rollup.windows
        },
        "report": report
    })))
}

async fn save_capacity(
    State(state): State<AppState>,
    Json(input): Json<CapacityInput>,
) -> Result<Json<Value>, ServerError> {
    if !matches!(input.plan_code.as_str(), "usd20" | "usd100" | "usd200") {
        return Err(ServerError::InvalidRequest(
            "plan_code must be usd20, usd100 or usd200".to_owned(),
        ));
    }
    if input
        .credit
        .is_some_and(|credit| !credit.is_finite() || credit < 0.0)
    {
        return Err(ServerError::InvalidRequest(
            "credit must be a finite non-negative number".to_owned(),
        ));
    }
    let Some(credit) = input.credit else {
        return Err(ServerError::InvalidRequest(
            "confirmed capacity requires a Credit value".to_owned(),
        ));
    };
    let status = input.status.unwrap_or_else(|| "confirmed".to_owned());
    if status != "confirmed" {
        return Err(ServerError::InvalidRequest(
            "the minimal capacity table only stores confirmed values".to_owned(),
        ));
    }
    let effective_from_ms = input.effective_from_ms.unwrap_or_else(now_ms);
    let account = sqlx::query(
        "SELECT account_key, plan_type FROM source_app_server
         WHERE kind = 'account' ORDER BY last_seen_at_ms DESC, id DESC LIMIT 1",
    )
    .fetch_optional(state.database.pool())
    .await?;
    let account_key = account
        .as_ref()
        .and_then(|row| row.try_get::<Option<String>, _>("account_key").ok())
        .flatten();
    let plan_type = account
        .as_ref()
        .and_then(|row| row.try_get::<Option<String>, _>("plan_type").ok())
        .flatten();
    let result = sqlx::query(
        "INSERT INTO capacities_v2
            (profile_code, account_key, plan_type, weekly_credit,
             effective_from_ms, confirmed_at_ms)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&input.plan_code)
    .bind(&account_key)
    .bind(&plan_type)
    .bind(credit)
    .bind(effective_from_ms)
    .bind(now_ms())
    .execute(state.database.pool())
    .await?;
    Ok(Json(json!({
        "status": "ok",
        "id": result.last_insert_rowid(),
        "plan_code": input.plan_code,
        "credit": credit,
        "effective_from_ms": effective_from_ms,
        "state": status
    })))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|value| i64::try_from(value.as_millis()).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use axum::{
        body::{to_bytes, Body},
        http::Request,
    };
    use tower::ServiceExt;

    use super::*;

    #[tokio::test]
    async fn minimal_routes_expose_report_and_health() {
        let database = Database::connect_in_memory().await.unwrap();
        sqlx::query(
            "INSERT INTO usage_daily(local_date, total_tokens, reset_count)
             VALUES ('2026-08-06', 42, 0)",
        )
        .execute(database.pool())
        .await
        .unwrap();
        let app = router(AppState {
            database,
            collector: JsonlCollector::new("/tmp/does-not-exist"),
            ccusage: CcusageCollector::disabled("/tmp/does-not-exist", "Asia/Shanghai"),
        });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/report")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            value["methodology"]["calculation_version"],
            "minimal-r1-rollup"
        );
        assert_eq!(value["days"][0]["usage"]["total"], 42);
    }

    #[tokio::test]
    async fn capacity_write_is_small_and_machine_local() {
        let database = Database::connect_in_memory().await.unwrap();
        let app = router(AppState {
            database,
            collector: JsonlCollector::new("/tmp/does-not-exist"),
            ccusage: CcusageCollector::disabled("/tmp/does-not-exist", "Asia/Shanghai"),
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/capacities")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"plan_code":"usd100","credit":200,"status":"confirmed"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}

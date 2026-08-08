//! The four-route local HTTP surface.

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
use std::sync::{atomic::AtomicBool, Arc};
use thiserror::Error;

use crate::{
    db::{CapacityRecord, Database, DbError},
    pipelines::{
        result::materialize::{refresh_rollups, MaterializeError},
        source::{
            ccusage::{CcusageCollector, CcusageError},
            jsonl::{JsonlCollector, JsonlError},
        },
    },
    service::report::{build_report, ReportError},
};

const INDEX_HTML: &str = include_str!("../../web/index.html");

#[derive(Clone)]
pub struct AppState {
    pub database: Database,
    pub collector: JsonlCollector,
    pub ccusage: CcusageCollector,
    pub scan_complete: Arc<AtomicBool>,
}

#[derive(Debug, Error)]
pub enum ServiceError {
    #[error("database error: {0}")]
    Database(#[from] DbError),
    #[error("JSONL error: {0}")]
    Jsonl(#[from] JsonlError),
    #[error("materialize error: {0}")]
    Materialize(#[from] MaterializeError),
    #[error("ccusage error: {0}")]
    Ccusage(#[from] CcusageError),
    #[error("report error: {0}")]
    Report(#[from] ReportError),
    #[error("invalid request: {0}")]
    InvalidRequest(String),
}

impl axum::response::IntoResponse for ServiceError {
    fn into_response(self) -> axum::response::Response {
        let status = if matches!(self, Self::InvalidRequest(_)) {
            StatusCode::BAD_REQUEST
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
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
    pub credit: f64,
    pub effective_from_ms: Option<i64>,
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

async fn health(State(state): State<AppState>) -> Result<Json<Value>, ServiceError> {
    Ok(Json(
        json!({"status":"ok","schema":"minimal-eight","tables":state.database.table_count().await?,"jsonl_home":state.collector.home_display(),"jsonl_scan_complete":state.scan_complete.load(std::sync::atomic::Ordering::Relaxed)}),
    ))
}

async fn report(
    State(state): State<AppState>,
    Query(query): Query<ReportQuery>,
) -> Result<Json<Value>, ServiceError> {
    Ok(Json(
        build_report(&state.database, query.date.as_deref()).await?,
    ))
}

async fn refresh(State(state): State<AppState>) -> Result<Json<Value>, ServiceError> {
    let scan = state.collector.scan_once(&state.database).await?;
    let rollup = refresh_rollups(&state.database).await?;
    let validation = match state.ccusage.run_once(&state.database).await {
        Ok(summary) => serde_json::to_value(summary).unwrap_or_else(|_| json!({"status":"ok"})),
        Err(error) => json!({"status":"failed","error":error.to_string()}),
    };
    Ok(Json(
        json!({"status":"ok","scan":scan,"rollup":rollup,"validation":validation,"report":build_report(&state.database,None).await?}),
    ))
}

async fn save_capacity(
    State(state): State<AppState>,
    Json(input): Json<CapacityInput>,
) -> Result<Json<Value>, ServiceError> {
    if !matches!(input.plan_code.as_str(), "usd20" | "usd100" | "usd200") {
        return Err(ServiceError::InvalidRequest(
            "plan_code must be usd20, usd100 or usd200".to_owned(),
        ));
    }
    if !input.credit.is_finite() || input.credit < 0.0 {
        return Err(ServiceError::InvalidRequest(
            "credit must be a finite non-negative number".to_owned(),
        ));
    }
    let account = state
        .database
        .list_source_app_server()
        .await?
        .into_iter()
        .filter(|row| row.try_get::<String, _>("kind").ok().as_deref() == Some("account"))
        .max_by_key(|row| row.try_get::<i64, _>("last_seen_at_ms").unwrap_or_default());
    let account_key = account.as_ref().and_then(|row| {
        row.try_get::<Option<String>, _>("account_key")
            .ok()
            .flatten()
    });
    let plan_type = account
        .as_ref()
        .and_then(|row| row.try_get::<Option<String>, _>("plan_type").ok().flatten());
    let effective_from_ms = input.effective_from_ms.unwrap_or_else(now_ms);
    let id = state
        .database
        .upsert_capacity(&CapacityRecord {
            profile_code: input.plan_code.clone(),
            account_key: account_key.clone(),
            plan_type: plan_type.clone(),
            weekly_credit: input.credit,
            effective_from_ms,
            effective_to_ms: None,
            confirmed_at_ms: now_ms(),
        })
        .await?;
    Ok(Json(
        json!({"status":"ok","id":id,"plan_code":input.plan_code,"credit":input.credit,"effective_from_ms":effective_from_ms}),
    ))
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
    use super::*;
    use crate::db::UsageWindowRecord;
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    #[tokio::test]
    async fn exposes_health_and_report_routes() {
        let database = Database::connect_in_memory().await.unwrap();
        database
            .replace_rollups(
                &[],
                &[],
                &[UsageWindowRecord {
                    window_id: "window:test".to_owned(),
                    window_kind: "primary".to_owned(),
                    window_start_ms: Some(1),
                    official_percent_start: Some(10.0),
                    official_percent_end: Some(12.0),
                    ..Default::default()
                }],
                &[],
            )
            .await
            .unwrap();
        let app = router(AppState {
            database,
            collector: JsonlCollector::new("/tmp/no-codex"),
            ccusage: CcusageCollector::disabled("/tmp/no-codex", "Asia/Shanghai"),
            scan_complete: Arc::new(AtomicBool::new(true)),
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
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let report: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(report["quota_windows"][0]["window_id"], "window:test");
    }
}

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
use std::sync::{Arc, Mutex};
use thiserror::Error;

use crate::{
    db::{CapacityRecord, Database, DbError},
    pipelines::{
        result::materialize::{refresh_rollups, MaterializeError},
        source::{
            app_server::{poll_once, AppServerConfig},
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
    pub sync_state: SyncState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyncPhase {
    Starting,
    Scanning,
    Materializing,
    Ready,
    Failed,
}

impl SyncPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Scanning => "scanning",
            Self::Materializing => "materializing",
            Self::Ready => "ready",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Debug)]
pub struct SyncSnapshot {
    pub phase: SyncPhase,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct SyncState {
    snapshot: Arc<Mutex<SyncSnapshot>>,
}

impl SyncState {
    pub fn new() -> Self {
        Self {
            snapshot: Arc::new(Mutex::new(SyncSnapshot {
                phase: SyncPhase::Starting,
                error: None,
            })),
        }
    }

    pub fn set_phase(&self, phase: SyncPhase) {
        let mut snapshot = self
            .snapshot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        snapshot.phase = phase;
        snapshot.error = None;
    }

    pub fn set_failed(&self, error: impl Into<String>) {
        let mut snapshot = self
            .snapshot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        snapshot.phase = SyncPhase::Failed;
        snapshot.error = Some(error.into());
    }

    pub fn snapshot(&self) -> SyncSnapshot {
        self.snapshot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
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
        .route("/api/sync/progress", get(sync_progress))
        .with_state(state)
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn health(State(state): State<AppState>) -> Result<Json<Value>, ServiceError> {
    let sync = state.sync_state.snapshot();
    let ready = sync.phase == SyncPhase::Ready;
    Ok(Json(json!({
        "status":"ok",
        "schema":"minimal-eight",
        "tables":state.database.table_count().await?,
        "jsonl_home":state.collector.home_display(),
        "jsonl_scan_complete":ready,
        "data_ready":ready,
        "sync_phase":sync.phase.as_str(),
        "sync_error":sync.error
    })))
}

async fn sync_progress(State(state): State<AppState>) -> Result<Json<Value>, ServiceError> {
    let sync = state.sync_state.snapshot();
    let ready = sync.phase == SyncPhase::Ready;
    let (days_synced, last_record_ms) = if ready {
        let days_synced = state.database.list_usage_daily().await?.len();
        let last_record_ms = state
            .database
            .list_source_jsonl()
            .await?
            .iter()
            .filter_map(|row| {
                if row.try_get::<String, _>("kind").ok()? == "usage" {
                    row.try_get::<i64, _>("observed_at_ms").ok()
                } else {
                    None
                }
            })
            .max();
        (Some(days_synced), last_record_ms)
    } else {
        (None, None)
    };
    Ok(Json(json!({
        "percent": if ready { Some(100) } else { None::<i32> },
        "phase": sync.phase.as_str(),
        "ready": ready,
        "failed": sync.phase == SyncPhase::Failed,
        "error": sync.error,
        "days_synced": days_synced,
        "total_days": None::<i32>,
        "last_record_ms": last_record_ms
    })))
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
    let scan_result = state.collector.scan_once(&state.database).await;
    let scan_ok = scan_result.is_ok();
    let scan = match scan_result {
        Ok(report) => serde_json::to_value(report).unwrap_or_else(|_| json!({"status":"ok"})),
        Err(error) => json!({"status":"failed","error":error.to_string()}),
    };
    let app_server_result = poll_once(&state.database, &AppServerConfig::default(), true).await;
    let app_server_ok = app_server_result.is_ok();
    let app_server = match app_server_result {
        Ok(report) => json!({"status":"ok","report":report}),
        Err(error) => json!({"status":"failed","error":error.to_string()}),
    };
    let validation_result = state.ccusage.run_once(&state.database).await;
    let validation = match validation_result {
        Ok(summary) => serde_json::to_value(summary).unwrap_or_else(|_| json!({"status":"ok"})),
        Err(error) => json!({"status":"failed","error":error.to_string()}),
    };
    let validation_ok = validation["status"].as_str() == Some("ok");
    let rollup = refresh_rollups(&state.database).await?;
    let status = if scan_ok && app_server_ok && validation_ok {
        "ok"
    } else {
        "partial"
    };
    Ok(Json(
        json!({"status":status,"scan":scan,"app_server":app_server,"rollup":rollup,"validation":validation,"report":build_report(&state.database,None).await?}),
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
            sync_state: {
                let sync_state = SyncState::new();
                sync_state.set_phase(SyncPhase::Ready);
                sync_state
            },
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
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let health: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(health["data_ready"], true);
        assert_eq!(health["sync_phase"], "ready");
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/sync/progress")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let progress: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(progress["ready"], true);
        assert_eq!(progress["phase"], "ready");
        assert_eq!(progress["percent"], 100);
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

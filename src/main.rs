pub mod config;
pub mod db;
pub mod pipelines;
pub mod pricing;
pub mod service;

use std::path::PathBuf;
use std::sync::{atomic::AtomicBool, Arc};

use config::ProjectConfig;
use pipelines::{
    result::materialize::refresh_rollups,
    source::{
        app_server::{poll_once, AppServerConfig},
        ccusage::CcusageCollector,
        jsonl::JsonlCollector,
    },
};
use service::http::{router, AppState};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let project = ProjectConfig::embedded()?;
    let runtime_dir = PathBuf::from(".runtime");
    std::fs::create_dir_all(&runtime_dir)?;
    let database = db::Database::connect(runtime_dir.join("codex-meter.sqlite")).await?;
    let codex_home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
        .unwrap_or_else(|| PathBuf::from(".codex"));
    let collector = JsonlCollector::new(codex_home)
        .with_cursor_path(Some(runtime_dir.join("jsonl-cursors.json")));
    let scan_complete = Arc::new(AtomicBool::new(false));
    let ccusage =
        CcusageCollector::from_env(collector.home_display(), project.app.timezone.clone());
    // ccusage reconciliation is opt-in and runs once on boot, then hourly.
    // Each run executes 8 subprocesses (API/subscription x daily/session x
    // auto/standard), so hourly is intentionally conservative; failures are
    // non-fatal because JSONL remains the primary ledger.
    if ccusage.enabled() {
        let ccusage_database = database.clone();
        let ccusage_collector = ccusage.clone();
        tokio::spawn(async move {
            loop {
                if let Err(error) = ccusage_collector.run_once(&ccusage_database).await {
                    eprintln!("ccusage reconciliation failed; JSONL remains primary: {error}");
                }
                tokio::time::sleep(std::time::Duration::from_secs(60 * 60)).await;
            }
        });
    }
    let refresh_database = database.clone();
    let refresh_collector = collector.clone();
    let refresh_scan_complete = Arc::clone(&scan_complete);
    tokio::spawn(async move {
        loop {
            match refresh_collector.scan_once(&refresh_database).await {
                Ok(scan) => {
                    if !refresh_scan_complete.load(std::sync::atomic::Ordering::Relaxed) {
                        refresh_scan_complete.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                    if scan.inserted_events > 0 || scan.inserted_quota_samples > 0 {
                        if let Err(error) = refresh_rollups(&refresh_database).await {
                            eprintln!("materialize refresh failed: {error}");
                        }
                    }
                }
                Err(error) => eprintln!("JSONL refresh failed: {error}"),
            }
            tokio::time::sleep(std::time::Duration::from_secs(5 * 60)).await;
        }
    });
    // App Server is the only source of current account/quota facts. JSONL
    // remains the primary usage source; App Server failures are non-fatal.
    let app_database = database.clone();
    let app_config = AppServerConfig::default();
    tokio::spawn(async move {
        let mut last_usage_at = std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(6 * 60 * 60))
            .unwrap_or_else(std::time::Instant::now);
        loop {
            let usage_due =
                last_usage_at.elapsed() >= std::time::Duration::from_secs(6 * 60 * 60);
            match poll_once(&app_database, &app_config, usage_due).await {
                Ok(_) => {
                    if let Err(error) = refresh_rollups(&app_database).await {
                        eprintln!("materialize refresh after App Server failed: {error}");
                    }
                }
                Err(error) => {
                    eprintln!("App Server unavailable; JSONL remains primary: {error}")
                }
            }
            if usage_due {
                last_usage_at = std::time::Instant::now();
            }
            tokio::time::sleep(app_config.poll_interval).await;
        }
    });
    let bind = std::env::var("CODEX_METER_BIND").unwrap_or(project.app.bind);
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    let state = AppState {
        database,
        collector,
        ccusage,
        scan_complete,
    };
    axum::serve(listener, router(state)).await?;
    Ok(())
}

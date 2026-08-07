//! Small loopback service for the Codex usage report.

use std::{path::PathBuf, time::Duration};

use codex_meter::minimal::{
    app_server::{spawn_supervisor, AppServerConfig},
    ccusage::CcusageCollector,
    refresh_rollups,
    server::{router, AppState},
    Database, JsonlCollector,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let database_path = std::env::var_os("CODEX_METER_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".runtime/codex-meter-seven.sqlite"));
    let codex_home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
        .unwrap_or_else(|| PathBuf::from(".codex"));
    let timezone =
        std::env::var("CODEX_METER_TIMEZONE").unwrap_or_else(|_| "Asia/Shanghai".to_owned());
    let database = Database::connect(&database_path).await?;
    let collector = JsonlCollector::new(codex_home.clone()).with_timezone(timezone.clone());
    let ccusage = CcusageCollector::from_env(codex_home, timezone);
    match collector.scan_once(&database).await {
        Ok(scan) => {
            eprintln!(
                "JSONL: {} files, {} events, {} quota samples",
                scan.files_scanned, scan.inserted_events, scan.inserted_quota_samples
            );
            match refresh_rollups(&database).await {
                Ok(summary) => eprintln!(
                    "Rollup: {} days, {} minutes, {} sessions, {} windows",
                    summary.days, summary.minutes, summary.sessions, summary.windows
                ),
                Err(error) => eprintln!("Rollup refresh unavailable: {error}"),
            }
        }
        Err(error) => eprintln!("JSONL scan unavailable; service remains usable: {error}"),
    }

    if std::env::var_os("CODEX_METER_DISABLE_COLLECTORS").is_none() {
        let refresh_database = database.clone();
        let refresh_collector = collector.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(10));
            loop {
                interval.tick().await;
                match refresh_collector.scan_once(&refresh_database).await {
                    Err(error) => eprintln!("JSONL refresh failed: {error}"),
                    Ok(scan) if scan.inserted_events > 0 || scan.inserted_quota_samples > 0 => {
                        if let Err(error) = refresh_rollups(&refresh_database).await {
                            eprintln!("Rollup refresh failed: {error}");
                        }
                    }
                    Ok(_) => {}
                }
            }
        });
    }
    if std::env::var_os("CODEX_METER_APP_SERVER_ON_BOOT").is_some() {
        let mut config = AppServerConfig::default();
        if let Ok(seconds) = std::env::var("CODEX_METER_APP_SERVER_POLL_SECONDS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or(())
        {
            config.poll_interval = Duration::from_secs(seconds.max(60));
        }
        let _ = spawn_supervisor(database.clone(), config).await;
        eprintln!("App Server account/quota collector enabled");
    }
    if ccusage.run_on_boot() {
        let validation_database = database.clone();
        let validation_collector = ccusage.clone();
        tokio::spawn(async move {
            match validation_collector.run_once(&validation_database).await {
                Ok(summary) => eprintln!(
                    "ccusage validation: {} / {} runs",
                    summary.succeeded, summary.runs
                ),
                Err(error) => eprintln!("ccusage validation unavailable: {error}"),
            }
            if let Err(error) = refresh_rollups(&validation_database).await {
                eprintln!("Rollup refresh after ccusage failed: {error}");
            }
        });
        eprintln!("ccusage validation enabled");
    }

    let bind_address =
        std::env::var("CODEX_METER_BIND").unwrap_or_else(|_| "127.0.0.1:18778".to_owned());
    let listener = tokio::net::TcpListener::bind(&bind_address).await?;
    println!("codex-meter listening on http://{bind_address}");
    axum::serve(
        listener,
        router(AppState {
            database,
            collector,
            ccusage,
        }),
    )
    .await?;
    Ok(())
}

//! The deliberately small implementation path used by the current plan.

pub mod app_server;
pub mod ccusage;
pub mod db;
pub mod jsonl;
pub mod pricing;
pub mod report;
pub mod rollup;
pub mod server;

pub use ccusage::{CcusageCollector, CcusageRunSummary};
pub use db::{
    Database, DbError, EventRecord, QuotaRecord, SourceAppServerRecord, SourceCcusageRecord,
    SourceJsonlRecord,
};
pub use jsonl::{JsonlCollector, JsonlScanReport};
pub use report::build_report;
pub use rollup::{refresh_rollups, RollupError, RollupSummary};

use serde::{Deserialize, Serialize};

/// Runtime configuration is process-owned, not a database table.  Keeping
/// timezone outside SQLite prevents the seven-table schema from growing a
/// settings table just for one display preference.
pub fn configured_timezone() -> String {
    std::env::var("CODEX_METER_TIMEZONE").unwrap_or_else(|_| "Asia/Shanghai".to_owned())
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TokenCounts {
    pub input: i64,
    pub cached: i64,
    pub cache_write: i64,
    pub output: i64,
    pub reasoning: i64,
    pub total: i64,
}

impl TokenCounts {
    pub fn positive(&self) -> bool {
        self.input > 0
            || self.cached > 0
            || self.cache_write > 0
            || self.output > 0
            || self.reasoning > 0
            || self.total > 0
    }

    pub fn saturating_sub(&self, previous: &Self) -> Self {
        Self {
            input: self.input.saturating_sub(previous.input).max(0),
            cached: self.cached.saturating_sub(previous.cached).max(0),
            cache_write: self.cache_write.saturating_sub(previous.cache_write).max(0),
            output: self.output.saturating_sub(previous.output).max(0),
            reasoning: self.reasoning.saturating_sub(previous.reasoning).max(0),
            total: self.total.saturating_sub(previous.total).max(0),
        }
    }

    pub fn regressed_from(&self, previous: &Self) -> bool {
        self.input < previous.input
            || self.cached < previous.cached
            || self.cache_write < previous.cache_write
            || self.output < previous.output
            || self.reasoning < previous.reasoning
            || self.total < previous.total
    }

    pub fn add_assign(&mut self, value: &Self) {
        self.input = self.input.saturating_add(value.input);
        self.cached = self.cached.saturating_add(value.cached);
        self.cache_write = self.cache_write.saturating_add(value.cache_write);
        self.output = self.output.saturating_add(value.output);
        self.reasoning = self.reasoning.saturating_add(value.reasoning);
        self.total = self.total.saturating_add(value.total);
    }
}

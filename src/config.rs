//! Small, versioned runtime configuration.
//!
//! Prices are deliberately shipped as files. Capacity defaults are copied into
//! the runtime database as a baseline; user changes are stored as overrides.

use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("configuration I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("configuration JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CapacityDefaults {
    #[serde(default = "default_usd20_credit")]
    pub usd20: f64,
    #[serde(default = "default_usd100_credit")]
    pub usd100: f64,
    #[serde(default = "default_usd200_credit")]
    pub usd200: f64,
}

impl Default for CapacityDefaults {
    fn default() -> Self {
        Self {
            usd20: default_usd20_credit(),
            usd100: default_usd100_credit(),
            usd200: default_usd200_credit(),
        }
    }
}

fn default_usd20_credit() -> f64 {
    3200.0
}
fn default_usd100_credit() -> f64 {
    16000.0
}
fn default_usd200_credit() -> f64 {
    64000.0
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AppConfig {
    #[serde(default = "default_timezone")]
    pub timezone: String,
    #[serde(default = "default_bind")]
    pub bind: String,
    #[serde(default)]
    pub codex_home: Option<PathBuf>,
    #[serde(default)]
    pub capacity_defaults: CapacityDefaults,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            timezone: default_timezone(),
            bind: default_bind(),
            codex_home: None,
            capacity_defaults: CapacityDefaults::default(),
        }
    }
}

/// A price catalog contains immutable, time-bounded versions instead of one
/// mutable card.  This keeps historical calculations reproducible when a
/// provider changes a model's price.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PriceFile {
    pub scheme: String,
    pub catalog_version: String,
    #[serde(default = "default_timezone")]
    pub timezone: String,
    pub versions: Vec<PriceVersion>,
}

impl PriceFile {
    pub fn version_at(&self, at_ms: i64) -> Option<&PriceVersion> {
        self.versions
            .iter()
            .filter(|version| {
                version.effective_from_ms <= at_ms
                    && version
                        .effective_to_ms
                        .map(|end| at_ms < end)
                        .unwrap_or(true)
            })
            .max_by_key(|version| version.effective_from_ms)
    }

    pub fn latest_version(&self) -> Option<&PriceVersion> {
        self.versions
            .iter()
            .max_by_key(|version| version.effective_from_ms)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PriceVersion {
    pub version: String,
    pub effective_from_ms: i64,
    #[serde(default)]
    pub effective_to_ms: Option<i64>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub source_url: Option<String>,
    #[serde(default)]
    pub source_precision: Option<String>,
    /// Official API pricing uses 272K input tokens as the long-context
    /// threshold.  Subscription credit pricing has no separate long card.
    #[serde(default)]
    pub long_context_threshold_tokens: Option<i64>,
    #[serde(default)]
    pub rates: Vec<ModelRate>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ModelRate {
    pub model: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub input_microusd_per_million: i64,
    pub cache_read_microusd_per_million: i64,
    pub cache_write_microusd_per_million: i64,
    pub output_microusd_per_million: i64,
    /// `None` means the tier is not published/supported for this model.
    #[serde(default)]
    pub fast_multiplier: Option<f64>,
    #[serde(default)]
    pub long_context: Option<LongContextRate>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LongContextRate {
    pub input_microusd_per_million: i64,
    pub cache_read_microusd_per_million: i64,
    pub cache_write_microusd_per_million: i64,
    pub output_microusd_per_million: i64,
}

#[derive(Clone, Debug)]
pub struct ProjectConfig {
    pub app: AppConfig,
    pub api_usd: PriceFile,
    pub subscription_credit: PriceFile,
}

impl ProjectConfig {
    pub fn embedded() -> Result<Self, ConfigError> {
        Ok(Self {
            app: serde_json::from_str(include_str!("../config/app.json")).unwrap_or_default(),
            api_usd: serde_json::from_str(include_str!("../config/api-usd.json"))?,
            subscription_credit: serde_json::from_str(include_str!(
                "../config/subscription-credit.json"
            ))?,
        })
    }

    pub fn from_dir(dir: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let dir = dir.as_ref();
        let app = read_json_or_default(dir.join("app.json"))?;
        Ok(Self {
            app,
            api_usd: read_json(dir.join("api-usd.json"))?,
            subscription_credit: read_json(dir.join("subscription-credit.json"))?,
        })
    }
}

fn read_json<T: for<'de> Deserialize<'de>>(path: PathBuf) -> Result<T, ConfigError> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn read_json_or_default(path: PathBuf) -> Result<AppConfig, ConfigError> {
    match fs::read(path) {
        Ok(bytes) if bytes.iter().all(u8::is_ascii_whitespace) => Ok(AppConfig::default()),
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(AppConfig::default()),
        Err(error) => Err(ConfigError::Io(error)),
    }
}

fn default_timezone() -> String {
    "Asia/Shanghai".to_owned()
}

fn default_bind() -> String {
    "127.0.0.1:18778".to_owned()
}

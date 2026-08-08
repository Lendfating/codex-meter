//! Token accounting and the two price schemes used by the application.
//!
//! Unknown model/price-card inputs remain `None`; an omitted service tier uses
//! the same configured standard/fast fallback as ccusage and carries a quality
//! marker instead of making an otherwise billable event disappear.

use std::{fs, sync::OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config::{ModelRate, PriceFile, PriceVersion, ProjectConfig};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct TokenCounts {
    pub input: i64,
    pub cached: i64,
    pub cache_write: i64,
    pub output: i64,
    pub reasoning: i64,
    pub total: i64,
}

impl TokenCounts {
    pub fn add_assign(&mut self, other: &Self) {
        self.input = self.input.saturating_add(other.input);
        self.cached = self.cached.saturating_add(other.cached);
        self.cache_write = self.cache_write.saturating_add(other.cache_write);
        self.output = self.output.saturating_add(other.output);
        self.reasoning = self.reasoning.saturating_add(other.reasoning);
        self.total = self.total.saturating_add(other.total);
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

    pub fn normalized(mut self) -> Self {
        self.input = self.input.max(0);
        self.cached = self.cached.max(0).min(self.input);
        self.cache_write = self.cache_write.max(0);
        self.output = self.output.max(0);
        self.reasoning = self.reasoning.max(0).min(self.output);
        if self.total <= 0 {
            self.total = self.input.saturating_add(self.output).max(
                self.input
                    .saturating_sub(self.cached)
                    .saturating_add(self.cached)
                    .saturating_add(self.output),
            );
        }
        self
    }

    pub fn observed(&self) -> bool {
        self.input != 0
            || self.cached != 0
            || self.cache_write != 0
            || self.output != 0
            || self.reasoning != 0
            || self.total != 0
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Price {
    pub credit_micros: Option<i64>,
    pub api_usd_micros: Option<i64>,
    pub quality: Vec<String>,
}

pub fn price(tokens: &TokenCounts, model: Option<&str>, tier: Option<&str>, at_ms: i64) -> Price {
    let Ok(config) = ProjectConfig::embedded() else {
        return unknown("pricing_config_unavailable");
    };
    price_with_config(
        tokens,
        model,
        tier,
        at_ms,
        &config.subscription_credit,
        &config.api_usd,
    )
}

pub fn price_with_config(
    tokens: &TokenCounts,
    model: Option<&str>,
    tier: Option<&str>,
    at_ms: i64,
    subscription: &PriceFile,
    api: &PriceFile,
) -> Price {
    let Some(model) = model else {
        return unknown("missing_pricing");
    };
    // Match ccusage's `auto` policy for an event without an explicit tier:
    // use the Codex config speed (standard unless config.toml asks for fast),
    // while retaining a quality marker instead of dropping the billable row.
    let (mut effective_tier, mut quality) = match tier {
        Some("fast") => ("fast", Vec::new()),
        Some("standard") => ("standard", Vec::new()),
        _ => (
            configured_service_tier(),
            vec!["service_tier_unclassified".to_owned()],
        ),
    };
    let Some(subscription_version) = subscription.version_at(at_ms) else {
        return unknown("missing_subscription_version");
    };
    let Some(api_version) = api.version_at(at_ms) else {
        return unknown("missing_api_version");
    };
    let Some(subscription_rate) = find_rate(&subscription_version.rates, model) else {
        return unknown("missing_pricing");
    };
    let Some(api_rate) = find_rate(&api_version.rates, model) else {
        return unknown("missing_pricing");
    };
    // A missing fast multiplier is not a missing price card.  The reference
    // calculator falls back to the standard card for that model.
    if effective_tier == "fast"
        && (subscription_rate.fast_multiplier.is_none() || api_rate.fast_multiplier.is_none())
    {
        effective_tier = "standard";
        quality.push("fast_unsupported_fallback_standard".to_owned());
    }
    let subscription_rates = rate_values(
        subscription_rate,
        tokens,
        subscription_version.long_context_threshold_tokens,
    );
    let api_rates = rate_values(api_rate, tokens, api_version.long_context_threshold_tokens);
    let Some(credit) = apply_multiplier(
        calculate(tokens, subscription_rates),
        subscription_rate.fast_multiplier,
        effective_tier,
    ) else {
        return unknown("pricing_multiplier_invalid");
    };
    let Some(api_usd) = apply_multiplier(
        calculate(tokens, api_rates),
        api_rate.fast_multiplier,
        effective_tier,
    ) else {
        return unknown("pricing_multiplier_invalid");
    };
    Price {
        credit_micros: Some(credit),
        api_usd_micros: Some(api_usd),
        quality,
    }
}

/// Resolve the default speed used by ccusage's Codex `auto` mode.  Explicit
/// event metadata is handled by `price_with_config` before this fallback is
/// consulted.
fn configured_service_tier() -> &'static str {
    static CONFIGURED_TIER: OnceLock<&'static str> = OnceLock::new();
    CONFIGURED_TIER.get_or_init(detect_configured_service_tier)
}

fn detect_configured_service_tier() -> &'static str {
    let mut paths = Vec::new();
    if let Some(home) = std::env::var_os("CODEX_HOME") {
        paths.push(std::path::PathBuf::from(home).join("config.toml"));
    }
    if let Some(home) = std::env::var_os("HOME") {
        paths.push(std::path::PathBuf::from(home).join(".codex/config.toml"));
    }
    for path in paths {
        let Ok(content) = fs::read_to_string(path) else {
            continue;
        };
        if content.lines().any(|line| {
            let setting = line.split('#').next().unwrap_or_default().trim();
            let Some((key, value)) = setting.split_once('=') else {
                return false;
            };
            if key.trim() != "service_tier" {
                return false;
            }
            matches!(value.trim().trim_matches(['"', '\'']), "fast" | "priority")
        }) {
            return "fast";
        }
        return "standard";
    }
    "standard"
}

fn find_rate<'a>(rates: &'a [ModelRate], model: &str) -> Option<&'a ModelRate> {
    let model = model.to_ascii_lowercase();
    rates
        .iter()
        .filter_map(|rate| match_score(rate, &model).map(|score| (score, rate)))
        .max_by_key(|(score, _)| *score)
        .map(|(_, rate)| rate)
}

fn match_score(rate: &ModelRate, model: &str) -> Option<usize> {
    std::iter::once(rate.model.as_str())
        .chain(rate.aliases.iter().map(String::as_str))
        .filter_map(|name| {
            let name = name.to_ascii_lowercase();
            if model == name {
                Some(1_000_000 + name.len())
            } else if model.starts_with(&(name.clone() + "-")) {
                Some(name.len())
            } else {
                None
            }
        })
        .max()
}

fn rate_values(
    rate: &ModelRate,
    tokens: &TokenCounts,
    long_context_threshold_tokens: Option<i64>,
) -> (i64, i64, i64, i64) {
    if let (Some(threshold), Some(long)) =
        (long_context_threshold_tokens, rate.long_context.as_ref())
    {
        if tokens.input > threshold {
            return (
                long.input_microusd_per_million,
                long.cache_read_microusd_per_million,
                long.cache_write_microusd_per_million,
                long.output_microusd_per_million,
            );
        }
    }
    (
        rate.input_microusd_per_million,
        rate.cache_read_microusd_per_million,
        rate.cache_write_microusd_per_million,
        rate.output_microusd_per_million,
    )
}

fn unknown(flag: &str) -> Price {
    Price {
        quality: vec![flag.to_owned()],
        ..Price::default()
    }
}

fn calculate(tokens: &TokenCounts, rates: (i64, i64, i64, i64)) -> i64 {
    let non_cached = tokens.input.saturating_sub(tokens.cached).max(0);
    let numerator = i128::from(non_cached)
        .saturating_mul(i128::from(rates.0))
        .saturating_add(i128::from(tokens.cached).saturating_mul(i128::from(rates.1)))
        .saturating_add(i128::from(tokens.cache_write).saturating_mul(i128::from(rates.2)))
        .saturating_add(i128::from(tokens.output).saturating_mul(i128::from(rates.3)));
    i64::try_from(
        numerator
            .saturating_add(500_000)
            .checked_div(1_000_000)
            .unwrap_or_default(),
    )
    .unwrap_or(i64::MAX)
}

fn apply_multiplier(base: i64, multiplier: Option<f64>, tier: &str) -> Option<i64> {
    if tier != "fast" {
        return Some(base);
    }
    let multiplier = multiplier?;
    if !multiplier.is_finite() || multiplier < 0.0 {
        return None;
    }
    Some(
        (base as f64 * multiplier)
            .round()
            .clamp(0.0, i64::MAX as f64) as i64,
    )
}

pub fn price_as_number(micros: Option<i64>) -> Option<f64> {
    micros.map(|value| value as f64 / 1_000_000.0)
}

pub fn pricing_version() -> String {
    ProjectConfig::embedded()
        .map(|config| {
            let subscription = config
                .subscription_credit
                .latest_version()
                .map(|version| version.version.as_str())
                .unwrap_or("missing");
            let api = config
                .api_usd
                .latest_version()
                .map(|version| version.version.as_str())
                .unwrap_or("missing");
            format!("{}+{}", subscription, api)
        })
        .unwrap_or_else(|_| "pricing-unavailable".to_owned())
}

pub fn capacity_defaults() -> Value {
    ProjectConfig::embedded()
        .map(|config| {
            let defaults = &config.app.capacity_defaults;
            json!({
                "usd20": defaults.usd20,
                "usd100": defaults.usd100,
                "usd200": defaults.usd200
            })
        })
        .unwrap_or_else(|_| json!({"usd20": 3200, "usd100": 16000, "usd200": 64000}))
}

pub fn price_card() -> Vec<Value> {
    let Ok(config) = ProjectConfig::embedded() else {
        return Vec::new();
    };
    let mut boundaries: Vec<i64> = Vec::new();
    for file in [&config.subscription_credit, &config.api_usd] {
        for version in &file.versions {
            boundaries.push(version.effective_from_ms);
            if let Some(to) = version.effective_to_ms {
                boundaries.push(to);
            }
        }
    }
    boundaries.sort_unstable();
    boundaries.dedup();
    let mut groups = Vec::new();
    for (index, from) in boundaries.iter().enumerate() {
        let to = boundaries.get(index + 1).copied();
        let Some(subscription) = config.subscription_credit.version_at(*from) else {
            continue;
        };
        let Some(api) = config.api_usd.version_at(*from) else {
            continue;
        };
        let rows: Vec<Value> = subscription
            .rates
            .iter()
            .map(|rate| {
                let api_rate = find_rate(&api.rates, &rate.model);
                json!({
                    "model": rate.model,
                    "subscription": {
                        "input": rate.input_microusd_per_million,
                        "cache_read": rate.cache_read_microusd_per_million,
                        "cache_write": rate.cache_write_microusd_per_million,
                        "output": rate.output_microusd_per_million,
                        "fast_multiplier": rate.fast_multiplier,
                    },
                    "api": api_rate.map(|api_rate| json!({
                        "input": api_rate.input_microusd_per_million,
                        "cache_read": api_rate.cache_read_microusd_per_million,
                        "cache_write": api_rate.cache_write_microusd_per_million,
                        "output": api_rate.output_microusd_per_million,
                        "fast_multiplier": api_rate.fast_multiplier,
                        "long_context": api_rate.long_context.as_ref().map(|long| json!({
                            "input": long.input_microusd_per_million,
                            "cache_read": long.cache_read_microusd_per_million,
                            "cache_write": long.cache_write_microusd_per_million,
                            "output": long.output_microusd_per_million,
                        })),
                    }))
                })
            })
            .collect();
        groups.push(json!({
            "version": format!("{} + {}", subscription.version, api.version),
            "effective_from_ms": from,
            "effective_to_ms": to,
            "rows": rows,
        }));
    }
    groups
}

/// Build the small config object consumed by ccusage.  Values are dollars per
/// token, while the internal card stores micro-dollars per million tokens.
pub fn ccusage_overrides(scheme: &str) -> Value {
    let Ok(config) = ProjectConfig::embedded() else {
        return Value::Object(Default::default());
    };
    let file = if scheme == "subscription" {
        &config.subscription_credit
    } else {
        &config.api_usd
    };
    file.latest_version()
        .map(ccusage_overrides_for_version)
        .unwrap_or_else(|| Value::Object(Default::default()))
}

/// Build overrides for the price version that applied at an observed event.
/// This is kept separate from the legacy latest-version helper so historical
/// validation can opt into the same time-aware pricing rule as materialization.
pub fn ccusage_overrides_at(scheme: &str, at_ms: i64) -> Value {
    let Ok(config) = ProjectConfig::embedded() else {
        return Value::Object(Default::default());
    };
    let file = if scheme == "subscription" {
        &config.subscription_credit
    } else {
        &config.api_usd
    };
    file.version_at(at_ms)
        .map(ccusage_overrides_for_version)
        .unwrap_or_else(|| Value::Object(Default::default()))
}

fn ccusage_overrides_for_version(version: &PriceVersion) -> Value {
    Value::Object(
        version
            .rates
            .iter()
            .flat_map(|rate| {
                let value = json!({
                    "inputCostPerToken": dollars_per_token(rate.input_microusd_per_million),
                    "cacheReadInputTokenCost": dollars_per_token(rate.cache_read_microusd_per_million),
                    "cacheCreationInputTokenCost": dollars_per_token(rate.cache_write_microusd_per_million),
                    "outputCostPerToken": dollars_per_token(rate.output_microusd_per_million),
                    "fastMultiplier": rate.fast_multiplier
                });
                std::iter::once(rate.model.clone())
                    .chain(rate.aliases.iter().cloned())
                    .map(move |model| (model, value.clone()))
            })
            .collect(),
    )
}

fn dollars_per_token(microusd_per_million: i64) -> f64 {
    microusd_per_million as f64 / 1_000_000_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Historical analysis boundary retained by the original design documents.
    /// Price selection itself uses the provider's published July 30 change date.
    const CARD_BOUNDARY_MS: i64 = 1_785_567_600_000;
    /// 2026-07-30T00:00:00Z.  OpenAI published the change as a date, so the
    /// catalog records the UTC-midnight normalization explicitly in metadata.
    const PRICE_CHANGE_MS: i64 = 1_785_369_600_000;

    #[test]
    fn unknown_values_stay_unknown() {
        let result = price(
            &TokenCounts {
                input: 1_000_000,
                total: 1_000_000,
                ..Default::default()
            },
            Some("not-a-model"),
            Some("standard"),
            CARD_BOUNDARY_MS,
        );
        assert_eq!(result.credit_micros, None);
        assert_eq!(result.api_usd_micros, None);
        assert!(result.quality.iter().any(|flag| flag == "missing_pricing"));
    }

    #[test]
    fn fast_uses_model_multiplier() {
        let tokens = TokenCounts {
            input: 1_000_000,
            total: 1_000_000,
            ..Default::default()
        };
        let standard = price(
            &tokens,
            Some("gpt-5.6-sol"),
            Some("standard"),
            CARD_BOUNDARY_MS,
        );
        let fast = price(&tokens, Some("gpt-5.6-sol"), Some("fast"), CARD_BOUNDARY_MS);
        assert!(fast.credit_micros.unwrap() > standard.credit_micros.unwrap());
    }

    #[test]
    fn ccusage_override_uses_dollars_per_token() {
        let overrides = ccusage_overrides("api");
        let input = overrides["gpt-5.6-sol"]["inputCostPerToken"]
            .as_f64()
            .unwrap();
        assert!((input - 0.000005).abs() < f64::EPSILON);
        assert_eq!(
            overrides["codex-auto-review"]["inputCostPerToken"],
            overrides["gpt-5.5"]["inputCostPerToken"]
        );
    }

    #[test]
    fn catalogs_select_the_pre_and_post_price_versions() {
        let config = ProjectConfig::embedded().unwrap();
        let old_credit = config
            .subscription_credit
            .version_at(PRICE_CHANGE_MS - 1)
            .unwrap();
        let new_credit = config
            .subscription_credit
            .version_at(PRICE_CHANGE_MS)
            .unwrap();
        assert_eq!(old_credit.version, "subscription-credit-before-2026-07-30");
        assert_eq!(new_credit.version, "subscription-credit-2026-07-30");
        assert_eq!(
            old_credit
                .rates
                .iter()
                .find(|rate| rate.model == "gpt-5.6-terra")
                .unwrap()
                .input_microusd_per_million,
            62_500_000
        );
        assert_eq!(
            new_credit
                .rates
                .iter()
                .find(|rate| rate.model == "gpt-5.6-terra")
                .unwrap()
                .input_microusd_per_million,
            50_000_000
        );

        let old_api = config.api_usd.version_at(PRICE_CHANGE_MS - 1).unwrap();
        let new_api = config.api_usd.version_at(PRICE_CHANGE_MS).unwrap();
        assert_eq!(
            old_api
                .rates
                .iter()
                .find(|rate| rate.model == "gpt-5.6-luna")
                .unwrap()
                .input_microusd_per_million,
            1_000_000
        );
        assert_eq!(
            new_api
                .rates
                .iter()
                .find(|rate| rate.model == "gpt-5.6-luna")
                .unwrap()
                .input_microusd_per_million,
            200_000
        );
    }

    #[test]
    fn price_uses_version_and_long_context_rules() {
        let short_context = TokenCounts {
            input: 200_000,
            total: 200_000,
            ..Default::default()
        };
        let old = price(
            &short_context,
            Some("gpt-5.6-terra"),
            Some("standard"),
            PRICE_CHANGE_MS - 1,
        );
        let new = price(
            &short_context,
            Some("gpt-5.6-terra"),
            Some("standard"),
            PRICE_CHANGE_MS,
        );
        assert_eq!(old.credit_micros, Some(12_500_000));
        assert_eq!(old.api_usd_micros, Some(500_000));
        assert_eq!(new.credit_micros, Some(10_000_000));
        assert_eq!(new.api_usd_micros, Some(400_000));

        let long = price(
            &TokenCounts {
                input: 300_000,
                total: 300_000,
                ..Default::default()
            },
            Some("gpt-5.6-sol"),
            Some("standard"),
            CARD_BOUNDARY_MS,
        );
        assert_eq!(long.credit_micros, Some(37_500_000));
        assert_eq!(long.api_usd_micros, Some(3_000_000));
    }

    #[test]
    fn exact_and_alias_models_beat_shorter_prefixes() {
        let mini = price(
            &TokenCounts {
                input: 200_000,
                total: 200_000,
                ..Default::default()
            },
            Some("gpt-5.4-mini"),
            Some("standard"),
            CARD_BOUNDARY_MS,
        );
        assert_eq!(mini.credit_micros, Some(3_750_000));
        assert_eq!(mini.api_usd_micros, Some(150_000));

        let review = price(
            &TokenCounts {
                input: 200_000,
                total: 200_000,
                ..Default::default()
            },
            Some("codex-auto-review"),
            Some("standard"),
            CARD_BOUNDARY_MS,
        );
        assert_eq!(review.credit_micros, Some(25_000_000));
        assert_eq!(review.api_usd_micros, Some(1_000_000));
    }

    #[test]
    fn fast_without_a_published_multiplier_falls_back_to_standard() {
        let tokens = TokenCounts {
            input: 200_000,
            total: 200_000,
            ..Default::default()
        };
        let before_fast = price(
            &tokens,
            Some("gpt-5.6-sol"),
            Some("fast"),
            PRICE_CHANGE_MS - 1,
        );
        assert_eq!(
            before_fast.credit_micros,
            Some(25_000_000),
            "an unsupported fast tier must remain billable at standard price"
        );
        assert_eq!(before_fast.api_usd_micros, Some(1_000_000));
        assert!(before_fast
            .quality
            .iter()
            .any(|flag| flag == "fast_unsupported_fallback_standard"));

        let after_fast = price(&tokens, Some("gpt-5.6-sol"), Some("fast"), PRICE_CHANGE_MS);
        assert_eq!(after_fast.credit_micros, Some(62_500_000));
        assert_eq!(after_fast.api_usd_micros, Some(2_000_000));
    }

    #[test]
    fn unclassified_tier_uses_configured_default_and_keeps_quality_marker() {
        let tokens = TokenCounts {
            input: 200_000,
            total: 200_000,
            ..Default::default()
        };
        let result = price(&tokens, Some("gpt-5.6-sol"), None, PRICE_CHANGE_MS);
        assert!(result.credit_micros.is_some());
        assert!(result.api_usd_micros.is_some());
        assert!(result
            .quality
            .iter()
            .any(|flag| flag == "service_tier_unclassified"));
    }
}

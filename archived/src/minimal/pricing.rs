use serde_json::{json, Value};

use super::TokenCounts;

const CARD_BOUNDARY_MS: i64 = 1_783_567_600_000; // 2026-08-01 00:00 Asia/Shanghai

#[derive(Clone, Copy, Debug)]
struct Rate {
    model: &'static str,
    subscription_input: i64,
    subscription_cached: i64,
    subscription_write: i64,
    subscription_output: i64,
    api_input: i64,
    api_cached: i64,
    api_write: i64,
    api_output: i64,
    subscription_fast_bps: i64,
    api_fast_bps: i64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Price {
    pub credit_micros: Option<i64>,
    pub api_usd_micros: Option<i64>,
    pub quality: Vec<String>,
}

const RATES: &[Rate] = &[
    Rate {
        model: "gpt-5.6-sol",
        subscription_input: 125_000_000,
        subscription_cached: 12_500_000,
        subscription_write: 125_000_000,
        subscription_output: 750_000_000,
        api_input: 5_000_000,
        api_cached: 500_000,
        api_write: 6_250_000,
        api_output: 30_000_000,
        subscription_fast_bps: 25_000,
        api_fast_bps: 20_000,
    },
    Rate {
        model: "gpt-5.6-terra",
        subscription_input: 50_000_000,
        subscription_cached: 5_000_000,
        subscription_write: 50_000_000,
        subscription_output: 300_000_000,
        api_input: 2_500_000,
        api_cached: 250_000,
        api_write: 3_125_000,
        api_output: 15_000_000,
        subscription_fast_bps: 25_000,
        api_fast_bps: 20_000,
    },
    Rate {
        model: "gpt-5.6-luna",
        subscription_input: 5_000_000,
        subscription_cached: 500_000,
        subscription_write: 5_000_000,
        subscription_output: 30_000_000,
        api_input: 1_000_000,
        api_cached: 100_000,
        api_write: 1_250_000,
        api_output: 6_000_000,
        subscription_fast_bps: 25_000,
        api_fast_bps: 20_000,
    },
    Rate {
        model: "gpt-5.5",
        subscription_input: 125_000_000,
        subscription_cached: 12_500_000,
        subscription_write: 125_000_000,
        subscription_output: 750_000_000,
        api_input: 10_000_000,
        api_cached: 1_000_000,
        api_write: 10_000_000,
        api_output: 45_000_000,
        subscription_fast_bps: 25_000,
        api_fast_bps: 25_000,
    },
    Rate {
        model: "gpt-5.4",
        subscription_input: 62_500_000,
        subscription_cached: 6_250_000,
        subscription_write: 62_500_000,
        subscription_output: 375_000_000,
        api_input: 2_500_000,
        api_cached: 250_000,
        api_write: 2_500_000,
        api_output: 15_000_000,
        subscription_fast_bps: 20_000,
        api_fast_bps: 20_000,
    },
    Rate {
        model: "gpt-5.4-mini",
        subscription_input: 18_750_000,
        subscription_cached: 1_875_000,
        subscription_write: 18_750_000,
        subscription_output: 113_000_000,
        api_input: 750_000,
        api_cached: 75_000,
        api_write: 750_000,
        api_output: 4_500_000,
        subscription_fast_bps: 20_000,
        api_fast_bps: 10_000,
    },
];

pub fn price(tokens: &TokenCounts, model: Option<&str>, tier: Option<&str>, at_ms: i64) -> Price {
    let Some(model) = model else {
        return Price {
            quality: vec!["missing_pricing".to_owned()],
            ..Price::default()
        };
    };
    let model_lower = model.to_ascii_lowercase();
    let Some(rate) = RATES.iter().find(|rate| {
        model_lower == rate.model || model_lower.starts_with(&format!("{}-", rate.model))
    }) else {
        return Price {
            quality: vec!["missing_pricing".to_owned()],
            ..Price::default()
        };
    };
    let Some(tier) = tier else {
        return Price {
            quality: vec!["fast_unknown".to_owned()],
            ..Price::default()
        };
    };
    let mut credit_rate = (
        rate.subscription_input,
        rate.subscription_cached,
        rate.subscription_write,
        rate.subscription_output,
    );
    if at_ms < CARD_BOUNDARY_MS && rate.model == "gpt-5.6-terra" {
        credit_rate = (62_500_000, 6_250_000, 62_500_000, 375_000_000);
    }
    if at_ms < CARD_BOUNDARY_MS && rate.model == "gpt-5.6-luna" {
        credit_rate = (25_000_000, 2_500_000, 25_000_000, 150_000_000);
    }
    let credit_base = calculate(tokens, credit_rate);
    let api_base = calculate(
        tokens,
        (
            rate.api_input,
            rate.api_cached,
            rate.api_write,
            rate.api_output,
        ),
    );
    let credit_micros = apply_tier(credit_base, rate.subscription_fast_bps, tier);
    let api_usd_micros = apply_tier(api_base, rate.api_fast_bps, tier);
    Price {
        credit_micros: Some(credit_micros),
        api_usd_micros: Some(api_usd_micros),
        quality: Vec::new(),
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

fn apply_tier(base: i64, fast_bps: i64, tier: &str) -> i64 {
    if tier == "fast" {
        base.saturating_mul(fast_bps).saturating_add(5_000) / 10_000
    } else {
        base
    }
}

pub fn price_as_number(micros: Option<i64>) -> Option<f64> {
    micros.map(|value| value as f64 / 1_000_000.0)
}

pub const PRICING_VERSION: &str = "static-card-2026-08-01";

pub fn price_card() -> Vec<Value> {
    RATES
        .iter()
        .map(|rate| {
            json!({
                "model": rate.model,
                "effective_from_ms": CARD_BOUNDARY_MS,
                "subscription_input": rate.subscription_input as f64 / 1_000_000.0,
                "api_input": rate.api_input as f64 / 1_000_000.0,
                "fast_multiplier": rate.subscription_fast_bps as f64 / 10_000.0
            })
        })
        .collect()
}

/// Build the small `ccusage.json` override object used for an independent
/// check. Values are dollars per token, while the internal card stores
/// micro-dollars per million tokens.
pub fn ccusage_overrides(scheme: &str) -> Value {
    let overrides = RATES
        .iter()
        .map(|rate| {
            let (input, cached, write, output, fast) = if scheme == "subscription" {
                (
                    rate.subscription_input,
                    rate.subscription_cached,
                    rate.subscription_write,
                    rate.subscription_output,
                    rate.subscription_fast_bps,
                )
            } else {
                (
                    rate.api_input,
                    rate.api_cached,
                    rate.api_write,
                    rate.api_output,
                    rate.api_fast_bps,
                )
            };
            (
                rate.model.to_owned(),
                json!({
                    "inputCostPerToken": dollars_per_token(input),
                    "cacheReadInputTokenCost": dollars_per_token(cached),
                    "cacheCreationInputTokenCost": dollars_per_token(write),
                    "outputCostPerToken": dollars_per_token(output),
                    "fastMultiplier": fast as f64 / 10_000.0
                }),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    Value::Object(overrides)
}

fn dollars_per_token(micros_per_million: i64) -> f64 {
    micros_per_million as f64 / 1_000_000_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_model_stays_unknown_instead_of_zero() {
        let result = price(
            &TokenCounts {
                input: 1_000_000,
                total: 1_000_000,
                ..TokenCounts::default()
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
            ..TokenCounts::default()
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
    fn ccusage_override_is_dollars_per_token() {
        let overrides = ccusage_overrides("api");
        let input = overrides["gpt-5.6-sol"]["inputCostPerToken"]
            .as_f64()
            .unwrap();
        assert!((input - 0.000005).abs() < f64::EPSILON);
    }
}

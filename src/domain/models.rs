use serde::{Deserialize, Serialize};
use serde_json::Value;

pub type EpochMillis = i64;

macro_rules! string_enum {
    ($(#[$meta:meta])* $name:ident { $($(#[$variant_meta:meta])* $variant:ident => $value:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name {
            $($(#[$variant_meta])* $variant),+
        }

        impl $name {
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $value),+
                }
            }
        }
    };
}

string_enum! {
    /// Authentication mechanism observed for an account context.
    AuthKind {
        Chatgpt => "chatgpt",
        OfficialApi => "official_api",
        CustomApi => "custom_api",
        Bedrock => "bedrock",
        Unknown => "unknown"
    }
}

string_enum! {
    /// User-facing grouping. This is intentionally independent from auth kind.
    DisplayGroup {
        Plus => "plus",
        Pro => "pro",
        OtherApi => "other_api",
        Other => "other",
        Unknown => "unknown"
    }
}

string_enum! {
    ClassificationSource {
        Observed => "observed",
        Inferred => "inferred",
        Manual => "manual",
        Unknown => "unknown"
    }
}

string_enum! {
    Quality {
        Exact => "exact",
        Estimated => "estimated",
        MixedAccount => "mixed_account",
        UnknownProvider => "unknown_provider",
        FastUnknown => "fast_unknown",
        MissingSamples => "missing_samples",
        BoundaryApproximate => "boundary_approximate"
    }
}

string_enum! {
    PricingScheme {
        SubscriptionCredit => "subscription_credit",
        ApiUsdEquivalent => "api_usd_equivalent"
    }
}

string_enum! {
    CapacityProfile {
        Usd20 => "usd20",
        Usd100 => "usd100",
        Usd200 => "usd200"
    }
}

string_enum! {
    CapacityStatus {
        Draft => "draft",
        Confirmed => "confirmed",
        Retired => "retired"
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourceMetadata {
    pub source: String,
    pub quality: Quality,
    pub pricing_version: Option<String>,
    pub collector_version: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Machine {
    pub name: String,
    pub install_id: String,
    pub timezone: String,
    pub created_at_ms: EpochMillis,
}

impl SourceMetadata {
    pub fn new(
        source: impl Into<String>,
        quality: Quality,
        pricing_version: Option<String>,
        collector_version: impl Into<String>,
    ) -> Self {
        Self {
            source: source.into(),
            quality,
            pricing_version,
            collector_version: collector_version.into(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TokenCounts {
    pub input_tokens: i64,
    pub cached_input_tokens: i64,
    pub cache_write_input_tokens: i64,
    pub output_tokens: i64,
    pub reasoning_output_tokens: i64,
    pub total_tokens: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AccountIdentity {
    pub machine_id: i64,
    pub kind: AuthKind,
    pub email_masked: Option<String>,
    pub identity_hmac: String,
    pub label: Option<String>,
    pub created_at_ms: EpochMillis,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AccountContextInterval {
    pub machine_id: i64,
    pub account_identity_id: i64,
    pub start_at_ms: EpochMillis,
    pub end_at_ms: Option<EpochMillis>,
    pub auth_kind: AuthKind,
    pub plan_type_raw: Option<String>,
    pub display_group: DisplayGroup,
    pub capacity_profile: Option<CapacityProfile>,
    pub provider_name: Option<String>,
    pub endpoint_hmac: Option<String>,
    pub classification_source: ClassificationSource,
    pub metadata: SourceMetadata,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TokenObservation {
    pub machine_id: i64,
    pub context_interval_id: Option<i64>,
    pub session_id: String,
    pub turn_id: Option<String>,
    pub observed_at_ms: EpochMillis,
    pub tokens: TokenCounts,
    pub model: Option<String>,
    pub service_tier: Option<String>,
    pub source_digest: String,
    pub collector_version: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct QuotaSnapshot {
    pub machine_id: i64,
    pub account_identity_id: i64,
    pub observed_at_ms: EpochMillis,
    pub limit_id: String,
    pub limit_name: Option<String>,
    pub used_percent: Option<f64>,
    pub window_minutes: Option<i64>,
    pub resets_at_ms: Option<EpochMillis>,
    pub plan_type_raw: Option<String>,
    pub credits_has_credits: Option<bool>,
    pub credits_unlimited: Option<bool>,
    pub credits_balance: Option<String>,
    pub metadata: SourceMetadata,
    pub source_digest: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AccountUsageSnapshot {
    pub machine_id: i64,
    pub account_identity_id: i64,
    pub observed_at_ms: EpochMillis,
    pub lifetime_tokens: Option<i64>,
    pub daily_buckets: Value,
    pub metadata: SourceMetadata,
    pub source_digest: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CcusageSessionSnapshot {
    pub machine_id: i64,
    pub session_id: String,
    pub observed_at_ms: EpochMillis,
    pub model_tokens: Value,
    pub pricing_scheme: PricingScheme,
    pub auto_amount: f64,
    pub standard_amount: f64,
    pub pricing_version: String,
    pub ccusage_version: String,
    pub command_duration_ms: i64,
    pub result_hash: String,
    pub source_digest: String,
    pub source: String,
    pub quality: Quality,
    pub collector_version: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct UsageDelta {
    pub machine_id: i64,
    pub account_context_interval_id: Option<i64>,
    pub session_id: Option<String>,
    pub start_at_ms: EpochMillis,
    pub end_at_ms: EpochMillis,
    pub tokens: TokenCounts,
    pub subscription_base_credit: Option<f64>,
    pub subscription_fast_surcharge: Option<f64>,
    pub subscription_total_credit: Option<f64>,
    pub api_base_usd: Option<f64>,
    pub api_fast_surcharge_usd: Option<f64>,
    pub api_total_usd: Option<f64>,
    pub dedupe_key: String,
    pub metadata: SourceMetadata,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct DailyRollup {
    pub machine_id: i64,
    pub local_date: String,
    pub account_identity_id: Option<i64>,
    pub tokens: TokenCounts,
    pub subscription_credit: Option<f64>,
    pub api_usd: Option<f64>,
    pub metadata: SourceMetadata,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct PricingVersion {
    pub id: String,
    pub scheme: PricingScheme,
    pub effective_at_ms: EpochMillis,
    pub timezone: String,
    pub rates: Value,
    pub fast_multipliers: Value,
    pub source_url: Option<String>,
    pub source_precision: String,
    pub created_at_ms: EpochMillis,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct PlanCapacity {
    pub id: String,
    pub machine_id: i64,
    pub plan_code: CapacityProfile,
    pub effective_from_ms: EpochMillis,
    pub effective_to_ms: Option<EpochMillis>,
    pub confirmed_credit: Option<f64>,
    pub status: CapacityStatus,
    pub note: Option<String>,
    pub created_at_ms: EpochMillis,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CalibrationSegment {
    pub machine_id: i64,
    pub account_identity_id: i64,
    pub start_at_ms: EpochMillis,
    pub end_at_ms: EpochMillis,
    pub window_kind: String,
    pub used_percent_start: Option<f64>,
    pub used_percent_end: Option<f64>,
    pub local_credit: f64,
    pub candidate_capacity: Option<f64>,
    pub sample_count: i64,
    pub contamination: Option<String>,
    pub adopted: bool,
    pub metadata: SourceMetadata,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ManualAnnotation {
    pub machine_id: i64,
    pub target_type: String,
    pub target_id: String,
    pub before_json: Value,
    pub after_json: Value,
    pub reason: String,
    pub created_at_ms: EpochMillis,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CollectorRun {
    pub machine_id: i64,
    pub source: String,
    pub started_at_ms: EpochMillis,
    pub duration_ms: Option<i64>,
    pub status: String,
    pub stderr_summary: Option<String>,
    pub collector_version: String,
}

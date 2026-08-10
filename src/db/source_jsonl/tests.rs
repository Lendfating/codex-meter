use sqlx::{sqlite::SqliteRow, Row};

use super::{normalize_records, SourceJsonlBatchReport};
use crate::db::{Database, SourceJsonlRecord};

fn rich_record(key: &str, suffix: &str, observed_at_ms: i64, kind: &str) -> SourceJsonlRecord {
    SourceJsonlRecord {
        source_key: key.to_owned(),
        kind: kind.to_owned(),
        observed_at_ms,
        last_seen_at_ms: Some(observed_at_ms + 1),
        session_id: Some(format!("session-{suffix}")),
        parent_session_id: Some(format!("parent-{suffix}")),
        root_session_id: Some(format!("root-{suffix}")),
        turn_id: Some(format!("turn-{suffix}")),
        relation: Some("child".to_owned()),
        title: Some(format!("title-{suffix}")),
        started_at_ms: Some(observed_at_ms + 2),
        ended_at_ms: Some(observed_at_ms + 3),
        model: Some(format!("model-{suffix}")),
        service_tier: Some(format!("tier-{suffix}")),
        reasoning_effort: Some(format!("effort-{suffix}")),
        provider: Some(format!("provider-{suffix}")),
        plan_type: Some(format!("plan-{suffix}")),
        input_tokens: Some(observed_at_ms + 4),
        cache_read_tokens: Some(observed_at_ms + 5),
        cache_write_tokens: Some(observed_at_ms + 6),
        output_tokens: Some(observed_at_ms + 7),
        reasoning_tokens: Some(observed_at_ms + 8),
        total_tokens: Some(observed_at_ms + 9),
        limit_id: Some(format!("limit-{suffix}")),
        window_kind: Some(format!("window-{suffix}")),
        used_percent: Some(observed_at_ms as f64 + 10.0),
        window_minutes: Some(observed_at_ms + 11),
        resets_at_ms: Some(observed_at_ms + 12),
        quality: Some(format!("quality-{suffix}")),
    }
}

fn partial_update(key: &str) -> SourceJsonlRecord {
    SourceJsonlRecord {
        source_key: key.to_owned(),
        kind: "turn".to_owned(),
        observed_at_ms: 999,
        session_id: Some("ignored-session".to_owned()),
        title: Some("title-third".to_owned()),
        total_tokens: Some(3_333),
        quality: Some("quality-third".to_owned()),
        ..Default::default()
    }
}

async fn apply_sequential(database: &Database, records: &[SourceJsonlRecord]) {
    for record in records {
        database.upsert_source_jsonl(record).await.unwrap();
    }
}

fn assert_rows_equal(left: &[SqliteRow], right: &[SqliteRow]) {
    assert_eq!(left.len(), right.len());
    macro_rules! compare {
        ($left:expr, $right:expr, $ty:ty, $column:literal) => {
            assert_eq!(
                $left.try_get::<$ty, _>($column).unwrap(),
                $right.try_get::<$ty, _>($column).unwrap(),
                "column {} differs",
                $column
            );
        };
    }

    for (left, right) in left.iter().zip(right) {
        compare!(left, right, String, "source_key");
        compare!(left, right, String, "kind");
        compare!(left, right, i64, "observed_at_ms");
        compare!(left, right, Option<i64>, "last_seen_at_ms");
        compare!(left, right, Option<String>, "session_id");
        compare!(left, right, Option<String>, "parent_session_id");
        compare!(left, right, Option<String>, "root_session_id");
        compare!(left, right, Option<String>, "turn_id");
        compare!(left, right, Option<String>, "relation");
        compare!(left, right, Option<String>, "title");
        compare!(left, right, Option<i64>, "started_at_ms");
        compare!(left, right, Option<i64>, "ended_at_ms");
        compare!(left, right, Option<String>, "model");
        compare!(left, right, Option<String>, "service_tier");
        compare!(left, right, Option<String>, "reasoning_effort");
        compare!(left, right, Option<String>, "provider");
        compare!(left, right, Option<String>, "plan_type");
        compare!(left, right, Option<i64>, "input_tokens");
        compare!(left, right, Option<i64>, "cache_read_tokens");
        compare!(left, right, Option<i64>, "cache_write_tokens");
        compare!(left, right, Option<i64>, "output_tokens");
        compare!(left, right, Option<i64>, "reasoning_tokens");
        compare!(left, right, Option<i64>, "total_tokens");
        compare!(left, right, Option<String>, "limit_id");
        compare!(left, right, Option<String>, "window_kind");
        compare!(left, right, Option<f64>, "used_percent");
        compare!(left, right, Option<i64>, "window_minutes");
        compare!(left, right, Option<i64>, "resets_at_ms");
        compare!(left, right, Option<String>, "quality");
    }
}

#[test]
fn normalization_preserves_first_stable_fields_and_last_non_null_updates() {
    let first = rich_record("usage:one", "first", 10, "usage");
    let second = rich_record("usage:one", "second", 20, "quota");
    let third = partial_update("usage:one");

    let normalized = normalize_records(vec![first.clone(), second.clone(), third]);

    assert_eq!(normalized.input_records, 3);
    assert_eq!(normalized.merged_duplicates, 2);
    assert_eq!(normalized.records.len(), 1);
    let record = &normalized.records[0];
    assert_eq!(record.kind, first.kind);
    assert_eq!(record.observed_at_ms, first.observed_at_ms);
    assert_eq!(record.session_id, first.session_id);
    assert_eq!(record.last_seen_at_ms, second.last_seen_at_ms);
    assert_eq!(record.parent_session_id, second.parent_session_id);
    assert_eq!(record.title.as_deref(), Some("title-third"));
    assert_eq!(record.total_tokens, Some(3_333));
    assert_eq!(record.quality.as_deref(), Some("quality-third"));
}

#[test]
fn quota_normalization_keeps_earliest_observation_and_latest_confirmation() {
    let late = rich_record("quota:stable", "late", 2_000, "quota");
    let early = rich_record("quota:stable", "early", 1_000, "quota");

    let normalized = normalize_records(vec![late, early]);
    let record = &normalized.records[0];

    assert_eq!(record.observed_at_ms, 1_000);
    assert_eq!(record.last_seen_at_ms, Some(2_001));
}

#[tokio::test]
async fn batch_matches_sequential_upsert_for_every_source_column() {
    let sequential = Database::connect_in_memory().await.unwrap();
    let batch = Database::connect_in_memory().await.unwrap();
    let records = vec![
        rich_record("usage:one", "first", 10, "usage"),
        rich_record("usage:one", "second", 20, "quota"),
        partial_update("usage:one"),
        rich_record("quota:two", "quota", 30, "quota"),
    ];

    apply_sequential(&sequential, &records).await;
    let report = batch.upsert_source_jsonl_batch(records).await.unwrap();

    assert_eq!(
        report,
        SourceJsonlBatchReport {
            inserted_events: 2,
            duplicate_events: 2,
            inserted_quota_samples: 1,
        }
    );
    assert_rows_equal(
        &sequential.list_source_jsonl().await.unwrap(),
        &batch.list_source_jsonl().await.unwrap(),
    );
}

#[tokio::test]
async fn batch_matches_sequential_upsert_when_keys_already_exist() {
    let sequential = Database::connect_in_memory().await.unwrap();
    let batch = Database::connect_in_memory().await.unwrap();
    let existing = rich_record("usage:existing", "existing", 1, "usage");
    sequential.upsert_source_jsonl(&existing).await.unwrap();
    batch.upsert_source_jsonl(&existing).await.unwrap();
    let records = vec![
        rich_record("usage:existing", "second", 2, "quota"),
        partial_update("usage:existing"),
        rich_record("quota:new", "new", 3, "quota"),
    ];

    apply_sequential(&sequential, &records).await;
    let report = batch.upsert_source_jsonl_batch(records).await.unwrap();

    assert_eq!(report.inserted_events, 1);
    assert_eq!(report.duplicate_events, 2);
    assert_eq!(report.inserted_quota_samples, 1);
    assert_rows_equal(
        &sequential.list_source_jsonl().await.unwrap(),
        &batch.list_source_jsonl().await.unwrap(),
    );
}

#[tokio::test]
async fn batch_handles_empty_and_more_than_one_lookup_chunk() {
    let database = Database::connect_in_memory().await.unwrap();
    assert_eq!(
        database
            .upsert_source_jsonl_batch(Vec::new())
            .await
            .unwrap(),
        SourceJsonlBatchReport::default()
    );

    let records = (0..801)
        .map(|index| {
            rich_record(
                &format!("usage:{index}"),
                &index.to_string(),
                index,
                "usage",
            )
        })
        .collect::<Vec<_>>();
    let first = database
        .upsert_source_jsonl_batch(records.clone())
        .await
        .unwrap();
    let second = database.upsert_source_jsonl_batch(records).await.unwrap();

    assert_eq!(first.inserted_events, 801);
    assert_eq!(first.duplicate_events, 0);
    assert_eq!(second.inserted_events, 0);
    assert_eq!(second.duplicate_events, 801);
}

#[tokio::test]
async fn quota_upsert_repairs_a_late_replay_observation() {
    let database = Database::connect_in_memory().await.unwrap();
    let mut replay = rich_record("quota:stable", "replay", 2_000, "quota");
    replay.last_seen_at_ms = Some(900);
    database
        .upsert_source_jsonl_batch(vec![replay])
        .await
        .unwrap();
    assert!(database.has_inverted_jsonl_quota_times().await.unwrap());

    let original = rich_record("quota:stable", "original", 1_000, "quota");
    database
        .upsert_source_jsonl_batch(vec![original])
        .await
        .unwrap();

    let row = database.list_source_jsonl().await.unwrap().pop().unwrap();
    assert_eq!(row.try_get::<i64, _>("observed_at_ms").unwrap(), 1_000);
    assert_eq!(
        row.try_get::<Option<i64>, _>("last_seen_at_ms").unwrap(),
        Some(1_001)
    );
    assert!(!database.has_inverted_jsonl_quota_times().await.unwrap());
}

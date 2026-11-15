use super::*;
use serde_json::{json, Value};

fn database() -> Database {
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    connection.execute_batch(
        "CREATE TABLE site_accounts (
            site_id TEXT, profile_id TEXT, profile_name TEXT DEFAULT '', account_name TEXT DEFAULT '',
            username TEXT DEFAULT '', is_valid INTEGER DEFAULT 0, sync_error TEXT DEFAULT '',
            api_key_count INTEGER DEFAULT 0, api_model_count INTEGER DEFAULT 0,
            PRIMARY KEY(site_id, profile_id));
         CREATE TABLE site_model_cache (
            site_id TEXT, profile_id TEXT, profile_name TEXT DEFAULT '', account_name TEXT DEFAULT '',
            username TEXT DEFAULT '', api_source TEXT DEFAULT '', keys_json TEXT DEFAULT '[]',
            groups_json TEXT DEFAULT '{}', models_json TEXT DEFAULT '[]', key_models_json TEXT DEFAULT '{}',
            health_json TEXT DEFAULT '{}', error TEXT DEFAULT '', updated_at TEXT DEFAULT '',
            PRIMARY KEY(site_id, profile_id));
         INSERT INTO site_accounts (site_id, profile_id, sync_error, api_key_count, api_model_count)
            VALUES ('site', 'A', 'credential expired', 2, 2);"
    ).unwrap();
    Database(std::sync::Mutex::new(connection))
}
fn account(profile: &str) -> SiteModelCacheAccount {
    serde_json::from_value(
        json!({"profileId":profile,"profileName":profile,"accountName":"test",
        "username":"test", "keys":[],"error":""}),
    )
    .unwrap()
}
fn initial(profile: &str) -> SiteModelsResult {
    serde_json::from_value(json!({
        "models":[{"id":"model-one"},{"id":"model-two"}], "source":"newapi-key", "keys":["one","two"],
        "keyGroups":{"one":"group","two":"group"},
        "keyModels":{"one":[{"id":"model-one"}], "two":[{"id":"model-two"}]},
        "profileId":profile, "errors":[], "modelHealth":{"model-one":{"successRate":0.9}}
    })).unwrap()
}
fn row(db: &Database, profile: &str) -> Value {
    db.0.lock().unwrap().query_row(
        "SELECT keys_json, groups_json, models_json, key_models_json, health_json, error, api_source
         FROM site_model_cache WHERE site_id='site' AND profile_id=?1", [profile], |row| {
            let parse = |index| -> rusqlite::Result<Value> {
                let text: String = row.get(index)?;
                Ok(serde_json::from_str(&text).unwrap())
            };
            Ok(json!({"keys":parse(0)?,"groups":parse(1)?,"models":parse(2)?,"keyModels":parse(3)?,
                "health":parse(4)?,"error":row.get::<_, String>(5)?,"source":row.get::<_, String>(6)?}))
        }
    ).unwrap()
}

#[test]
fn site_cache_failed_account_keeps_its_data_and_other_accounts() {
    let db = database();
    for profile in ["A", "B"] {
        save_site_model_cache(
            &db,
            "site",
            &account(profile),
            Some(&initial(profile)),
            false,
        )
        .unwrap();
    }
    let before = row(&db, "A");
    let other = row(&db, "B");
    let mut failure = initial("A");
    failure.keys.clear();
    failure.models.clear();
    failure.key_groups.clear();
    failure.key_models.clear();
    failure.model_health.clear();
    failure.errors = vec!["HTTP 503".into()];
    // A legacy caller omitting preserveKeys must be protected as well.
    save_site_model_cache(&db, "site", &account("A"), Some(&failure), false).unwrap();
    let after = row(&db, "A");
    for field in ["keys", "groups", "models", "keyModels", "health", "source"] {
        assert_eq!(
            after[field], before[field],
            "{field} must survive a failed sync"
        );
    }
    assert_eq!(after["error"], "HTTP 503");
    assert_eq!(row(&db, "B"), other);
}

#[test]
fn site_cache_read_error_cannot_overwrite_the_existing_row() {
    let db = database();
    save_site_model_cache(&db, "site", &account("A"), Some(&initial("A")), false).unwrap();
    db.0.lock()
        .unwrap()
        .execute(
            "UPDATE site_model_cache SET models_json=CAST('corrupt' AS BLOB) WHERE profile_id='A'",
            [],
        )
        .unwrap();
    let mut fresh = initial("A");
    fresh.keys = vec!["replacement".into()];
    let error = save_site_model_cache(&db, "site", &account("A"), Some(&fresh), false).unwrap_err();
    assert!(error.contains("读取站点模型缓存失败"));
    let (keys, storage_type): (String, String) =
        db.0.lock()
            .unwrap()
            .query_row(
                "SELECT keys_json, typeof(models_json) FROM site_model_cache WHERE profile_id='A'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&keys).unwrap(),
        json!(["one", "two"])
    );
    assert_eq!(storage_type, "blob");
}

#[test]
fn site_cache_old_schema_without_health_still_preserves_core_mappings() {
    let db = database();
    save_site_model_cache(&db, "site", &account("A"), Some(&initial("A")), false).unwrap();
    db.0.lock()
        .unwrap()
        .execute("ALTER TABLE site_model_cache DROP COLUMN health_json", [])
        .unwrap();
    let mut keys_only = initial("A");
    keys_only.keys = vec!["one".into()];
    keys_only.models.clear();
    keys_only.key_models.clear();
    keys_only.model_health.clear();
    save_site_model_cache(&db, "site", &account("A"), Some(&keys_only), false).unwrap();
    let (models, mappings): (String, String) =
        db.0.lock()
            .unwrap()
            .query_row(
                "SELECT models_json, key_models_json FROM site_model_cache WHERE profile_id='A'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&models).unwrap(),
        json!([{"id":"model-one"}])
    );
    assert_eq!(
        serde_json::from_str::<Value>(&mappings).unwrap(),
        json!({"one":[{"id":"model-one"}]})
    );
}

#[test]
fn site_cache_transport_failure_does_not_disappear_behind_retained_keys() {
    let db = database();
    save_site_model_cache(&db, "site", &account("A"), Some(&initial("A")), false).unwrap();
    let mut failed = account("A");
    failed.error = "connection reset".into();
    save_site_model_cache(&db, "site", &failed, None, false).unwrap();
    let cached = row(&db, "A");
    assert_eq!(cached["keys"], json!(["one", "two"]));
    assert_eq!(cached["error"], "connection reset");
}

#[test]
fn site_cache_key_refresh_prunes_deleted_key_but_retains_missing_model_mapping_and_health() {
    let db = database();
    save_site_model_cache(&db, "site", &account("A"), Some(&initial("A")), false).unwrap();
    let mut keys_only = initial("A");
    keys_only.keys = vec!["one".into()];
    keys_only.models.clear();
    keys_only.key_models.clear();
    keys_only.key_groups.retain(|key, _| key == "one");
    keys_only.model_health.clear();
    save_site_model_cache(&db, "site", &account("A"), Some(&keys_only), false).unwrap();
    let cached = row(&db, "A");
    assert_eq!(cached["keys"], json!(["one"]));
    assert_eq!(cached["keyModels"], json!({"one":[{"id":"model-one"}]}));
    assert_eq!(cached["models"], json!([{"id":"model-one"}]));
    assert_eq!(cached["groups"], json!({"one":"group"}));
    assert!(!cached["health"].as_object().unwrap().is_empty());
}

#[test]
fn site_cache_partial_mapping_success_merges_fresh_and_previous_results() {
    let db = database();
    save_site_model_cache(&db, "site", &account("A"), Some(&initial("A")), false).unwrap();
    let mut partial = initial("A");
    partial.models = vec![SiteModelItem {
        id: "fresh".into(),
        owned_by: None,
    }];
    partial.key_models = HashMap::from([("one".into(), partial.models.clone())]);
    partial.errors = vec!["second key failed".into()];
    save_site_model_cache(&db, "site", &account("A"), Some(&partial), false).unwrap();
    let cached = row(&db, "A");
    assert_eq!(cached["keyModels"]["two"], json!([{"id":"model-two"}]));
    assert_eq!(cached["models"], json!([{"id":"fresh"},{"id":"model-two"}]));
    assert_eq!(cached["error"], "second key failed");
}

#[test]
fn site_cache_preserved_failure_does_not_mark_invalid_account_recovered() {
    let db = database();
    save_site_model_cache(&db, "site", &account("A"), Some(&initial("A")), false).unwrap();
    db.0.lock()
        .unwrap()
        .execute(
            "UPDATE site_accounts SET is_valid=0,sync_error='expired' WHERE profile_id='A'",
            [],
        )
        .unwrap();
    let mut failure = initial("A");
    failure.keys.clear();
    failure.models.clear();
    failure.key_models.clear();
    failure.errors = vec!["HTTP 401".into()];
    save_site_model_cache(&db, "site", &account("A"), Some(&failure), true).unwrap();
    let (valid, error): (i64, String) =
        db.0.lock()
            .unwrap()
            .query_row(
                "SELECT is_valid,sync_error FROM site_accounts WHERE profile_id='A'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
    assert_eq!((valid, error), (0, "expired".into()));
}

#[test]
fn site_cache_authoritative_empty_mapping_is_not_restored_from_old_models() {
    let db = database();
    save_site_model_cache(&db, "site", &account("A"), Some(&initial("A")), false).unwrap();
    let mut empty = initial("A");
    empty.keys = vec!["one".into()];
    empty.models.clear();
    empty.key_models = HashMap::from([("one".into(), vec![])]);
    save_site_model_cache(&db, "site", &account("A"), Some(&empty), false).unwrap();
    let cached = row(&db, "A");
    assert_eq!(cached["models"], json!([]));
    assert_eq!(cached["keyModels"], json!({"one":[]}));
}

#[test]
fn site_cache_explicit_account_key_removal_remains_possible() {
    let db = database();
    save_site_model_cache(&db, "site", &account("A"), Some(&initial("A")), false).unwrap();
    save_site_model_cache(&db, "site", &account("B"), Some(&initial("B")), false).unwrap();
    save_site_model_cache(&db, "site", &account("A"), None, false).unwrap();
    let cached = row(&db, "A");
    assert_eq!(cached["keys"], json!([]));
    assert_eq!(cached["models"], json!([]));
    assert_eq!(row(&db, "B")["keys"], json!(["one", "two"]));
}

#[test]
fn site_cache_failed_discovery_does_not_zero_out_account_counts() {
    let db = database();
    let mut failure = initial("A");
    failure.keys.clear();
    failure.models.clear();
    failure.errors = vec!["request timed out".into()];
    cache_profile_api_counts(&db, Some("site"), Some("A"), failure).unwrap();
    let counts: (i64, i64) =
        db.0.lock()
            .unwrap()
            .query_row(
                "SELECT api_key_count,api_model_count FROM site_accounts WHERE profile_id='A'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
    assert_eq!(counts, (2, 2));
}

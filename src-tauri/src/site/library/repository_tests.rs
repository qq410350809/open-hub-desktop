use super::*;
use crate::db::insert_site_transaction;
use crate::models::Database;
use serde_json::json;
use std::sync::Arc;

fn database() -> Database {
    let database = Database::open(std::path::Path::new(":memory:")).unwrap();
    database
        .lock_conn()
        .unwrap()
        .execute("DELETE FROM directory_sites", [])
        .unwrap();
    database
}

fn site(id: &str) -> SiteRecord {
    SiteRecord {
        id: id.into(),
        name: format!("Site {id}"),
        description: "description".into(),
        registration_limit: 2,
        icon: "icon".into(),
        api_base_url: format!("https://{id}.example/api"),
        aux_base_url: "https://backup.example/v1".into(),
        system_type: "new-api".into(),
        supports_immersive_translation: true,
        supports_ldc: true,
        supports_checkin: true,
        supports_nsfw: true,
        checkin_url: "https://checkin.example/".into(),
        checkin_note: "checkin-note".into(),
        benefit_url: "https://benefit.example/".into(),
        rate_limit: "1 rps".into(),
        status_url: "https://status.example/".into(),
        is_only_maintainer_visible: true,
        requires_invite_code: true,
        is_fake_charity: true,
        has_pending_report: true,
        use_proxy_pool: true,
        favorite: true,
        hidden: true,
        tags: vec![format!("{id}-tag-z"), format!("{id}-tag-a")],
        maintainers: vec![Maintainer {
            name: format!("{id} maintainer"),
            id: format!("{id}-owner"),
            username: "username".into(),
            profile_url: "https://owner.example/".into(),
        }],
        extension_links: vec![ExtensionLink {
            label: format!("{id} docs"),
            url: "https://docs.example/".into(),
        }],
        ..SiteRecord::default()
    }
}

fn insert(connection: &mut Connection, site: &SiteRecord) {
    let transaction = connection.transaction().unwrap();
    insert_site_transaction(&transaction, site).unwrap();
    transaction.commit().unwrap();
}

#[test]
fn list_and_single_site_read_share_all_fields_and_relations() {
    let database = database();
    let mut connection = database.lock_conn().unwrap();
    for id in ["a", "b"] {
        insert(&mut connection, &site(id));
    }
    connection.execute("UPDATE directory_sites SET use_system_proxy=1,is_pending=1,is_runaway=1,updated_at='2026-11-01 12:00:00' WHERE id='a'", []).unwrap();
    let listed = read_sites(&connection).unwrap();
    assert_eq!(listed.len(), 2);
    for item in listed {
        let single = read_site(&connection, &item.id).unwrap().unwrap();
        assert_eq!(
            serde_json::to_value(&single).unwrap(),
            serde_json::to_value(&item).unwrap()
        );
        assert_eq!(single.use_system_proxy, item.use_system_proxy);
        let expected = site(&item.id);
        assert_eq!(single.tags, expected.tags);
        assert_eq!(single.maintainers[0].id, expected.maintainers[0].id);
        assert_eq!(
            single.extension_links[0].label,
            expected.extension_links[0].label
        );
        assert_eq!(single.aux_base_url, expected.aux_base_url);
        if item.id == "a" {
            assert!(single.is_pending && single.is_runaway && single.use_system_proxy);
        }
    }
}

#[test]
fn list_preserves_usage_date_and_insertion_order_priorities() {
    let database = database();
    let mut connection = database.lock_conn().unwrap();
    for id in ["personal", "pending", "old", "new-first", "new-last"] {
        insert(&mut connection, &site(id));
    }
    connection
        .execute_batch(
            "UPDATE directory_sites SET updated_at='2026-11-01 12:00:00';
         UPDATE directory_sites SET is_personal=1,updated_at='2026-01-01' WHERE id='personal';
         UPDATE directory_sites SET is_pending=1,updated_at='2026-02-01' WHERE id='pending';
         UPDATE directory_sites SET updated_at='2026-03-01' WHERE id='old';",
        )
        .unwrap();
    let ids: Vec<_> = read_sites(&connection)
        .unwrap()
        .into_iter()
        .map(|site| site.id)
        .collect();
    assert_eq!(ids, ["personal", "pending", "new-last", "new-first", "old"]);
}

#[test]
fn missing_site_is_none_but_bad_row_and_schema_are_errors() {
    let database = database();
    let mut connection = database.lock_conn().unwrap();
    assert!(read_site(&connection, "missing").unwrap().is_none());
    insert(&mut connection, &site("a"));
    connection
        .execute(
            "UPDATE directory_sites SET name=CAST('bad' AS BLOB) WHERE id='a'",
            [],
        )
        .unwrap();
    assert!(read_site(&connection, "a")
        .unwrap_err()
        .contains("读取站点失败"));
    assert!(read_sites(&connection)
        .unwrap_err()
        .contains("读取站点列表失败"));
    connection
        .execute(
            "UPDATE directory_sites SET name='restored' WHERE id='a'",
            [],
        )
        .unwrap();
    connection
        .execute("DROP TABLE site_extensions", [])
        .unwrap();
    assert!(read_site(&connection, "a")
        .unwrap_err()
        .contains("site_extensions"));
    assert!(read_sites(&connection)
        .unwrap_err()
        .contains("site_extensions"));
}

#[test]
fn corrupt_relation_rows_are_reported_instead_of_silently_omitted() {
    let database = database();
    let mut connection = database.lock_conn().unwrap();
    insert(&mut connection, &site("a"));
    connection
        .execute(
            "UPDATE site_tags SET tag=CAST('bad' AS BLOB) WHERE site_id='a'",
            [],
        )
        .unwrap();
    assert!(read_sites(&connection).unwrap_err().contains("site_tags"));
    assert!(read_site(&connection, "a")
        .unwrap_err()
        .contains("site_tags"));
}

#[test]
fn all_relation_tables_use_site_id_indexes_for_single_site_reads() {
    let database = database();
    let connection = database.lock_conn().unwrap();
    for table in ["site_tags", "site_maintainers", "site_extensions"] {
        let mut statement = connection
            .prepare(&format!(
                "EXPLAIN QUERY PLAN SELECT * FROM {table} WHERE site_id=?1 ORDER BY rowid"
            ))
            .unwrap();
        let plan = statement
            .query_map(["a"], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
            .join("\n");
        assert!(
            plan.contains(&format!("idx_{table}_site")),
            "{table} query must use its index: {plan}"
        );
    }
}

#[test]
fn bulk_relations_attach_only_to_their_owner_for_large_libraries() {
    let database = database();
    let mut connection = database.lock_conn().unwrap();
    for index in 0..250 {
        insert(&mut connection, &site(&format!("site-{index}")));
    }
    let sites = read_sites(&connection).unwrap();
    assert_eq!(sites.len(), 250);
    for item in sites {
        assert_eq!(
            item.tags,
            [format!("{}-tag-z", item.id), format!("{}-tag-a", item.id)]
        );
        assert_eq!(item.maintainers.len(), 1);
        assert_eq!(item.maintainers[0].id, format!("{}-owner", item.id));
        assert_eq!(item.extension_links.len(), 1);
        assert_eq!(item.extension_links[0].label, format!("{} docs", item.id));
    }
}

#[test]
fn library_payload_keeps_cached_data_and_current_refresh_errors_together() {
    let database = database();
    let mut connection = database.lock_conn().unwrap();
    insert(&mut connection, &site("a"));
    connection.execute_batch(
        "INSERT INTO site_accounts (site_id,profile_id,domain,cookie_count,cookie_names,profile_name,account_name)
             VALUES ('a','Profile 1','a.example',1,'[\"session\"]','Profile 1','account');
         INSERT INTO site_model_cache (site_id,profile_id,keys_json,models_json,error)
             VALUES ('a','Profile 1','[\"old-key\"]','[{\"id\":\"old-model\"}]','HTTP 503');"
    ).unwrap();
    let payload = read_library(&connection).unwrap();
    assert_eq!(payload.sites.len(), 1);
    assert!(!payload.suggested_tags.is_empty());
    let session = &payload.usage_sites[0].sessions[0];
    assert_eq!(session.api_key_count, 1);
    assert_eq!(session.api_model_count, 1);
    assert_eq!(session.api_sync_error, "HTTP 503");
    // API and JSON field names remain unchanged.
    let value = serde_json::to_value(payload).unwrap();
    assert!(value.get("suggestedTags").is_some());
    assert!(value.get("usageSites").is_some());
    assert_eq!(
        value["sites"][0]["auxBaseUrl"],
        json!("https://backup.example/v1")
    );
}

#[test]
fn session_grouping_preserves_order_and_keeps_accounts_in_their_site() {
    let database = database();
    let mut connection = database.lock_conn().unwrap();
    for id in ["b", "a"] {
        insert(&mut connection, &site(id));
    }
    for (site, profile) in [
        ("b", "Profile 2"),
        ("a", "Profile 2"),
        ("b", "Profile 1"),
        ("a", "Profile 1"),
    ] {
        connection.execute(
            "INSERT INTO site_accounts (site_id,profile_id,domain,cookie_count,cookie_names,profile_name,account_name)
             VALUES (?1,?2,'example.com',1,'[]',?2,?1)", [site,profile]
        ).unwrap();
    }
    let usage = read_cached_usage_sites(&connection).unwrap();
    assert_eq!(usage.len(), 2);
    for (index, id) in ["a", "b"].iter().enumerate() {
        assert_eq!(&usage[index].site_id, id);
        let sessions = &usage[index].sessions;
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].profile_id, "Profile 1");
        assert_eq!(sessions[1].profile_id, "Profile 2");
        assert!(sessions.iter().all(|session| &session.account_name == id));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn database_contention_does_not_block_the_async_executor() {
    let database = Arc::new(database());
    let lock = database.lock_conn().unwrap();
    let mut task = tokio::spawn(load_library(database.clone()));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(40), &mut task)
            .await
            .is_err(),
        "reader should wait while the DB lock is held, without blocking the timeout"
    );
    drop(lock);
    task.await.unwrap().unwrap();
}

use super::*;

#[test]
#[ignore]
fn dump_real_project_keys() {
    let tmp = std::env::temp_dir().join("openhub-project-key-dump.json");
    std::env::set_var("OPENHUB_TOKEN_CACHE_PATH", &tmp);
    let data = collect_uncached(true).expect("collect");
    let mut rows = std::collections::BTreeMap::<
        (String, String),
        (i64, std::collections::BTreeSet<String>),
    >::new();
    for bucket in &data.usage.buckets {
        let entry = rows
            .entry((bucket.workspace_root.clone(), bucket.project_key.clone()))
            .or_default();
        entry.0 += bucket.total_tokens;
        entry.1.insert(bucket.source.clone());
    }
    for ((workspace, project), (total, sources)) in rows {
        println!(
            "{:>12} | {:<60} | {:<50} | {:?}",
            total, workspace, project, sources
        );
    }
    let _ = std::fs::remove_file(tmp);
}

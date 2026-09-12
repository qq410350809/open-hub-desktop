//! 项目键归一化。
//!
//! 项目键（`project_key`）有两种形态：
//! - 路径键：会话初始工作区所属项目根的绝对路径（`.git` 优先；无仓库时取最近清单目录；
//!   都没有则取 cwd 自身——会话初始工作区即项目）。
//!   但位置在临时/内部目录（`/tmp`、`/var/folders`、Windows Temp）、家目录本身、
//!   或末级目录名是会话 id / UUID / 工具默认工作区名（`default` 等）的会话不产生路径键，
//!   统一归入标签键「临时任务 / 独立会话」。
//!   同一目录在任何工具里都得到同一个键，不同位置的同名目录不会合并。
//! - 标签键：来源无法给出位置时的固定标签（如 `Copilot CLI`、`临时任务 / 独立会话`）。
//!
//! 工作区根只由 [`assign_workspace_roots`] 根据**已出现的路径键**做拓扑归组，
//! 不读项目目录里的 `pom.xml` / `.git` / `package.json`。目录删了，历史会话仍按原路径归并。
//! 最近的祖先键即工作区；若该祖先自己也是项目键且有 ≥2 个直接子键，则视为收藏夹（key-bucket），
//! 不把并列的独立仓库捏在一起。剩下的兄弟键再按直接父目录合成工作区（父目录是边界或
//! key-bucket 时跳过）。嵌套工作区提升到最外层，供前端两级展示。
//! 多根 `.code-workspace` 仍在解析项目键时读取：文件还在则按 folders 的公共父目录定位，
//! 文件已删则去掉后缀当路径。
use crate::models::{TokenSession, TokenUsageBucket};
use serde_json::Value as JsonValue;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};

pub const TRANSIENT_PROJECT_KEY: &str = "临时任务 / 独立会话";

/// 版本控制根：仓库即项目，最强信号（worktree / 子模块的 `.git` 是文件，`exists` 同样覆盖）。
const VCS_MARKERS: &[&str] = &[".git", ".hg", ".svn"];
/// 构建清单：无仓库时用于定位项目根。
const MANIFEST_MARKERS: &[&str] = &[
    "Cargo.toml",
    "package.json",
    "pom.xml",
    "go.mod",
    "pyproject.toml",
    "build.gradle",
    "build.gradle.kts",
    "settings.gradle",
    "settings.gradle.kts",
    "composer.json",
    "Gemfile",
    "pubspec.yaml",
    "Package.swift",
    "mix.exs",
    "deno.json",
];

pub fn is_session_uuid(s: &str) -> bool {
    let s = s.trim();
    if s.len() == 36
        && s.as_bytes()[8] == b'-'
        && s.as_bytes()[13] == b'-'
        && s.as_bytes()[18] == b'-'
        && s.as_bytes()[23] == b'-'
    {
        return s.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
    }
    false
}

/// 会话 id / 临时目录名 / 工具默认工作区名等不代表任何项目位置的值。
fn is_transient_marker(s: &str) -> bool {
    let s = s.trim();
    is_session_uuid(s)
        || s.starts_with("session-")
        || s.starts_with("rollout-")
        || matches!(
            s,
            "default" | "Default Project" | "untitled" | "Untitled" | "未命名"
        )
}

/// 路径形态的临时/内部位置判定。仅在链上找不到任何标记（`ProjectRoots.found == false`）时生效：
/// - 家目录本身、系统临时目录（/tmp、/private/tmp、/var/folders、Windows AppData Temp）；
/// - 末级目录名是会话 id / UUID / 工具默认工作区名。
/// 无标记的普通工作目录不在此列，仍以 cwd 自身为项目键（会话初始工作区即项目）。
fn is_transient_path(path: &Path) -> bool {
    let home = home_dir();
    if home.as_deref() == Some(path) {
        return true;
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    if !name.is_empty()
        && (is_session_uuid(name)
            || name.starts_with("session-")
            || name.starts_with("rollout-")
            || matches!(
                name,
                "default" | "Default Project" | "untitled" | "Untitled" | "未命名"
            ))
    {
        return true;
    }
    let s = path.to_string_lossy();
    let t = s.trim_end_matches('/');
    if t == "/tmp" || t.starts_with("/tmp/") || t.starts_with("/private/tmp") {
        return true;
    }
    let lowered = t.to_ascii_lowercase();
    lowered.starts_with("/var/folders/") || lowered.contains("/appdata/local/temp")
}

/// 路径键：以 `/` 或 Windows 盘符开头。其余一律视为标签键。
pub fn is_path_like_key(key: &str) -> bool {
    let bytes = key.as_bytes();
    key.starts_with('/')
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && bytes[2] == b'/')
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// 常见「项目收藏夹」目录名：到这里停止，避免把 IdeaProjects 整棵树当成一个工作区。
const BOUNDARY_NAMES: &[&str] = &[
    "Desktop",
    "Documents",
    "Downloads",
    "Pictures",
    "Movies",
    "Music",
    "Library",
    "Applications",
    "IdeaProjects",
    "Projects",
    "AndroidStudioProjects",
];

/// `/Users/<name>`、`/home/<name>` 视为家目录边界，不依赖当前进程的 HOME。
fn is_user_home_dir(dir: &Path) -> bool {
    matches!(
        dir.parent().and_then(|parent| parent.to_str()),
        Some("/Users") | Some("/home")
    ) && dir.file_name().is_some()
}

/// 向上遍历的边界：到这些目录（含）即停止，不把它们当作项目或工作区。
fn is_boundary_dir(dir: &Path, home: Option<&Path>) -> bool {
    if dir.as_os_str().is_empty() || dir.parent().is_none() {
        return true;
    }
    if home.is_some_and(|home| dir == home) || is_user_home_dir(dir) {
        return true;
    }
    if matches!(
        dir.to_str(),
        Some("/Users")
            | Some("/home")
            | Some("/Applications")
            | Some("/tmp")
            | Some("/private/tmp")
    ) {
        return true;
    }
    dir.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| BOUNDARY_NAMES.contains(&name))
}

/// 合成工作区时跳过收藏夹：边界目录，以及紧挨边界的那一层（`~/code`、`/Applications/custom`）。
fn is_collection_parent(dir: &Path, home: Option<&Path>) -> bool {
    is_boundary_dir(dir, home)
        || dir
            .parent()
            .is_some_and(|parent| is_boundary_dir(parent, home))
}

fn has_marker(dir: &Path, markers: &[&str]) -> bool {
    markers.iter().any(|marker| dir.join(marker).exists())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRoots {
    /// 最近一层项目根。
    pub project: PathBuf,
    /// 链上是否真的找到了标记（仓库/清单）。false 表示起点所在位置不构成项目——
    /// 此时起点属于临时/内部目录时归入临时会话，普通目录仍以 cwd 自身为键。
    pub found: bool,
}

fn resolve_project_roots_uncached(start: &Path) -> ProjectRoots {
    let home = home_dir();
    let mut chain: Vec<&Path> = Vec::new();
    let mut current = Some(start);
    while let Some(dir) = current {
        if is_boundary_dir(dir, home.as_deref()) {
            break;
        }
        chain.push(dir);
        current = dir.parent();
    }
    if chain.is_empty() {
        return ProjectRoots {
            project: start.to_path_buf(),
            found: false,
        };
    }
    let vcs_idx = chain.iter().position(|dir| has_marker(dir, VCS_MARKERS));
    let manifest_idx = chain
        .iter()
        .position(|dir| has_marker(dir, MANIFEST_MARKERS));
    let project_idx = vcs_idx.or(manifest_idx).unwrap_or(0);
    let project = chain[project_idx];
    ProjectRoots {
        project: project.to_path_buf(),
        found: vcs_idx.is_some() || manifest_idx.is_some(),
    }
}

fn roots_cache() -> &'static Mutex<HashMap<PathBuf, ProjectRoots>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, ProjectRoots>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 每轮采集开始时清空，避免项目目录被创建/删除后沿用旧判定。
pub fn reset_project_roots_cache() {
    if let Ok(mut cache) = roots_cache().lock() {
        cache.clear();
    }
}

/// 解析路径对应的项目根。结果按进程内缓存（同一目录在日志里会出现成千上万次）。
pub fn resolve_project_roots(start: &Path) -> ProjectRoots {
    if let Ok(cache) = roots_cache().lock() {
        if let Some(hit) = cache.get(start) {
            return hit.clone();
        }
    }
    let roots = resolve_project_roots_uncached(start);
    if let Ok(mut cache) = roots_cache().lock() {
        cache.insert(start.to_path_buf(), roots.clone());
    }
    roots
}

/// 把来源��出的原始位置清洗成绝对路径：去 `file://` 与百分号编码、展开 `~`、统一分隔符、去尾部斜杠。
/// `.code-workspace` 工作区文件取其去后缀后的路径作为位置。
fn clean_location(raw: &str) -> Option<PathBuf> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    let mut cleaned = if let Some(stripped) = s.strip_prefix("file://") {
        percent_encoding::percent_decode_str(stripped)
            .decode_utf8_lossy()
            .to_string()
    } else {
        s.to_string()
    };
    if cleaned.contains('\\') {
        cleaned = cleaned.replace('\\', "/");
    }
    if cleaned == "~" || cleaned.starts_with("~/") {
        let home = home_dir()?;
        cleaned = format!("{}{}", home.to_string_lossy(), &cleaned[1..]);
    }
    let trimmed = if cleaned.len() > 1 {
        cleaned.trim_end_matches('/')
    } else {
        cleaned.as_str()
    };
    if trimmed.ends_with(".code-workspace") && is_path_like_key(trimmed) {
        let ws_path = PathBuf::from(trimmed);
        if ws_path.is_file() {
            if let Some(from_folders) = location_from_code_workspace(&ws_path) {
                return Some(from_folders);
            }
        }
        let stripped = trimmed.strip_suffix(".code-workspace").unwrap_or(trimmed);
        if is_path_like_key(stripped) {
            return Some(PathBuf::from(stripped));
        }
        return None;
    }
    if !is_path_like_key(trimmed) {
        return None;
    }
    Some(PathBuf::from(trimmed))
}

/// 目录位置（cwd / 工作区文件夹）→ 项目键。非路径返回 None，由调用方决定兜底标签。
/// 位置解析不出真实项目根（无仓库/清单标记，或位于临时/内部目录）时归入临时任务。
pub fn project_key_from_location(raw: &str) -> Option<String> {
    if is_transient_marker(raw.trim()) {
        return Some(TRANSIENT_PROJECT_KEY.to_string());
    }
    let path = clean_location(raw)?;
    let start = if path.is_file() {
        path.parent()?.to_path_buf()
    } else {
        path
    };
    let roots = resolve_project_roots(&start);
    if !roots.found && (is_transient_path(&start) || is_boundary_dir(&start, home_dir().as_deref()))
    {
        return Some(TRANSIENT_PROJECT_KEY.to_string());
    }
    Some(roots.project.to_string_lossy().to_string())
}

/// 文件位置（活动文档等）→ 项目键：始终从其父目录开始解析。
pub fn project_key_from_document(raw: &str) -> Option<String> {
    let path = clean_location(raw)?;
    let parent = path.parent()?;
    if parent.as_os_str().is_empty() {
        return None;
    }
    let roots = resolve_project_roots(parent);
    if !roots.found && (is_transient_path(parent) || is_boundary_dir(parent, home_dir().as_deref()))
    {
        return Some(TRANSIENT_PROJECT_KEY.to_string());
    }
    Some(roots.project.to_string_lossy().to_string())
}

/// 位置能解析则用路径键；非路径但非空的值（工作区名等）原样作为标签键；空值退回来��标签。
pub fn project_key_or_label(raw: &str, label: &str) -> String {
    if let Some(key) = project_key_from_location(raw) {
        return key;
    }
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        label.to_string()
    } else {
        trimmed.to_string()
    }
}

/// 单键看不到兄弟关系，无法判定工作区；请用 [`assign_workspace_roots`]。
pub fn workspace_root_for_key(_key: &str) -> String {
    String::new()
}

fn normalize_logical_path(path: PathBuf) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn common_path_prefix(paths: &[PathBuf]) -> Option<PathBuf> {
    let mut iter = paths.iter();
    let mut prefix = iter.next()?.clone();
    for path in iter {
        let mut next = PathBuf::new();
        let mut prefix_comps = prefix.components();
        let mut path_comps = path.components();
        loop {
            match (prefix_comps.next(), path_comps.next()) {
                (Some(left), Some(right)) if left == right => next.push(left.as_os_str()),
                _ => break,
            }
        }
        if next.as_os_str().is_empty() {
            return None;
        }
        prefix = next;
    }
    Some(prefix)
}

fn location_from_code_workspace(path: &Path) -> Option<PathBuf> {
    let text = fs::read_to_string(path).ok()?;
    let val = serde_json::from_str::<JsonValue>(&text).ok()?;
    let folders = val.get("folders")?.as_array()?;
    let base = path.parent()?;
    let mut resolved = Vec::new();
    for folder in folders {
        let rel = folder.get("path")?.as_str()?;
        let raw = PathBuf::from(rel);
        let joined = if raw.is_absolute() {
            raw
        } else {
            base.join(raw)
        };
        resolved.push(normalize_logical_path(joined));
    }
    if resolved.is_empty() {
        return None;
    }
    if resolved.len() == 1 {
        return Some(resolved.remove(0));
    }
    let lca = common_path_prefix(&resolved)?;
    if is_boundary_dir(&lca, home_dir().as_deref()) {
        return path.parent().map(Path::to_path_buf);
    }
    Some(lca)
}

fn is_parent_path(parent: &str, child: &str) -> bool {
    let parent = parent.trim_end_matches('/');
    child.len() > parent.len()
        && child.starts_with(parent)
        && child.as_bytes().get(parent.len()) == Some(&b'/')
}

fn parent_key(key: &str) -> Option<String> {
    let parent = Path::new(key).parent()?;
    if parent.as_os_str().is_empty() {
        return None;
    }
    Some(parent.to_string_lossy().into_owned())
}

fn nearest_ancestor_key(key: &str, key_set: &HashSet<String>) -> Option<String> {
    let mut current = key.to_string();
    while let Some(parent) = parent_key(&current) {
        if key_set.contains(&parent) {
            return Some(parent);
        }
        current = parent;
    }
    None
}

/// 祖先自己是项目键，且至少有两个「直接子键」（`ancestor/name`，中间不再有 `/`）。
/// `/Applications/custom` 下并列的 OpenHub / LanProxy 属于这一类，不能当成一个工作区。
fn is_key_bucket(
    ancestor: &str,
    key_set: &HashSet<String>,
    direct_counts: &HashMap<String, usize>,
) -> bool {
    key_set.contains(ancestor) && direct_counts.get(ancestor).copied().unwrap_or(0) >= 2
}

fn direct_child_counts(path_keys: &[String]) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for key in path_keys {
        if let Some(parent) = parent_key(key) {
            *counts.entry(parent).or_insert(0) += 1;
        }
    }
    counts
}

/// 结合全部项目键，给每个路径键分配工作区根。只看路径拓扑，不读磁盘。
///
/// 1. 最近的祖先键即工作区；若该祖先是 key-bucket（自己也是键且 ≥2 个直接子键）则跳过；
/// 2. 剩余键按直接父目录合成工作区（父目录是收藏夹或 key-bucket 时跳过）；
/// 3. 嵌套工作区提升到最外层，保证前端两级展示。
pub fn assign_workspace_roots(keys: &[String]) -> HashMap<String, String> {
    let mut path_keys: Vec<String> = keys
        .iter()
        .filter(|key| is_path_like_key(key))
        .cloned()
        .collect();
    path_keys.sort();
    path_keys.dedup();

    let key_set: HashSet<String> = path_keys.iter().cloned().collect();
    let direct_counts = direct_child_counts(&path_keys);

    let mut map = HashMap::new();
    for key in &path_keys {
        let Some(ancestor) = nearest_ancestor_key(key, &key_set) else {
            continue;
        };
        if is_key_bucket(&ancestor, &key_set, &direct_counts) {
            continue;
        }
        map.insert(key.clone(), ancestor);
    }

    let remaining: Vec<String> = path_keys
        .iter()
        .filter(|key| !map.contains_key(*key))
        .cloned()
        .collect();
    let mut by_parent: HashMap<String, Vec<String>> = HashMap::new();
    for key in remaining {
        let Some(parent) = parent_key(&key) else {
            continue;
        };
        by_parent.entry(parent).or_default().push(key);
    }
    let home = home_dir();
    for (parent, members) in by_parent {
        if members.len() < 2 {
            continue;
        }
        if is_collection_parent(Path::new(&parent), home.as_deref()) {
            continue;
        }
        if is_key_bucket(&parent, &key_set, &direct_counts) {
            continue;
        }
        for key in members {
            map.entry(key).or_insert_with(|| parent.clone());
        }
    }

    for _ in 0..8 {
        let snapshot = map.clone();
        let mut changed = false;
        for (key, workspace) in map.iter_mut() {
            let Some(parent_ws) = snapshot.get(workspace) else {
                continue;
            };
            if parent_ws.is_empty() || parent_ws == workspace {
                continue;
            }
            if is_parent_path(parent_ws, key) {
                *workspace = parent_ws.clone();
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    map
}

/// 用全部桶和会话的项目键回填 `workspace_root`。
pub fn apply_workspace_roots(buckets: &mut [TokenUsageBucket], sessions: &mut [TokenSession]) {
    let mut keys = Vec::with_capacity(buckets.len() + sessions.len());
    keys.extend(buckets.iter().map(|bucket| bucket.project_key.clone()));
    keys.extend(sessions.iter().map(|session| session.project_key.clone()));
    let map = assign_workspace_roots(&keys);
    for bucket in buckets {
        bucket.workspace_root = map.get(&bucket.project_key).cloned().unwrap_or_default();
    }
    for session in sessions {
        session.workspace_root = map.get(&session.project_key).cloned().unwrap_or_default();
    }
}

/// 还原 Claude / Command Code 的 `-Users-name-dir` 式目录名为绝对路径。
/// `-` 既是分隔符也可能是目录名的一部分，按最长优先逐段探测文件系统消歧；
/// 探测不到的剩余部分整体作为最后一级目录名。会话/临时目录返回 None。
pub fn decode_encoded_dash_path(raw: &str) -> Option<PathBuf> {
    let raw = raw.trim();
    if raw.is_empty() || raw.contains("-copilot-chats-") || is_session_uuid(raw.trim_matches('-')) {
        return None;
    }
    if !raw.starts_with('-') {
        return None;
    }
    let sub_parts: Vec<&str> = raw.split('-').filter(|s| !s.is_empty()).collect();
    if sub_parts.is_empty() {
        return None;
    }
    let mut curr_path = PathBuf::from("/");
    let mut idx = 0;
    while idx < sub_parts.len() {
        let mut matched = false;
        for end in (idx + 1..=sub_parts.len()).rev() {
            let segment = sub_parts[idx..end].join("-");
            let test_dir = curr_path.join(&segment);
            if test_dir.exists() {
                curr_path = test_dir;
                idx = end;
                matched = true;
                break;
            }
        }
        if !matched {
            curr_path = curr_path.join(sub_parts[idx..].join("-"));
            break;
        }
    }
    Some(curr_path)
}

/// Claude / Command Code 项目目录名 → 项目键。
pub fn project_key_from_encoded_dir_name(raw: &str, label: &str) -> String {
    let raw = raw.trim();
    if raw.contains("-copilot-chats-") || is_session_uuid(raw.trim_matches('-')) {
        return TRANSIENT_PROJECT_KEY.to_string();
    }
    let Some(path) = decode_encoded_dash_path(raw) else {
        return label.to_string();
    };
    let roots = resolve_project_roots(&path);
    if !roots.found && (is_transient_path(&path) || is_boundary_dir(&path, home_dir().as_deref())) {
        return TRANSIENT_PROJECT_KEY.to_string();
    }
    roots.project.to_string_lossy().to_string()
}

pub fn claude_project_from_path(path: &Path) -> String {
    let mut parent = path.parent();
    while parent
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .map(|name| name == "subagents")
        .unwrap_or(false)
    {
        parent = parent.and_then(Path::parent);
    }
    let raw = parent
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .unwrap_or("");
    project_key_from_encoded_dir_name(raw, "Claude")
}

pub fn command_code_project_from_path(path: &Path) -> String {
    let raw = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .unwrap_or("");
    project_key_from_encoded_dir_name(raw, "Command Code")
}

/// 读取 VS Code 系（VS Code / Cursor / Windsurf）`workspaceStorage/<hash>/workspace.json`
/// 里的文件夹 URI 并解析为项目键。
pub fn vscode_workspace_key_from_storage_dir(ws_dir: &Path) -> Option<String> {
    let text = fs::read_to_string(ws_dir.join("workspace.json")).ok()?;
    let val = serde_json::from_str::<JsonValue>(&text).ok()?;
    let folder_uri = val
        .get("folder")
        .or_else(|| val.get("workspace"))
        .and_then(JsonValue::as_str)?;
    project_key_from_location(folder_uri)
}

/// `workspaceStorage/<hash>/state.vscdb` → 项目键；全局库或无法解析时退回标签。
pub fn vscode_workspace_key_from_db_path(db_path: &Path, label: &str) -> String {
    db_path
        .parent()
        .and_then(vscode_workspace_key_from_storage_dir)
        .unwrap_or_else(|| label.to_string())
}

pub fn vscode_workspace_project_from_path(path: &Path) -> String {
    if path
        .components()
        .any(|c| c.as_os_str() == "emptyWindowChatSessions")
    {
        return TRANSIENT_PROJECT_KEY.to_string();
    }
    let mut parent = path.parent();
    while let Some(p) = parent {
        if p.file_name().and_then(|n| n.to_str()) == Some("chatSessions") {
            if let Some(key) = p.parent().and_then(vscode_workspace_key_from_storage_dir) {
                return key;
            }
            break;
        }
        parent = p.parent();
    }
    "VS Code".to_string()
}

pub fn extract_antigravity_project_from_transcript(text: &str) -> Option<String> {
    for line in text.lines().take(25) {
        if let Ok(val) = serde_json::from_str::<JsonValue>(line) {
            if let Some(content) = val.get("content").and_then(JsonValue::as_str) {
                if let Some(pos) = content.find("Active Document: ") {
                    let sub = &content[pos + "Active Document: ".len()..];
                    let path_str = sub.lines().next().unwrap_or("").trim();
                    let clean = path_str.split('(').next().unwrap_or(path_str).trim();
                    if let Some(resolved) = project_key_from_document(clean) {
                        return Some(resolved);
                    }
                }
                if let Some(pos) = content.find(" -> ") {
                    let before = &content[..pos];
                    if let Some(line_start) = before.rfind('\n') {
                        let cand = before[line_start + 1..].trim();
                        if cand.starts_with('/') {
                            if let Some(resolved) = project_key_from_document(cand) {
                                return Some(resolved);
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

pub fn command_code_sidecar_model(path: &Path) -> String {
    let Ok(text) = fs::read_to_string(path.with_extension("meta.json")) else {
        return String::new();
    };
    serde_json::from_str::<JsonValue>(&text)
        .ok()
        .and_then(|value| {
            value
                .get("model")
                .and_then(JsonValue::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_default()
}

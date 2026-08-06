#!/bin/bash
set -e
cd /Applications/custom/OpenHub/src-tauri/src

# 1. Fix models_fetch.rs: remove trailing #[cfg(test)]
# The last line is "#[cfg(test)]" with nothing after it
sed -i '' '$d' models_fetch.rs
# Also remove any trailing empty line before it
sed -i '' -e '${/^$/d}' models_fetch.rs

# 2. Make all struct fields pub(crate) in models.rs, site_ops.rs, models_fetch.rs
# Add pub(crate) before each indented field that doesn't already have pub
for f in models.rs site_ops.rs models_fetch.rs account_sync.rs chrome_usage.rs; do
  # For tuple struct Database: add pub(crate) to the field
  sed -i '' -E 's/^pub\(crate\) struct Database\(std::sync::Mutex<Connection>\);/pub(crate) struct Database(pub(crate) std::sync::Mutex<Connection>);/' "$f"
  # For named fields: add pub(crate) before lines matching pattern (4 or 8 space indent, word: type)
  # But skip lines that already have pub, start with #, //, or are inside fn
  sed -i '' -E '/^[[:space:]]{4,8}pub/b; /^[[:space:]]{4,8}#/b; /^[[:space:]]{4,8}\//b; s/^([[:space:]]{4,8})([a-z_]+):([^:])/\1pub(crate) \2:\3/' "$f"
done

# 3. Fix missing imports

# chrome_usage.rs: needs rusqlite::params and std::time
sed -i '' '1s/^/use rusqlite::params;\
/' chrome_usage.rs

# system_detect.rs: needs rusqlite::params
sed -i '' '1s/^/use rusqlite::params;\
/' system_detect.rs

# site_ops.rs: needs std::time::{SystemTime, UNIX_EPOCH} (already has it in imports but might be wrong)
# Check if SystemTime and UNIX_EPOCH are imported
grep -q 'SystemTime' site_ops.rs && echo "SystemTime found" || echo "SystemTime NOT found"

# remote_sync.rs: needs HashSet (already has it?)

# account_sync.rs: needs chrome_local_storage import
grep -q 'chrome_local_storage' account_sync.rs && echo "chrome_local_storage found" || echo "chrome_local_storage NOT found"

# 4. Fix lib.rs: add tauri::Manager import and use module paths for commands
# Already has use std::fs; but needs tauri::Manager for app.path() and app.manage()

# 5. Fix generate_handler! in lib.rs to use module paths
# The issue is that #[tauri::command] functions in submodules need to be referenced by path
# Let's change the generate_handler to use module::function paths

echo "Fix script done"

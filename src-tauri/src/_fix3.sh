#!/bin/bash
set -e
cd /Applications/custom/OpenHub/src-tauri/src

# 1. Fix models.rs: Remove pub(crate) from enum variant fields
sed -i '' 's/        pub(crate) cookie_header: String,/        cookie_header: String,/' models.rs
sed -i '' 's/        pub(crate) user_id: String,/        user_id: String,/' models.rs
sed -i '' 's/        pub(crate) access_token: String,/        access_token: String,/' models.rs

# 2. Fix models_fetch.rs: Add missing imports
sed -i '' '1s/^use rusqlite::Connection;/use rusqlite::{params, Connection, OptionalExtension};/' models_fetch.rs
sed -i '' 's/^use std::time::Duration;/use std::time::{Duration, SystemTime, UNIX_EPOCH};/' models_fetch.rs

# 3. Fix remote_sync.rs: Add HashSet and OptionalExtension
sed -i '' 's/^use std::time::Duration;/use std::{collections::HashSet, time::Duration};/' remote_sync.rs
sed -i '' 's/^use serde::{Deserialize, Serialize};/use serde::{Deserialize, Serialize};\nuse rusqlite::OptionalExtension;/' remote_sync.rs
sed -i '' 's/^use tauri::State;/use tauri::{Manager, State};/' remote_sync.rs

# 4. Fix account_sync.rs: Add SystemTime, UNIX_EPOCH, OptionalExtension, Manager
sed -i '' 's/^use std::time::Duration;/use std::time::{Duration, SystemTime, UNIX_EPOCH};/' account_sync.rs
sed -i '' 's/^use rusqlite::{params, Connection};/use rusqlite::{params, Connection, OptionalExtension};/' account_sync.rs
sed -i '' 's/^use tauri::State;/use tauri::{Manager, State};/' account_sync.rs

# 5. Fix chrome_usage.rs: Add chrome_local_storage, OptionalExtension, Manager
sed -i '' 's/^use rusqlite::{params, Connection};/use rusqlite::{params, Connection, OptionalExtension};/' chrome_usage.rs
sed -i '' 's/^use tauri::State;/use tauri::{Manager, State};/' chrome_usage.rs
sed -i '' 's/^use crate::chrome_session;$/use crate::chrome_session;\nuse crate::chrome_local_storage;/' chrome_usage.rs

# 6. Fix system_detect.rs: Add HashSet, SystemTime, UNIX_EPOCH
sed -i '' 's/^use std::collections::HashMap;/use std::collections::{HashMap, HashSet};/' system_detect.rs
sed -i '' 's/^use std::time::Duration;/use std::time::{Duration, SystemTime, UNIX_EPOCH};/' system_detect.rs

# 7. Fix site_crud.rs: Add OptionalExtension, Manager
sed -i '' 's/^use rusqlite::{params, Connection};/use rusqlite::{params, Connection, OptionalExtension};/' site_crud.rs
sed -i '' 's/^use tauri::State;/use tauri::{Manager, State};/' site_crud.rs

# 8. Fix db.rs: Add OptionalExtension (already has params and Connection)
sed -i '' 's/^use rusqlite::{params, Connection, OptionalExtension};/use rusqlite::{params, Connection, OptionalExtension};/' db.rs

# 9. Make all #[tauri::command] functions pub instead of pub(crate)
for f in site_crud.rs models_fetch.rs db.rs remote_sync.rs account_sync.rs chrome_usage.rs system_detect.rs; do
  # Replace "pub(crate) fn" or "pub(crate) async fn" that follows #[tauri::command]
  # We need to find lines with #[tauri::command] and the next line with pub(crate)
  sed -i '' -E '/^#\[tauri::command\]$/{n; s/^pub\(crate\) (async )?fn/pub \1fn/}' "$f"
done

echo "All fixes applied!"

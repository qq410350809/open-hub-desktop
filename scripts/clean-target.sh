#!/usr/bin/env bash
# OpenHub target 清理脚本：保留最新编译产物，清除旧版本/冗余文件
# 用法：bash scripts/clean-target.sh [--deep]
#   默认：清除旧二进制 + 旧 incremental 会话 + .d 依赖文件
#   --deep：额外清除所有 .o（下次编译需重新链接，慎用）

set -euo pipefail
cd "$(dirname "$0")/.." || exit 1

TARGET="src-tauri/target"
if [ ! -d "$TARGET" ]; then
  echo "[clean] target 目录不存在，跳过"
  exit 0
fi

DEPS="$TARGET/debug/deps"

# macOS BSD head 不支持 -n -1，用 awk 代替
drop_last() { awk 'NR>1{print prev}{prev=$0}END{}'; }

# 1) 清除 debug/deps 下旧版本的可执行二进制（只保留最新 hash 的）
if [ -d "$DEPS" ]; then
  BINS=$(find "$DEPS" -maxdepth 1 -name "open_hub_desktop-*" -type f ! -name "*.d" ! -name "*.o" 2>/dev/null | sort)
  COUNT=$(printf '%s' "$BINS" | grep -c . 2>/dev/null || true)
  if [ "$COUNT" -gt 1 ]; then
    echo "$BINS" | drop_last | while IFS= read -r f; do
      SIZE=$(du -h "$f" | cut -f1)
      rm -f "$f"
      echo "[clean] 删除旧二进制 $(basename "$f") ($SIZE)"
    done
  fi

  # 2) 清除重复版本 crate 的 .rlib（同名保留最新 2 个）
  for base in $(find "$DEPS" -maxdepth 1 -name "lib*.rlib" -exec basename {} \; | sed 's/-[a-f0-9]*\.rlib$//' | sort -u); do
    RLBS=$(find "$DEPS" -maxdepth 1 -name "${base}-*.rlib" | sort)
    CNT=$(printf '%s' "$RLBS" | grep -c . 2>/dev/null || true)
    if [ "$CNT" -gt 2 ]; then
      printf '%s\n' "$RLBS" | drop_last | drop_last | while IFS= read -r f; do
        rm -f "$f"
      done
    fi
  done

  # 3) 清除所有 .d 依赖跟踪文件（仅调试用，不影响缓存命中）
  find "$DEPS" -maxdepth 1 -name "*.d" -delete 2>/dev/null && echo "[clean] 清除 .d 文件"

  # 4) --deep 模式：清除 .o 目标文件
  if [ "${1:-}" = "--deep" ]; then
    find "$DEPS" -maxdepth 1 -name "*.o" -delete 2>/dev/null
    echo "[deep] 清除 .o 文件"
  fi
fi

# 5) 清除 incremental 缓存（只保留最近 3 个会话）
INC="$TARGET/debug/incremental"
if [ -d "$INC" ]; then
  DIRS=$(find "$INC" -maxdepth 1 -mindepth 1 -type d | sort)
  CNT=$(printf '%s' "$DIRS" | grep -c . 2>/dev/null || true)
  if [ "$CNT" -gt 3 ]; then
    echo "$DIRS" | awk 'NR>3{print}' | while IFS= read -r d; do
      SIZE=$(du -h "$d" 2>/dev/null | cut -f1)
      rm -rf "$d"
      echo "[clean] 清除旧 incremental 会话 $(basename "$d") ($SIZE)"
    done
  fi
fi

# 6) --deep 模式清除 release 目录
if [ "${1:-}" = "--deep" ] && [ -d "$TARGET/release" ]; then
  rm -rf "$TARGET/release"
  echo "[deep] 清除 release 目录"
fi

echo "[clean] 完成"

use std::{
    collections::{HashMap, HashSet},
    fs,
    path::Path,
};

const LEVELDB_TABLE_MAGIC: u64 = 0xdb47_7524_8b80_fb57;
const LOG_BLOCK_SIZE: usize = 32 * 1024;
const STORAGE_KEYS: [&str; 6] = [
    "user",
    "quota_display_type",
    "quota_per_unit",
    "status",
    "auth_token",
    "auth_user",
];

#[derive(Debug, Clone)]
pub(crate) struct LocalStorageTarget {
    pub(crate) site_id: String,
    pub(crate) profile_id: String,
    pub(crate) origin: String,
}

#[derive(Debug)]
pub(crate) struct LocalStorageMatch {
    pub(crate) site_id: String,
    pub(crate) profile_id: String,
    pub(crate) values: HashMap<String, String>,
    pub(crate) error: String,
}

#[derive(Debug)]
struct StorageRecord {
    key: Vec<u8>,
    sequence: u64,
    value: Option<Vec<u8>>,
}

pub(crate) fn read_local_storage_from_home(
    home_dir: &Path,
    targets: &[LocalStorageTarget],
) -> Vec<LocalStorageMatch> {
    let chrome_root = home_dir.join("Library/Application Support/Google/Chrome");
    let mut by_profile: HashMap<&str, Vec<&LocalStorageTarget>> = HashMap::new();
    for target in targets {
        by_profile
            .entry(&target.profile_id)
            .or_default()
            .push(target);
    }

    let mut matches = Vec::with_capacity(targets.len());
    for (profile_id, profile_targets) in by_profile {
        let wanted = profile_targets
            .iter()
            .flat_map(|target| {
                STORAGE_KEYS
                    .iter()
                    .map(|key| (target.origin.clone(), (*key).to_string()))
            })
            .collect::<HashSet<_>>();
        let directory = chrome_root.join(profile_id).join("Local Storage/leveldb");
        let result = read_profile_storage(&directory, &wanted);
        for target in profile_targets {
            match &result {
                Ok(values) => {
                    let values = STORAGE_KEYS
                        .iter()
                        .filter_map(|key| {
                            values
                                .get(&(target.origin.clone(), (*key).to_string()))
                                .cloned()
                                .map(|value| ((*key).to_string(), value))
                        })
                        .collect();
                    matches.push(LocalStorageMatch {
                        site_id: target.site_id.clone(),
                        profile_id: target.profile_id.clone(),
                        values,
                        error: String::new(),
                    });
                }
                Err(error) => matches.push(LocalStorageMatch {
                    site_id: target.site_id.clone(),
                    profile_id: target.profile_id.clone(),
                    values: HashMap::new(),
                    error: error.clone(),
                }),
            }
        }
    }
    matches
}

fn read_profile_storage(
    directory: &Path,
    wanted: &HashSet<(String, String)>,
) -> Result<HashMap<(String, String), String>, String> {
    if !directory.is_dir() {
        return Err("未找到该 Chrome Profile 的 Local Storage".into());
    }
    let mut records = Vec::new();
    let mut parsed_files = 0_usize;
    for entry in
        fs::read_dir(directory).map_err(|error| format!("无法读取 Local Storage：{error}"))?
    {
        let entry = entry.map_err(|error| format!("Local Storage 文件无效：{error}"))?;
        let path = entry.path();
        match path.extension().and_then(|value| value.to_str()) {
            Some("ldb") => {
                if let Ok(mut file_records) = read_table_records(&path) {
                    parsed_files += 1;
                    records.append(&mut file_records);
                }
            }
            Some("log") => {
                if let Ok(mut file_records) = read_log_records(&path) {
                    parsed_files += 1;
                    records.append(&mut file_records);
                }
            }
            _ => {}
        }
    }
    if parsed_files == 0 {
        return Err("无法解析该 Chrome Profile 的 Local Storage".into());
    }

    let mut latest: HashMap<(String, String), (u64, Option<String>)> = HashMap::new();
    for record in records {
        let Some(storage_key) = decode_storage_key(&record.key) else {
            continue;
        };
        if !wanted.contains(&storage_key) {
            continue;
        }
        let value = record.value.as_deref().and_then(decode_blink_string);
        let current = latest.entry(storage_key).or_insert((0, None));
        if record.sequence >= current.0 {
            *current = (record.sequence, value);
        }
    }
    Ok(latest
        .into_iter()
        .filter_map(|(key, (_, value))| value.map(|value| (key, value)))
        .collect())
}

fn decode_storage_key(bytes: &[u8]) -> Option<(String, String)> {
    if bytes.first() != Some(&b'_') {
        return None;
    }
    let separator = bytes.iter().position(|byte| *byte == 0)?;
    let origin = std::str::from_utf8(bytes.get(1..separator)?)
        .ok()?
        .to_string();
    let key = decode_blink_string(bytes.get(separator + 1..)?)?;
    Some((origin, key))
}

fn decode_blink_string(bytes: &[u8]) -> Option<String> {
    let (encoding, value) = bytes.split_first()?;
    match *encoding {
        0 => {
            let chunks = value.chunks_exact(2);
            if !chunks.remainder().is_empty() {
                return None;
            }
            let code_units = chunks
                .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
                .collect::<Vec<_>>();
            String::from_utf16(&code_units).ok()
        }
        1 => Some(String::from_utf8_lossy(value).into_owned()),
        _ => std::str::from_utf8(bytes).ok().map(str::to_string),
    }
}

fn read_table_records(path: &Path) -> Result<Vec<StorageRecord>, String> {
    let file = fs::read(path).map_err(|error| format!("无法读取 LevelDB 表：{error}"))?;
    if file.len() < 48 {
        return Err("LevelDB 表过短".into());
    }
    let footer = &file[file.len() - 48..];
    let magic = u64::from_le_bytes(footer[40..48].try_into().map_err(|_| "LevelDB 页脚无效")?);
    if magic != LEVELDB_TABLE_MAGIC {
        return Err("LevelDB 表签名无效".into());
    }
    let mut footer_position = 0;
    read_block_handle(footer, &mut footer_position)?;
    let index_handle = read_block_handle(footer, &mut footer_position)?;
    let index = read_table_block(&file, index_handle)?;
    let index_entries = read_block_entries(&index)?;
    let mut records = Vec::new();
    for (_, handle_value) in index_entries {
        let mut position = 0;
        let handle = read_block_handle(&handle_value, &mut position)?;
        let block = read_table_block(&file, handle)?;
        for (internal_key, value) in read_block_entries(&block)? {
            if internal_key.len() < 8 {
                continue;
            }
            let tag = u64::from_le_bytes(
                internal_key[internal_key.len() - 8..]
                    .try_into()
                    .map_err(|_| "LevelDB 内部键无效")?,
            );
            records.push(StorageRecord {
                key: internal_key[..internal_key.len() - 8].to_vec(),
                sequence: tag >> 8,
                value: (tag as u8 == 1).then_some(value),
            });
        }
    }
    Ok(records)
}

fn read_log_records(path: &Path) -> Result<Vec<StorageRecord>, String> {
    let file = fs::read(path).map_err(|error| format!("无法读取 LevelDB 日志：{error}"))?;
    let mut batches = Vec::new();
    let mut fragmented = Vec::new();
    let mut position = 0_usize;
    while position + 7 <= file.len() {
        let block_offset = position % LOG_BLOCK_SIZE;
        if LOG_BLOCK_SIZE - block_offset < 7 {
            position += LOG_BLOCK_SIZE - block_offset;
            continue;
        }
        let length = u16::from_le_bytes([file[position + 4], file[position + 5]]) as usize;
        let record_type = file[position + 6];
        position += 7;
        if length == 0 && record_type == 0 {
            position += LOG_BLOCK_SIZE - (position % LOG_BLOCK_SIZE);
            continue;
        }
        let Some(end) = position.checked_add(length) else {
            break;
        };
        let Some(payload) = file.get(position..end) else {
            break;
        };
        position = end;
        match record_type {
            1 => batches.push(payload.to_vec()),
            2 => {
                fragmented.clear();
                fragmented.extend_from_slice(payload);
            }
            3 => fragmented.extend_from_slice(payload),
            4 => {
                fragmented.extend_from_slice(payload);
                batches.push(std::mem::take(&mut fragmented));
            }
            _ => {}
        }
    }
    let mut records = Vec::new();
    for batch in batches {
        records.extend(read_write_batch(&batch)?);
    }
    Ok(records)
}

fn read_write_batch(batch: &[u8]) -> Result<Vec<StorageRecord>, String> {
    if batch.len() < 12 {
        return Err("LevelDB WriteBatch 过短".into());
    }
    let sequence = u64::from_le_bytes(batch[..8].try_into().map_err(|_| "WriteBatch 序号无效")?);
    let count =
        u32::from_le_bytes(batch[8..12].try_into().map_err(|_| "WriteBatch 数量无效")?) as usize;
    let mut position = 12;
    let mut records = Vec::with_capacity(count.min(4096));
    for index in 0..count {
        let record_type = *batch.get(position).ok_or("WriteBatch 记录不完整")?;
        position += 1;
        let key = read_length_prefixed(batch, &mut position)?.to_vec();
        let value = match record_type {
            0 => None,
            1 => Some(read_length_prefixed(batch, &mut position)?.to_vec()),
            _ => return Err("WriteBatch 记录类型不支持".into()),
        };
        records.push(StorageRecord {
            key,
            sequence: sequence + index as u64,
            value,
        });
    }
    Ok(records)
}

fn read_length_prefixed<'a>(bytes: &'a [u8], position: &mut usize) -> Result<&'a [u8], String> {
    let length = usize::try_from(read_varint(bytes, position)?).map_err(|_| "字段过长")?;
    let end = position.checked_add(length).ok_or("字段长度溢出")?;
    let value = bytes.get(*position..end).ok_or("字段数据不完整")?;
    *position = end;
    Ok(value)
}

fn read_block_handle(bytes: &[u8], position: &mut usize) -> Result<(usize, usize), String> {
    let offset = usize::try_from(read_varint(bytes, position)?).map_err(|_| "块偏移过大")?;
    let size = usize::try_from(read_varint(bytes, position)?).map_err(|_| "块大小过大")?;
    Ok((offset, size))
}

fn read_table_block(file: &[u8], handle: (usize, usize)) -> Result<Vec<u8>, String> {
    let (offset, size) = handle;
    let end = offset.checked_add(size).ok_or("LevelDB 块范围溢出")?;
    let block = file.get(offset..end).ok_or("LevelDB 块超出文件范围")?;
    let compression = *file.get(end).ok_or("LevelDB 块缺少压缩标记")?;
    match compression {
        0 => Ok(block.to_vec()),
        1 => decompress_snappy(block),
        _ => Err(format!("不支持的 LevelDB 压缩类型：{compression}")),
    }
}

fn read_block_entries(block: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>, String> {
    if block.len() < 4 {
        return Err("LevelDB 块过短".into());
    }
    let restart_count = u32::from_le_bytes(
        block[block.len() - 4..]
            .try_into()
            .map_err(|_| "LevelDB 重启点无效")?,
    ) as usize;
    let restart_bytes = restart_count.checked_mul(4).ok_or("LevelDB 重启点过多")?;
    let entries_end = block
        .len()
        .checked_sub(4 + restart_bytes)
        .ok_or("LevelDB 重启点越界")?;
    let mut entries = Vec::new();
    let mut previous_key = Vec::new();
    let mut position = 0;
    while position < entries_end {
        let shared =
            usize::try_from(read_varint(block, &mut position)?).map_err(|_| "共享键过长")?;
        let unshared = usize::try_from(read_varint(block, &mut position)?).map_err(|_| "键过长")?;
        let value_length =
            usize::try_from(read_varint(block, &mut position)?).map_err(|_| "值过长")?;
        if shared > previous_key.len() {
            return Err("LevelDB 共享键无效".into());
        }
        let key_end = position.checked_add(unshared).ok_or("LevelDB 键范围溢出")?;
        let value_end = key_end
            .checked_add(value_length)
            .ok_or("LevelDB 值范围溢出")?;
        if value_end > entries_end {
            return Err("LevelDB 条目越界".into());
        }
        let mut key = previous_key[..shared].to_vec();
        key.extend_from_slice(&block[position..key_end]);
        entries.push((key.clone(), block[key_end..value_end].to_vec()));
        previous_key = key;
        position = value_end;
    }
    Ok(entries)
}

fn read_varint(bytes: &[u8], position: &mut usize) -> Result<u64, String> {
    let mut value = 0_u64;
    for shift in (0..70).step_by(7) {
        let byte = *bytes.get(*position).ok_or("Varint 数据不完整")?;
        *position += 1;
        if shift == 63 && byte > 1 {
            return Err("Varint 数值溢出".into());
        }
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err("Varint 过长".into())
}

fn decompress_snappy(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let mut position = 0;
    let expected =
        usize::try_from(read_varint(bytes, &mut position)?).map_err(|_| "Snappy 数据过大")?;
    if expected > 64 * 1024 * 1024 {
        return Err("Snappy 解压大小超过限制".into());
    }
    let mut output = Vec::with_capacity(expected);
    while position < bytes.len() && output.len() < expected {
        let tag = bytes[position];
        position += 1;
        match tag & 0x03 {
            0 => {
                let encoded_length = tag >> 2;
                let length = if encoded_length < 60 {
                    encoded_length as usize + 1
                } else {
                    let extra = encoded_length as usize - 59;
                    if extra > 4 || bytes.len().saturating_sub(position) < extra {
                        return Err("Snappy 字面量长度无效".into());
                    }
                    let mut length = 0_usize;
                    for index in 0..extra {
                        length |= usize::from(bytes[position + index]) << (index * 8);
                    }
                    position += extra;
                    length + 1
                };
                let end = position.checked_add(length).ok_or("Snappy 字面量溢出")?;
                output.extend_from_slice(bytes.get(position..end).ok_or("Snappy 字面量不完整")?);
                position = end;
            }
            1 => {
                let length = 4 + usize::from((tag >> 2) & 0x07);
                let next = *bytes.get(position).ok_or("Snappy COPY_1 不完整")?;
                position += 1;
                let offset = (usize::from(tag & 0xe0) << 3) | usize::from(next);
                copy_snappy(&mut output, offset, length)?;
            }
            2 => {
                let length = 1 + usize::from(tag >> 2);
                let raw = bytes
                    .get(position..position + 2)
                    .ok_or("Snappy COPY_2 不完整")?;
                position += 2;
                copy_snappy(
                    &mut output,
                    u16::from_le_bytes([raw[0], raw[1]]) as usize,
                    length,
                )?;
            }
            3 => {
                let length = 1 + usize::from(tag >> 2);
                let raw = bytes
                    .get(position..position + 4)
                    .ok_or("Snappy COPY_4 不完整")?;
                position += 4;
                let offset =
                    u32::from_le_bytes(raw.try_into().map_err(|_| "Snappy 偏移无效")?) as usize;
                copy_snappy(&mut output, offset, length)?;
            }
            _ => unreachable!(),
        }
    }
    if output.len() != expected {
        return Err("Snappy 解压长度不匹配".into());
    }
    Ok(output)
}

fn copy_snappy(output: &mut Vec<u8>, offset: usize, length: usize) -> Result<(), String> {
    if offset == 0 || offset > output.len() {
        return Err("Snappy 复制偏移无效".into());
    }
    for _ in 0..length {
        let value = output[output.len() - offset];
        output.push(value);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_chromium_local_storage_strings_and_keys() {
        assert_eq!(decode_blink_string(b"\x01user"), Some("user".into()));
        assert_eq!(
            decode_storage_key(b"_https://example.com\0\x01quota_per_unit"),
            Some(("https://example.com".into(), "quota_per_unit".into()))
        );
        let utf16 = [0_u8, b'u', 0, b's', 0, b'e', 0, b'r', 0];
        assert_eq!(decode_blink_string(&utf16), Some("user".into()));
    }

    #[test]
    fn decodes_leveldb_varints_and_snappy_literals() {
        let mut position = 0;
        assert_eq!(read_varint(&[0xac, 0x02], &mut position).unwrap(), 300);
        assert_eq!(
            decompress_snappy(&[5, 16, b'h', b'e', b'l', b'l', b'o']).unwrap(),
            b"hello"
        );
    }

    #[test]
    fn reads_leveldb_write_batch_records() {
        let mut batch = Vec::new();
        batch.extend_from_slice(&7_u64.to_le_bytes());
        batch.extend_from_slice(&1_u32.to_le_bytes());
        batch.push(1);
        batch.push(5);
        batch.extend_from_slice(b"key-1");
        batch.push(6);
        batch.extend_from_slice(b"value1");
        let records = read_write_batch(&batch).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].sequence, 7);
        assert_eq!(records[0].key, b"key-1");
        assert_eq!(records[0].value.as_deref(), Some(b"value1".as_slice()));
    }
}

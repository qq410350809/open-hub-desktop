#[cfg(feature = "desktop")]
use rfd::FileDialog;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;

#[derive(Serialize, Deserialize)]
pub struct SaveFileArgs {
    pub filename: String,
    pub content: String, // Base64 编码的文件内容
}

#[derive(Serialize, Deserialize)]
pub struct SaveFileResult {
    pub path: Option<String>,
    pub cancelled: bool,
}

/// 弹出原生保存对话框，让用户选择保存位置后写入文件。
/// content 为 Base64 编码的字节流，避免二进制数据在 JSON 序列化中损坏。
///
/// 桌面专属：server 形态无原生文件对话框，导出由浏览器下载完成。
#[cfg_attr(feature = "desktop", tauri::command)]
#[cfg(feature = "desktop")]
pub async fn save_export_file(args: SaveFileArgs) -> Result<SaveFileResult, String> {
    use base64::Engine;

    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&args.content)
        .map_err(|e| format!("解码文件内容失败: {e}"))?;

    let file_path = FileDialog::new()
        .set_file_name(&args.filename)
        .save_file();

    let Some(path) = file_path else {
        return Ok(SaveFileResult {
            path: None,
            cancelled: true,
        });
    };

    let mut file = fs::File::create(&path).map_err(|e| format!("创建文件失败: {e}"))?;
    file.write_all(&bytes)
        .map_err(|e| format!("写入文件失败: {e}"))?;

    Ok(SaveFileResult {
        path: Some(path.to_string_lossy().to_string()),
        cancelled: false,
    })
}

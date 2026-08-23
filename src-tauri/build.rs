fn main() {
    // 仅桌面形态需要 Tauri 资源打包；server 形态无窗口资源。
    #[cfg(feature = "desktop")]
    tauri_build::build()
}

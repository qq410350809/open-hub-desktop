//! 菜单栏（托盘）图标：关闭窗口后应用驻留后台时的唯一入口。
//!
//! 行为约定（macOS）：
//! - 关闭主窗口 → 窗口隐藏 + Dock 图标收起，仅保留菜单栏图标（见 lib.rs）；
//! - 左键单击托盘图标 → 显示主窗口并恢复 Dock 图标；
//! - 右键弹出菜单：显示主窗口 / 退出 OpenHub；
//! - 真正退出只走托盘菜单或 Cmd+Q。

use tauri::{
    menu::{Menu, MenuItem},
    tray::{TrayIconBuilder, TrayIconEvent},
    Manager,
};
use tracing::info;

/// 显示主窗口并恢复 Dock 图标（从托盘唤起的统一入口）。
fn show_main_window<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    #[cfg(target_os = "macos")]
    {
        let _ = app.set_dock_visibility(true);
        refresh_dock_icon(app);
    }
    if let Some(window) = app.get_webview_window("main") {
        #[cfg(target_os = "macos")]
        restore_window_chrome(&window);
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// 修复 tao `TransformProcessType`（Dock 隐藏/恢复）留下的窗口副作用：
/// 隐藏 Dock 前所有窗口被 `setCanHide(false)`，转回前台应用后以此状态唤起的窗口
/// 会被 macOS 剥掉交通灯（关闭按钮）。唤起前把 canHide 置回并强制标题栏重绘。
#[cfg(target_os = "macos")]
fn restore_window_chrome<R: tauri::Runtime>(window: &tauri::WebviewWindow<R>) {
    use objc2_app_kit::{NSApplication, NSWindowButton};
    use objc2_foundation::MainThreadMarker;

    let Ok(ns_window_ptr) = window.ns_window() else {
        return;
    };
    let ns_window = unsafe { &*(ns_window_ptr as *const objc2_app_kit::NSWindow) };

    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);

    // setCanHide 置回 true；standardWindowButton 触发 AppKit 重建交通灯，
    // styleMask 原值重写强制标题栏失效重绘。
    ns_window.setCanHide(true);
    let style_mask = ns_window.styleMask();
    ns_window.setStyleMask(style_mask);
    let _ = ns_window.standardWindowButton(NSWindowButton::CloseButton);
    let _ = ns_window.standardWindowButton(NSWindowButton::MiniaturizeButton);
    let _ = ns_window.standardWindowButton(NSWindowButton::ZoomButton);
    // Dock 恢复后 AppKit 可能遗留 UIElement 期的空菜单栏，重新激活以还原菜单栏归属。
    app.activate();
}

/// Dock 图标变形（TransformProcessType 快速切换的缓存残影）的补救：
/// 用打包内应用图标重建 NSImage 并重设，促使 Dock 重绘图标帧。
#[cfg(target_os = "macos")]
fn refresh_dock_icon<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    use objc2::AnyThread;
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::{NSData, MainThreadMarker};

    // tauri 的 default_window_icon 拿到的是裸 RGBA 像素，NSImage 不认；
    // 直接解码打包内 PNG（与托盘图标回退同源）。
    let png: &NSData = &NSData::with_bytes(include_bytes!("../../icons/32x32.png"));
    let Some(image) = NSImage::initWithData(NSImage::alloc(), png) else {
        return;
    };
    let _ = app;
    unsafe {
        NSApplication::sharedApplication(MainThreadMarker::new_unchecked())
            .setApplicationIconImage(Some(&image));
    }
}

/// 安装菜单栏图标（幂等：重复调用会创建多个图标，setup 只调用一次）。
pub fn install_tray<R: tauri::Runtime>(app: &tauri::App<R>) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "tray-show", "显示主窗口", true, None::<&str>)?;
    let sep = tauri::menu::PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "tray-quit", "退出 OpenHub", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &sep, &quit])?;

    // 图标：优先复用应用默认图标（tauri.conf.json icons），缺失时回退打包内 32x32 PNG。
    let icon = app.default_window_icon().cloned().or_else(|| {
        tauri::image::Image::from_bytes(include_bytes!("../../icons/32x32.png")).ok()
    });

    let mut builder = TrayIconBuilder::with_id("main-tray")
        .tooltip("OpenHub — 点击图标显示主窗口")
        .menu(&menu)
        // 左键留给「单击显示主窗口」，菜单固定由右键弹出。
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "tray-show" => show_main_window(app),
            "tray-quit" => {
                info!("[OpenHub] 托盘菜单触发退出");
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button, .. } = event {
                if button == tauri::tray::MouseButton::Left {
                    show_main_window(tray.app_handle());
                }
            }
        });
    if let Some(icon) = icon {
        builder = builder.icon(icon);
    }

    builder.build(app)?;
    Ok(())
}


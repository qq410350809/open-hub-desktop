//! 中文原生菜单。
//! 1) 覆盖 macOS 菜单栏；
//! 2) 编辑菜单中文项供系统快捷键/部分文本菜单复用；
//! 3) 页面内右键由前端中文菜单接管（WKWebView 默认英文菜单不可完全本地化）。

use tauri::menu::{AboutMetadata, IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::Manager;

type DynItem<'a, R> = &'a dyn IsMenuItem<R>;

/// 安装中文菜单到应用与主窗口。
pub fn install_chinese_menu<R: tauri::Runtime>(app: &tauri::App<R>) -> tauri::Result<()> {
    // macOS 会把菜单栏第一个子菜单固定显示为应用名，标题写什么都会被系统替换。
    let about =
        PredefinedMenuItem::about(app, Some("关于 OpenHub"), Some(AboutMetadata::default())).ok();
    let sep = PredefinedMenuItem::separator(app).ok();
    let services = PredefinedMenuItem::services(app, Some("服务")).ok();
    let hide = PredefinedMenuItem::hide(app, Some("隐藏 OpenHub")).ok();
    let hide_others = PredefinedMenuItem::hide_others(app, Some("隐藏其他")).ok();
    let show_all = PredefinedMenuItem::show_all(app, Some("全部显示")).ok();
    let quit = PredefinedMenuItem::quit(app, Some("退出 OpenHub")).ok();
    let app_items: Vec<DynItem<'_, R>> = vec![
        about.as_ref().map(|i| i as DynItem<'_, R>),
        sep.as_ref().map(|i| i as DynItem<'_, R>),
        services.as_ref().map(|i| i as DynItem<'_, R>),
        sep.as_ref().map(|i| i as DynItem<'_, R>),
        hide.as_ref().map(|i| i as DynItem<'_, R>),
        hide_others.as_ref().map(|i| i as DynItem<'_, R>),
        show_all.as_ref().map(|i| i as DynItem<'_, R>),
        sep.as_ref().map(|i| i as DynItem<'_, R>),
        quit.as_ref().map(|i| i as DynItem<'_, R>),
    ]
    .into_iter()
    .flatten()
    .collect();
    let app_menu = Submenu::with_items(app, "OpenHub", true, &app_items)?;

    let close_window = PredefinedMenuItem::close_window(app, Some("关闭窗口")).ok();
    let refresh_item = MenuItem::with_id(app, "file-refresh", "刷新", true, Some("CmdOrCtrl+R"))
        .expect("创建文件菜单刷新项失败");
    let sep_file = PredefinedMenuItem::separator(app).ok();
    let file_items: Vec<DynItem<'_, R>> = vec![
        close_window.as_ref().map(|i| i as DynItem<'_, R>),
        sep_file.as_ref().map(|i| i as DynItem<'_, R>),
        Some(&refresh_item as DynItem<'_, R>),
    ]
    .into_iter()
    .flatten()
    .collect();
    let file_menu = Submenu::with_items(app, "文件", true, &file_items)?;

    // 编辑菜单是文本输入区域系统菜单/快捷键的数据来源之一。
    let undo = PredefinedMenuItem::undo(app, Some("撤销")).ok();
    let redo = PredefinedMenuItem::redo(app, Some("重做")).ok();
    let cut = PredefinedMenuItem::cut(app, Some("剪切")).ok();
    let copy = PredefinedMenuItem::copy(app, Some("拷贝")).ok();
    let paste = PredefinedMenuItem::paste(app, Some("粘贴")).ok();
    let select_all = PredefinedMenuItem::select_all(app, Some("全选")).ok();
    let edit_items: Vec<DynItem<'_, R>> = vec![
        undo.as_ref().map(|i| i as DynItem<'_, R>),
        redo.as_ref().map(|i| i as DynItem<'_, R>),
        sep.as_ref().map(|i| i as DynItem<'_, R>),
        cut.as_ref().map(|i| i as DynItem<'_, R>),
        copy.as_ref().map(|i| i as DynItem<'_, R>),
        paste.as_ref().map(|i| i as DynItem<'_, R>),
        select_all.as_ref().map(|i| i as DynItem<'_, R>),
    ]
    .into_iter()
    .flatten()
    .collect();
    let edit_menu = Submenu::with_items(app, "编辑", true, &edit_items)?;

    let fullscreen = PredefinedMenuItem::fullscreen(app, Some("进入全屏")).ok();
    let view_items: Vec<DynItem<'_, R>> = vec![fullscreen.as_ref().map(|i| i as DynItem<'_, R>)]
        .into_iter()
        .flatten()
        .collect();
    let view_menu = Submenu::with_items(app, "视图", true, &view_items)?;

    let minimize = PredefinedMenuItem::minimize(app, Some("最小化")).ok();
    let maximize = PredefinedMenuItem::maximize(app, Some("缩放")).ok();
    let window_items: Vec<DynItem<'_, R>> = vec![
        minimize.as_ref().map(|i| i as DynItem<'_, R>),
        maximize.as_ref().map(|i| i as DynItem<'_, R>),
    ]
    .into_iter()
    .flatten()
    .collect();
    let window_menu = Submenu::with_items(app, "窗口", true, &window_items)?;

    let menu = Menu::with_items(
        app,
        &[&app_menu, &file_menu, &edit_menu, &view_menu, &window_menu],
    )?;
    app.set_menu(menu.clone())?;

    // 同步挂到主窗口，避免部分场景只读窗口菜单。
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.set_menu(menu);
    }

    Ok(())
}

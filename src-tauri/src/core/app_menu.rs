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
    // 关于面板：填充 macOS 支持的字段（name/version/copyright/credits），
    // 版本号编译期取自 Cargo.toml，与 tauri.conf.json 由 check:version 保证一致。
    let about = PredefinedMenuItem::about(
        app,
        Some("关于 OpenHub"),
        Some(AboutMetadata {
            name: Some("OpenHub".into()),
            version: Some(env!("CARGO_PKG_VERSION").into()),
            copyright: Some("© 2026 OpenHub".into()),
            credits: Some("本地站点资料库".into()),
            ..AboutMetadata::default()
        }),
    )
    .ok();
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

    // —— 文件菜单：新建 / 刷新 / 导出（参照常见 macOS 应用布局）——
    let new_site = MenuItem::with_id(app, "file-new-site", "新建站点…", true, Some("CmdOrCtrl+N"))
        .expect("创建文件菜单新建项失败");
    let refresh_item =
        MenuItem::with_id(app, "file-refresh", "全部刷新", true, Some("CmdOrCtrl+R"))
            .expect("创建文件菜单刷新项失败");
    let export_data = MenuItem::with_id(app, "file-export", "导出数据…", true, Some("CmdOrCtrl+E"))
        .expect("创建文件菜单导出项失败");
    let sep_file = PredefinedMenuItem::separator(app).ok();
    let file_items: Vec<DynItem<'_, R>> = vec![
        Some(&new_site as DynItem<'_, R>),
        sep_file.as_ref().map(|i| i as DynItem<'_, R>),
        Some(&refresh_item as DynItem<'_, R>),
        sep_file.as_ref().map(|i| i as DynItem<'_, R>),
        Some(&export_data as DynItem<'_, R>),
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

    // —— 视图菜单：页面导航 + 刷新页面 + 全屏 ——
    let nav_defs: [(&str, &str, &str); 8] = [
        ("nav-tokenstats", "本地统计", "CmdOrCtrl+1"),
        ("nav-gatewaystats", "网关统计", "CmdOrCtrl+2"),
        ("nav-library", "站点库", "CmdOrCtrl+3"),
        ("nav-modelparams", "模型参数", "CmdOrCtrl+4"),
        ("nav-modelproxy", "模型网关", "CmdOrCtrl+5"),
        ("nav-charity", "公益监听", "CmdOrCtrl+6"),
        ("nav-proxy", "代理池", "CmdOrCtrl+7"),
        ("nav-settings", "设置…", "CmdOrCtrl+,"),
    ];
    let mut nav_items: Vec<Box<dyn IsMenuItem<R>>> = Vec::new();
    for (id, text, accelerator) in nav_defs {
        nav_items.push(Box::new(
            MenuItem::with_id(app, id, text, true, Some(accelerator))
                .expect("创建视图菜单导航项失败"),
        ));
    }
    let reload_page = MenuItem::with_id(
        app,
        "view-reload",
        "刷新页面",
        true,
        Some("CmdOrCtrl+Shift+R"),
    )
    .expect("创建视图菜单刷新页面项失败");
    let fullscreen = PredefinedMenuItem::fullscreen(app, Some("进入全屏")).ok();
    let sep_view = PredefinedMenuItem::separator(app).ok();
    let mut view_refs: Vec<DynItem<'_, R>> = nav_items
        .iter()
        .map(|item| item.as_ref() as DynItem<'_, R>)
        .collect();
    for optional in [
        Some(&reload_page as DynItem<'_, R>),
        sep_view.as_ref().map(|i| i as DynItem<'_, R>),
        fullscreen.as_ref().map(|i| i as DynItem<'_, R>),
    ] {
        if let Some(item) = optional {
            view_refs.push(item);
        }
    }
    let view_menu = Submenu::with_items(app, "视图", true, &view_refs)?;

    let minimize = PredefinedMenuItem::minimize(app, Some("最小化")).ok();
    let maximize = PredefinedMenuItem::maximize(app, Some("缩放")).ok();
    let close_window = PredefinedMenuItem::close_window(app, Some("关闭窗口")).ok();
    let sep_window = PredefinedMenuItem::separator(app).ok();
    let window_items: Vec<DynItem<'_, R>> = vec![
        minimize.as_ref().map(|i| i as DynItem<'_, R>),
        maximize.as_ref().map(|i| i as DynItem<'_, R>),
        sep_window.as_ref().map(|i| i as DynItem<'_, R>),
        close_window.as_ref().map(|i| i as DynItem<'_, R>),
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

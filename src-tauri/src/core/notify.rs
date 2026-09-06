//! 系统通知：macOS 走现代 UNUserNotificationCenter，其余桌面平台走 tauri-plugin-notification。
//!
//! 为什么 macOS 不直接用 tauri-plugin-notification：它经 notify-rust 落到已废弃的
//! NSUserNotification API——应用前台时系统不弹横幅、dev 模式挂名 Terminal 导致
//! 通知被静默丢弃（历史记录为空）。UNUserNotificationCenter 只在主线程可用，
//! 有真实授权流程、前台横幅（willPresent + Banner）、正常的通知中心历史。

#[cfg(target_os = "macos")]
mod imp {
    use objc2::rc::Retained;
    use objc2::runtime::{Bool, NSObject, NSObjectProtocol, ProtocolObject};
    use objc2::{define_class, MainThreadMarker, MainThreadOnly};
    use objc2_foundation::{NSBundle, NSError, NSString};
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNNotification, UNMutableNotificationContent,
        UNNotificationPresentationOptions, UNNotificationRequest, UNNotificationSound,
        UNUserNotificationCenter, UNUserNotificationCenterDelegate,
    };
    use std::sync::OnceLock;

    // 前台横幅 delegate：只实现 willPresent，返回 Banner + Sound，
    // 应用在前台时也弹横幅（这正是旧 NSUserNotification API 做不到的）。
    // UNUserNotificationCenter 对 delegate 是弱引用，必须由本模块静态持有。
    define_class!(
        // SAFETY: NSObject 无子类化要求，PresentDelegate 不实现 Drop。
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        struct PresentDelegate;

        // SAFETY: NSObjectProtocol 无安全要求。
        unsafe impl NSObjectProtocol for PresentDelegate {}

        // SAFETY: 签名与 UNUserNotificationCenterDelegate 声明一致。
        unsafe impl UNUserNotificationCenterDelegate for PresentDelegate {
            // SAFETY: 选择器与签名一致。
            #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
            fn will_present(
                &self,
                _center: &UNUserNotificationCenter,
                _notification: &UNNotification,
                completion_handler: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
            ) {
                completion_handler.call((
                    UNNotificationPresentationOptions::Banner
                        | UNNotificationPresentationOptions::Sound,
                ));
            }
        }
    );

    // ProtocolObject<dyn Trait> 的 Send/Sync 跟随具体实现类型；delegate 只在主线程
    // 触碰，这里由我们保证，用一个显式 newtype 承诺。
    struct DelegateHandle(Retained<ProtocolObject<dyn UNUserNotificationCenterDelegate>>);
    unsafe impl Send for DelegateHandle {}
    unsafe impl Sync for DelegateHandle {}

    static PRESENT_DELEGATE: OnceLock<DelegateHandle> = OnceLock::new();

    impl PresentDelegate {
        fn new(mtm: MainThreadMarker) -> Retained<Self> {
            let this = Self::alloc(mtm);
            // SAFETY: NSObject 的 init 选择器签名正确。
            unsafe { objc2::msg_send![this, init] }
        }
    }

    fn ns_error_message(error: *mut NSError) -> Option<String> {
        if error.is_null() {
            return None;
        }
        // SAFETY: 指针来自系统回调，非空时有效。
        let error = unsafe { &*error };
        Some(error.localizedDescription().to_string())
    }

    /// 发送系统通知。返回 Err 为失败原因（前端 toast 展示），Ok(()) 表示已提交投递。
    pub fn send_system_notification(title: &str, body: &str) -> Result<(), String> {
        let marker = MainThreadMarker::new().ok_or("通知必须在主线程发送")?;
        // 未打包的开发进程没有 bundle，系统必然无法投递（这正是 dev 模式下旧 API
        // 静默丢通知、历史为空的原因），如实上报而不是假装成功。
        let bundle_id = NSBundle::mainBundle()
            .bundleIdentifier()
            .map(|id| id.to_string())
            .unwrap_or_default();
        if bundle_id.is_empty() {
            return Err(
                "当前是未打包的开发进程，系统无法投递通知；请用打包后的 OpenHub.app 验证"
                    .to_string(),
            );
        }

        let center = UNUserNotificationCenter::currentNotificationCenter();

        // 授权（同步等待回调；首启会弹系统授权框，拒绝/失败都如实上报）
        let options = UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound;
        let (granted, auth_error) = {
            let (sender, receiver) = std::sync::mpsc::channel::<(Bool, Option<String>)>();
            let completion =
                block2::RcBlock::new(move |granted: Bool, error: *mut NSError| {
                    let _ = sender.send((granted, ns_error_message(error)));
                });
            // SAFETY: block 与参数类型和系统声明一致。
            center.requestAuthorizationWithOptions_completionHandler(options, &completion);
            match receiver.recv_timeout(std::time::Duration::from_secs(10)) {
                Ok((granted, error)) => (bool::from(granted), error),
                Err(_) => (false, Some("授权请求超时".to_string())),
            }
        };
        if !granted {
            return Err(match auth_error {
                Some(message) => format!("系统通知授权失败：{message}"),
                None => "系统通知权限未授权，请在 系统设置 → 通知 → OpenHub 里允许".to_string(),
            });
        }

        // 装前台横幅 delegate（OnceLock 保证只装一次；Retained 存在静态里）
        let delegate = &PRESENT_DELEGATE
            .get_or_init(|| DelegateHandle(ProtocolObject::from_retained(PresentDelegate::new(marker))))
            .0;
        center.setDelegate(Some(delegate));

        // UNMutableNotificationContent 是任意线程类（非 MainThreadOnly），用 new()
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(title));
        content.setBody(&NSString::from_str(body));
        content.setSound(Some(&UNNotificationSound::defaultSound()));

        let identifier = NSString::from_str(&format!(
            "openhub-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &identifier,
            &content,
            None,
        );

        let send_error = {
            let (sender, receiver) = std::sync::mpsc::channel::<Option<String>>();
            let completion = block2::RcBlock::new(move |error: *mut NSError| {
                let _ = sender.send(ns_error_message(error));
            });
            // SAFETY: block 与参数类型和系统声明一致。
            center.addNotificationRequest_withCompletionHandler(&request, Some(&completion));
            // 回调未达（超时）也视为已受理：横幅由系统异步弹出
            receiver
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap_or(None)
        };
        if let Some(message) = send_error {
            return Err(format!("系统通知投递失败：{message}"));
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
pub use imp::send_system_notification;

#[cfg(not(target_os = "macos"))]
pub fn send_system_notification(title: &str, body: &str) -> Result<(), String> {
    // 非 macOS 桌面平台保持原有 tauri 插件通道（Windows/Linux 无上述问题）
    tauri_plugin_notification::NotificationExt::notification(&tauri::AppHandle::any_thread())
        .builder()
        .title(title)
        .body(body)
        .show()
        .map_err(|error| format!("发送系统通知失败：{error}"))
}

/// Tauri 命令入口：前端经 run_command("send_system_notification") 调用。
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn send_system_notification_command(
    app: tauri::AppHandle,
    title: String,
    body: String,
) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        // UNUserNotificationCenter 只能在主线程访问，而 async 命令跑在 worker
        // 线程上，所以把发送派发回主线程并同步等结果。
        let (sender, receiver) = std::sync::mpsc::channel::<Result<(), String>>();
        let result = app.run_on_main_thread(move || {
            let _ = sender.send(send_system_notification(&title, &body));
        });
        if let Err(error) = result {
            return Err(format!("通知任务派发失败：{error}"));
        }
        receiver
            .recv_timeout(std::time::Duration::from_secs(25))
            .map_err(|_| "通知任务超时".to_string())?
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        send_system_notification(&title, &body)
    }
}

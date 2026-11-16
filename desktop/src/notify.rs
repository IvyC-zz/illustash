//! 系统通知和后台运行时对系统的声明（#451）。
//!
//! - macOS：UNUserNotificationCenter。第一次转到后台时请求权限（`prepare`），系统弹它自己的询问框，只问一次；被拒绝
//!   就发不出去，其余照旧。点通知由代理对象转给 `background::open_tasks`。只有从 .app 包里运行时才有通知中心，
//!   `cargo run` 直接跑的程序没有，那时什么都不做。有任务在跑时向系统声明「用户发起的活动」，免得 App Nap 拖慢
//!   Pixiv 登录窗口里的请求；不阻止系统睡眠。
//! - Windows：WinRT 的 toast，按应用 ID（tauri.conf.json 的 identifier，安装包给开始菜单快捷方式设了同一个 ID）发。
//!   不需要请求权限；系统里关掉了通知就收不到。点通知由 toast 的 Activated 事件转给 `background::open_tasks`。
//!   没装、直接跑的开发构建没有这个快捷方式，toast 不显示。
use tauri::AppHandle;

pub fn send(app: &AppHandle, title: &str, body: &str) {
    let (handle, title, body) = (app.clone(), title.to_owned(), body.to_owned());
    // 两个平台的通知接口都要在主线程上用（WinRT 要求初始化过的单元线程，代理对象在主线程上设）。
    let _ = app.run_on_main_thread(move || platform::send(&handle, &title, &body));
}

/// 第一次转到后台时调用：macOS 请求通知权限。
pub fn prepare(app: &AppHandle) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || platform::prepare(&handle));
}

/// 有任务在跑时向系统声明（macOS 的 App Nap）；别的平台什么都不做。
pub fn keep_awake(app: &AppHandle, busy: bool) {
    let _ = app.run_on_main_thread(move || platform::keep_awake(busy));
}

#[cfg(target_os = "macos")]
mod platform {
    use std::cell::{Cell, RefCell};
    use std::sync::OnceLock;

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::{Bool, NSObjectProtocol, ProtocolObject};
    use objc2::{define_class, msg_send, AllocAnyThread};
    use objc2_foundation::{NSActivityOptions, NSBundle, NSError, NSObject, NSProcessInfo, NSString};
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNMutableNotificationContent, UNNotification, UNNotificationPresentationOptions,
        UNNotificationRequest, UNNotificationResponse, UNUserNotificationCenter, UNUserNotificationCenterDelegate,
    };
    use tauri::AppHandle;

    static APP: OnceLock<AppHandle> = OnceLock::new();

    thread_local! {
        static ASKED: Cell<bool> = const { Cell::new(false) };
        static DELEGATE: RefCell<Option<Retained<Delegate>>> = const { RefCell::new(None) };
        static ACTIVITY: RefCell<Option<Retained<ProtocolObject<dyn NSObjectProtocol>>>> = const { RefCell::new(None) };
        static COUNTER: Cell<u64> = const { Cell::new(0) };
    }

    define_class!(
        // SAFETY: NSObject 没有子类化的额外要求；这个类不覆盖 dealloc，也没有实例变量。
        #[unsafe(super(NSObject))]
        #[name = "IllustashNotificationDelegate"]
        struct Delegate;

        unsafe impl NSObjectProtocol for Delegate {}

        unsafe impl UNUserNotificationCenterDelegate for Delegate {
            // App 在前台（比如窗口隐藏着、用户在用 Dock 菜单）时也显示横幅。
            #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
            fn will_present(
                &self,
                _center: &UNUserNotificationCenter,
                _notification: &UNNotification,
                handler: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
            ) {
                handler.call((UNNotificationPresentationOptions::Banner | UNNotificationPresentationOptions::List,));
            }

            #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
            fn did_receive(
                &self,
                _center: &UNUserNotificationCenter,
                _response: &UNNotificationResponse,
                handler: &block2::DynBlock<dyn Fn()>,
            ) {
                if let Some(app) = APP.get() {
                    crate::background::open_tasks(app);
                }
                handler.call(());
            }
        }
    );

    impl Delegate {
        fn new() -> Retained<Self> {
            let this = Self::alloc().set_ivars(());
            // SAFETY: NSObject 的 init 总是可以调用，返回同一个对象。
            unsafe { msg_send![super(this), init] }
        }
    }

    /// 只有从 .app 包里运行时才有通知中心；否则 `currentNotificationCenter` 会抛异常。
    fn center() -> Option<Retained<UNUserNotificationCenter>> {
        let bundle = NSBundle::mainBundle();
        bundle.bundleIdentifier()?;
        if !bundle.bundlePath().to_string().ends_with(".app") {
            return None;
        }
        let center = UNUserNotificationCenter::currentNotificationCenter();
        DELEGATE.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.is_none() {
                let delegate = Delegate::new();
                center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
                // 通知中心只弱引用代理对象：留在这里，App 活多久它就活多久。
                *slot = Some(delegate);
            }
        });
        Some(center)
    }

    pub fn prepare(app: &AppHandle) {
        let _ = APP.set(app.clone());
        if ASKED.replace(true) {
            return;
        }
        let Some(center) = center() else { return };
        let done = RcBlock::new(|granted: Bool, _error: *mut NSError| {
            if !granted.as_bool() {
                eprintln!("[notify] 没有通知权限");
            }
        });
        center.requestAuthorizationWithOptions_completionHandler(
            UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
            &done,
        );
    }

    pub fn send(app: &AppHandle, title: &str, body: &str) {
        let _ = APP.set(app.clone());
        let Some(center) = center() else { return };
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(title));
        content.setBody(&NSString::from_str(body));
        let id = COUNTER.with(|c| {
            c.set(c.get() + 1);
            c.get()
        });
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &NSString::from_str(&format!("illustash-{id}")),
            &content,
            None,
        );
        center.addNotificationRequest_withCompletionHandler(&request, None);
    }

    pub fn keep_awake(busy: bool) {
        ACTIVITY.with(|slot| {
            let mut slot = slot.borrow_mut();
            match (busy, slot.is_some()) {
                (true, false) => {
                    let reason = NSString::from_str("Illustash 后台任务");
                    *slot = Some(NSProcessInfo::processInfo().beginActivityWithOptions_reason(
                        NSActivityOptions::UserInitiatedAllowingIdleSystemSleep,
                        &reason,
                    ));
                }
                (false, true) => {
                    if let Some(token) = slot.take() {
                        // SAFETY: token 是上面 beginActivityWithOptions:reason: 返回的，只结束一次。
                        unsafe { NSProcessInfo::processInfo().endActivity(&token) };
                    }
                }
                _ => {}
            }
        });
    }
}

#[cfg(windows)]
mod platform {
    use tauri::{AppHandle, Manager};
    use windows::core::HSTRING;
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::Foundation::TypedEventHandler;
    use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};

    fn escape(text: &str) -> String {
        text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
    }

    fn toast(app: &AppHandle, title: &str, body: &str) -> windows::core::Result<()> {
        let xml = XmlDocument::new()?;
        let text = if body.is_empty() { String::new() } else { format!("<text>{}</text>", escape(body)) };
        xml.LoadXml(&HSTRING::from(format!(
            "<toast><visual><binding template=\"ToastGeneric\"><text>{}</text>{text}</binding></visual></toast>",
            escape(title)
        )))?;
        let notification = ToastNotification::CreateToastNotification(&xml)?;
        let handle = app.clone();
        notification.Activated(&TypedEventHandler::new(move |_, _| {
            crate::background::open_tasks(&handle);
            Ok(())
        }))?;
        let id = HSTRING::from(app.config().identifier.as_str());
        ToastNotificationManager::CreateToastNotifierWithId(&id)?.Show(&notification)
    }

    pub fn send(app: &AppHandle, title: &str, body: &str) {
        if let Err(error) = toast(app, title, body) {
            eprintln!("[notify] 发不出通知：{error}");
        }
    }

    pub fn prepare(_app: &AppHandle) {}

    pub fn keep_awake(_busy: bool) {}
}

#[cfg(not(any(target_os = "macos", windows)))]
mod platform {
    use tauri::AppHandle;

    pub fn send(_app: &AppHandle, _title: &str, _body: &str) {}

    pub fn prepare(_app: &AppHandle) {}

    pub fn keep_awake(_busy: bool) {}
}

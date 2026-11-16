//! 把图拖到别的应用（#450）：在主窗口上起系统的拖放，拖的是文件。只允许「复制」：拖进访达或资源管理器只能复制，
//! 不能移动，原图不会被挪走。路径由宿主按资产 ID 从图库里查出来（bridge.rs 的 `/platform/files/drag`）。
//! 必须在主线程上调用，而且要在页面上的鼠标还按着的时候：页面在按下后移动了几像素才发请求，外壳排到主线程上立即开始。
use std::path::PathBuf;

/// 在 `window` 上开始拖 `paths`。`icon` 是拖动时显示的小图（缩略图缓存），没有时用系统给这种文件的图标。
#[cfg(target_os = "macos")]
pub fn start(window: &tauri::WebviewWindow, paths: Vec<PathBuf>, icon: Option<PathBuf>) -> Result<(), String> {
    let pointer = window.ns_window().map_err(|e| e.to_string())? as usize;
    window.run_on_main_thread(move || mac::start(pointer, &paths, icon.as_deref())).map_err(|e| e.to_string())
}

#[cfg(windows)]
pub fn start(window: &tauri::WebviewWindow, paths: Vec<PathBuf>, _icon: Option<PathBuf>) -> Result<(), String> {
    let hwnd = window.hwnd().map_err(|e| e.to_string())?.0 as usize;
    window.run_on_main_thread(move || win::start(hwnd, &paths)).map_err(|e| e.to_string())
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn start(_: &tauri::WebviewWindow, _: Vec<PathBuf>, _: Option<PathBuf>) -> Result<(), String> {
    Err("这个平台不支持拖出".into())
}

#[cfg(target_os = "macos")]
mod mac {
    use std::{cell::RefCell, path::Path, path::PathBuf};

    use objc2::{
        define_class, msg_send,
        rc::Retained,
        runtime::{NSObject, NSObjectProtocol, ProtocolObject},
        MainThreadMarker, MainThreadOnly,
    };
    use objc2_app_kit::{
        NSApplication, NSDragOperation, NSDraggingContext, NSDraggingItem, NSDraggingSession, NSDraggingSource,
        NSEvent, NSEventModifierFlags, NSEventType, NSImage, NSWindow, NSWorkspace,
    };
    use objc2_foundation::{NSArray, NSPoint, NSRect, NSSize, NSString, NSURL};

    define_class!(
        // SAFETY: NSObject 没有子类化的额外要求；这个类不覆盖 dealloc。
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "IllustashDragSource"]
        struct Source;

        // SAFETY: NSObjectProtocol 没有额外要求。
        unsafe impl NSObjectProtocol for Source {}

        // SAFETY: 方法签名与 NSDraggingSource 协议一致。
        unsafe impl NSDraggingSource for Source {
            /// 不论拖到哪里（本应用内外）都只给「复制」。
            #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
            fn operations(&self, _session: &NSDraggingSession, _context: NSDraggingContext) -> NSDragOperation {
                NSDragOperation::Copy
            }
        }
    );

    thread_local! {
        /// 拖放进行期间留着拖放源；下一次拖动时换掉。
        static SOURCE: RefCell<Option<Retained<Source>>> = const { RefCell::new(None) };
    }

    /// 拖动时显示的图最长边（点）。
    const ICON: f64 = 96.0;

    pub(super) fn start(window: usize, paths: &[PathBuf], icon: Option<&Path>) {
        let Some(mtm) = MainThreadMarker::new() else { return };
        // SAFETY: `window` 是主窗口的 NSWindow 指针（tauri 的 ns_window），主窗口在应用运行期间一直存在；这里在主线程上。
        let window: &NSWindow = unsafe { &*(window as *const NSWindow) };
        let Some(view) = window.contentView() else { return };
        let image = icon
            .and_then(|p| NSImage::initWithContentsOfFile(NSImage::alloc(), &NSString::from_str(&p.to_string_lossy())))
            .unwrap_or_else(|| {
                NSWorkspace::sharedWorkspace()
                    .iconForFile(&NSString::from_str(&paths.first().map(|p| p.to_string_lossy()).unwrap_or_default()))
            });
        let size = image.size();
        let scale = ICON / size.width.max(size.height).max(1.0);
        let frame = NSSize::new(size.width * scale, size.height * scale);
        let location = window.mouseLocationOutsideOfEventStream();
        let items: Vec<Retained<NSDraggingItem>> = paths
            .iter()
            .enumerate()
            .map(|(i, path)| {
                let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
                let item =
                    NSDraggingItem::initWithPasteboardWriter(NSDraggingItem::alloc(), ProtocolObject::from_ref(&*url));
                // 几张图叠在鼠标下面，后面的稍微错开。
                let offset = (i.min(4) as f64) * 4.0;
                let origin =
                    NSPoint::new(location.x - frame.width / 2.0 + offset, location.y - frame.height / 2.0 - offset);
                // SAFETY: contents 是 NSImage，NSDraggingItem 的文档允许用它作拖动时显示的内容。
                unsafe { item.setDraggingFrame_contents(NSRect::new(origin, frame), Some(&image)) };
                item
            })
            .collect();
        if items.is_empty() {
            return;
        }
        // 用页面上还按着的那次拖动事件；排到主线程时当前事件可能已经是别的，就照它补一个同一窗口的拖动事件。
        let current = NSApplication::sharedApplication(mtm).currentEvent();
        let event = current
            .clone()
            .filter(|e| matches!(e.r#type(), NSEventType::LeftMouseDragged | NSEventType::LeftMouseDown))
            .or_else(|| {
                NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
                    NSEventType::LeftMouseDragged,
                    location,
                    NSEventModifierFlags::empty(),
                    current.map(|e| e.timestamp()).unwrap_or(0.0),
                    window.windowNumber(),
                    None,
                    0,
                    1,
                    1.0,
                )
            });
        let Some(event) = event else { return };
        // SAFETY: 在主线程上分配，init 是 NSObject 的指定初始化方法。
        let source: Retained<Source> = unsafe { msg_send![super(Source::alloc(mtm).set_ivars(())), init] };
        let items = NSArray::from_retained_slice(&items);
        view.beginDraggingSessionWithItems_event_source(&items, &event, ProtocolObject::from_ref(&*source));
        SOURCE.with(|slot| *slot.borrow_mut() = Some(source));
    }
}

#[cfg(windows)]
mod win {
    use std::{os::windows::ffi::OsStrExt, path::PathBuf};

    use windows::{
        core::PCWSTR,
        Win32::{
            Foundation::HWND,
            System::{
                Com::IDataObject,
                Ole::{IDropSource, OleInitialize, OleUninitialize, DROPEFFECT_COPY},
            },
            UI::Shell::{
                BHID_DataObject, Common::ITEMIDLIST, ILCreateFromPathW, ILFree, SHCreateShellItemArrayFromIDLists,
                SHDoDragDrop,
            },
        },
    };

    pub(super) fn start(hwnd: usize, paths: &[PathBuf]) {
        if let Err(error) = drag(HWND(hwnd as *mut _), paths) {
            eprintln!("[drag] 拖出失败：{error}");
        }
    }

    fn drag(hwnd: HWND, paths: &[PathBuf]) -> windows::core::Result<()> {
        // SAFETY: 在主线程上；OleInitialize 与下面的 OleUninitialize 配对（已初始化过时返回 S_FALSE，也要配对）。
        unsafe { OleInitialize(None)? };
        let mut lists: Vec<*mut ITEMIDLIST> = Vec::with_capacity(paths.len());
        let result = (|| {
            for path in paths {
                let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
                // SAFETY: wide 以 NUL 结尾，调用期间有效；返回的 ID 列表在下面统一用 ILFree 释放。
                let list = unsafe { ILCreateFromPathW(PCWSTR(wide.as_ptr())) };
                if list.is_null() {
                    return Err(windows::core::Error::from_thread());
                }
                lists.push(list);
            }
            let refs: Vec<*const ITEMIDLIST> = lists.iter().map(|l| *l as *const ITEMIDLIST).collect();
            // SAFETY: refs 里都是上面拿到的完整 ID 列表，调用期间有效。
            let items = unsafe { SHCreateShellItemArrayFromIDLists(&refs)? };
            // 系统自带的数据对象：带 CF_HDROP 和外壳 ID 列表，拖动时的缩略图也由系统生成。
            // SAFETY: items 是有效的 IShellItemArray。
            let data: IDataObject = unsafe { items.BindToHandler(None, &BHID_DataObject)? };
            // 只允许复制。SHDoDragDrop 自己提供拖放源，在鼠标松开或取消之前不返回（期间照常分发窗口消息）。
            // SAFETY: hwnd 是主窗口；data 有效。
            let effect = unsafe { SHDoDragDrop(Some(hwnd), &data, None::<&IDropSource>, DROPEFFECT_COPY)? };
            eprintln!("[drag] 结束，效果 {}", effect.0);
            Ok(())
        })();
        for list in lists {
            // SAFETY: 每个列表来自 ILCreateFromPathW，只释放这一次。
            unsafe { ILFree(Some(list as *const ITEMIDLIST)) };
        }
        // SAFETY: 与上面成功的 OleInitialize 配对。
        unsafe { OleUninitialize() };
        result
    }
}

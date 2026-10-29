//! File-based generic passwords, compatible with the former `security` CLI.
//! No subprocess or environment override participates in native keychain access.
use std::{ffi::c_void, ptr};
type Ref = *const c_void;
const NOT_FOUND: i32 = -25300;
const DUPLICATE: i32 = -25299;
const PARAM: i32 = -50;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(value: Ref);
    fn CFStringCreateWithBytes(allocator: Ref, bytes: *const u8, length: isize, encoding: u32, external: u8) -> Ref;
    fn CFDataCreate(allocator: Ref, bytes: *const u8, length: isize) -> Ref;
    fn CFDataGetLength(data: Ref) -> isize;
    fn CFDataGetBytePtr(data: Ref) -> *const u8;
    fn CFGetTypeID(value: Ref) -> usize;
    fn CFDataGetTypeID() -> usize;
    fn CFDictionaryCreate(
        allocator: Ref,
        keys: *const Ref,
        values: *const Ref,
        count: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> Ref;
    static kCFTypeArrayCallBacks: u8;
    static kCFTypeDictionaryKeyCallBacks: u8;
    static kCFTypeDictionaryValueCallBacks: u8;
    static kCFBooleanTrue: Ref;
}
#[link(name = "Security", kind = "framework")]
extern "C" {
    fn SecItemCopyMatching(query: Ref, result: *mut Ref) -> i32;
    fn SecItemAdd(attributes: Ref, result: *mut Ref) -> i32;
    fn SecItemUpdate(query: Ref, attributes: Ref) -> i32;
    fn SecItemDelete(query: Ref) -> i32;
    static kSecClass: Ref;
    static kSecClassGenericPassword: Ref;
    static kSecAttrService: Ref;
    static kSecAttrAccount: Ref;
    static kSecAttrLabel: Ref;
    static kSecValueData: Ref;
    static kSecReturnData: Ref;
    static kSecReturnPersistentRef: Ref;
    static kSecMatchItemList: Ref;
    static kSecMatchLimit: Ref;
    static kSecMatchLimitOne: Ref;
    #[cfg(test)]
    static kSecMatchSearchList: Ref;
    #[cfg(test)]
    static kSecUseKeychain: Ref;
}
// CF Create/Copy values are owned. Dictionaries retain their keys and values
// using Apple's CFType callbacks; no borrowed Rust buffer escapes an API call.
struct Owned(Ref);
impl Owned {
    fn new(value: Ref) -> Result<Self, i32> {
        if value.is_null() {
            Err(PARAM)
        } else {
            Ok(Self(value))
        }
    }
    fn string(value: &str) -> Result<Self, i32> {
        // SAFETY: `value` 的指针和长度在调用期间有效，CoreFoundation 复制字节；0x08000100 是 kCFStringEncodingUTF8，
        // &str 必为合法 UTF-8。返回空指针时由 `new` 转成错误。
        unsafe { Self::new(CFStringCreateWithBytes(ptr::null(), value.as_ptr(), value.len() as isize, 0x08000100, 0)) }
    }
    fn data(value: &[u8]) -> Result<Self, i32> {
        // SAFETY: 切片指针和长度在调用期间有效，CFDataCreate 复制字节；返回空指针时由 `new` 转成错误。
        unsafe { Self::new(CFDataCreate(ptr::null(), value.as_ptr(), value.len() as isize)) }
    }
    fn array(value: Ref) -> Result<Self, i32> {
        // SAFETY: `&value` 指向调用期间有效的一个元素；回调是 Apple 导出的 kCFTypeArrayCallBacks，
        // 数组自行 retain 元素。
        unsafe { Self::new(CFArrayCreate(ptr::null(), &value, 1, ptr::addr_of!(kCFTypeArrayCallBacks).cast())) }
    }
    fn dictionary(pairs: &[(Ref, Ref)]) -> Result<Self, i32> {
        let keys: Vec<_> = pairs.iter().map(|p| p.0).collect();
        let values: Vec<_> = pairs.iter().map(|p| p.1).collect();
        // The callback structures are opaque to Rust: only their exported
        // addresses are passed to CoreFoundation, never read or dereferenced.
        // SAFETY: `keys`、`values` 与 `pairs` 等长且在调用期间存活；CFType 回调让字典 retain 键值，调用返回后不再引用
        // Rust 缓冲区。
        unsafe {
            Self::new(CFDictionaryCreate(
                ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                pairs.len() as isize,
                ptr::addr_of!(kCFTypeDictionaryKeyCallBacks).cast(),
                ptr::addr_of!(kCFTypeDictionaryValueCallBacks).cast(),
            ))
        }
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: `Owned` 只包装 Create/Copy 返回的非空引用（`new` 已拒绝空指针），每个值只在这里释放一次。
        unsafe { CFRelease(self.0) }
    }
}
#[derive(Default)]
pub(super) struct Store {
    // Only unit tests can supply a keychain. Shipped code uses the existing
    // user search list/default keychain, never the Data Protection keychain.
    #[cfg(test)]
    keychain: Option<Owned>,
    #[cfg(test)]
    search_list: Option<Owned>,
}
impl Store {
    fn query(&self, kind: &str, adding: bool) -> Result<Owned, i32> {
        #[cfg(test)]
        if self.keychain.is_none() && (self.search_list.is_none() || adding) {
            // Test builds must fail closed without an explicit synthetic scope;
            // a search-list-only fixture must never add to a default keychain.
            return Err(PARAM);
        }
        let service = Owned::string("Illustash")?;
        let account = Owned::string(kind)?;
        #[allow(unused_mut)]
        // SAFETY: 只读 Security 框架导出的常量指针，加载框架时已初始化，此后不变。
        let mut pairs = unsafe {
            vec![(kSecClass, kSecClassGenericPassword), (kSecAttrService, service.0), (kSecAttrAccount, account.0)]
        };
        #[cfg(not(test))]
        let _ = adding;
        #[cfg(test)]
        let search_list = if let Some(keychain) = &self.keychain {
            // Explicit scope for every operation. Do not set the user's search
            // list or default keychain, even temporarily.
            let list = Owned::array(keychain.0)?;
            // SAFETY: 只读 Security 框架导出的常量指针；`keychain`、`list` 在字典创建前保持存活。
            unsafe {
                pairs.push(if adding { (kSecUseKeychain, keychain.0) } else { (kSecMatchSearchList, list.0) });
            }
            Some(list)
        } else {
            None
        };
        #[cfg(test)]
        if !adding {
            if let Some(list) = &self.search_list {
                // SAFETY: 只读 Security 框架导出的常量指针；`list` 在字典创建前保持存活。
                unsafe {
                    pairs.push((kSecMatchSearchList, list.0));
                }
            }
        }
        let result = Owned::dictionary(&pairs);
        #[cfg(test)]
        drop(search_list);
        result
    }
    pub(super) fn read(&self, kind: &str) -> Result<Vec<u8>, i32> {
        let query = self.query(kind, false)?;
        let attributes =
            // SAFETY: 只读 CoreFoundation/Security 导出的常量指针。
            unsafe { Owned::dictionary(&[(kSecReturnData, kCFBooleanTrue), (kSecMatchLimit, kSecMatchLimitOne)])? };
        let query = merge(&query, &attributes)?;
        let mut value = ptr::null();
        // SAFETY: `query` 是存活的字典，`value` 是本地出参；成功时返回 +1 引用，下一行交给 `Owned` 释放。
        let status = unsafe { SecItemCopyMatching(query.0, &mut value) };
        if status != 0 {
            return Err(status);
        }
        let value = Owned::new(value)?;
        // SAFETY: `value` 是非空的已拥有引用。先确认类型是 CFData 再取长度和字节指针；切片在 `value` 释放前复制进 Vec。
        unsafe {
            if CFGetTypeID(value.0) != CFDataGetTypeID() {
                return Err(PARAM);
            }
            let len = CFDataGetLength(value.0);
            if len == 0 {
                return Ok(Vec::new());
            }
            Ok(std::slice::from_raw_parts(CFDataGetBytePtr(value.0), len as usize).to_vec())
        }
    }
    pub(super) fn write(&self, kind: &str, bytes: &[u8]) -> Result<(), i32> {
        let data = Owned::data(bytes)?;
        let label = Owned::string(&format!("Illustash · {kind}"))?;
        // SAFETY: 只读导出常量；`data`、`label` 在字典创建期间存活，字典 retain 它们。
        let attributes = unsafe { Owned::dictionary(&[(kSecValueData, data.0), (kSecAttrLabel, label.0)])? };
        // Resolve one item first: SecItemUpdate/Delete otherwise act on every
        // matching item across the user's search list. The former CLI acted on
        // one item, and existing ACL/identity must remain attached to that item.
        match self.first_item_query(kind) {
            // SAFETY: `query`、`attributes` 都是存活的字典，调用不保留 Rust 指针。
            Ok(query) => status_result(unsafe { SecItemUpdate(query.0, attributes.0) }),
            Err(NOT_FOUND) => {
                let add = merge(&self.query(kind, true)?, &attributes)?;
                // SAFETY: `add` 是存活的字典；结果出参允许为空指针。
                let status = unsafe { SecItemAdd(add.0, ptr::null_mut()) };
                if status == DUPLICATE {
                    let query = self.first_item_query(kind)?;
                    // SAFETY: `query`、`attributes` 都是存活的字典，调用不保留 Rust 指针。
                    status_result(unsafe { SecItemUpdate(query.0, attributes.0) })
                } else {
                    status_result(status)
                }
            }
            Err(status) => Err(status),
        }
    }
    fn first_item_query(&self, kind: &str) -> Result<Owned, i32> {
        let query = self.query(kind, false)?;
        // SAFETY: 只读 CoreFoundation/Security 导出的常量指针。
        let flags = unsafe {
            Owned::dictionary(&[(kSecReturnPersistentRef, kCFBooleanTrue), (kSecMatchLimit, kSecMatchLimitOne)])?
        };
        let lookup = merge(&query, &flags)?;
        let mut item = ptr::null();
        // SAFETY: `lookup` 是存活的字典，`item` 是本地出参；成功时返回 +1 引用，随后交给 `Owned` 释放。
        let status = unsafe { SecItemCopyMatching(lookup.0, &mut item) };
        status_result(status)?;
        let item = Owned::new(item)?;
        let items = Owned::array(item.0)?;
        // SAFETY: 只读导出常量；`items` 在字典创建期间存活，字典 retain 它。
        let selection = unsafe { Owned::dictionary(&[(kSecMatchItemList, items.0)])? };
        merge(&query, &selection)
    }
    pub(super) fn remove(&self, kind: &str) -> Result<(), i32> {
        match self.first_item_query(kind) {
            // SAFETY: `query` 是存活的字典。
            Ok(query) => match unsafe { SecItemDelete(query.0) } {
                0 | NOT_FOUND => Ok(()),
                status => Err(status),
            },
            Err(NOT_FOUND) => Ok(()),
            Err(status) => Err(status),
        }
    }
}
fn status_result(status: i32) -> Result<(), i32> {
    if status == 0 {
        Ok(())
    } else {
        Err(status)
    }
}
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFDictionaryCreateMutableCopy(allocator: Ref, capacity: isize, source: Ref) -> Ref;
    fn CFDictionaryApplyFunction(
        dictionary: Ref,
        callback: unsafe extern "C" fn(Ref, Ref, *mut c_void),
        context: *mut c_void,
    );
    fn CFDictionarySetValue(dictionary: Ref, key: Ref, value: Ref);
    fn CFArrayCreate(allocator: Ref, values: *const Ref, count: isize, callbacks: *const c_void) -> Ref;
}
fn merge(query: &Owned, attributes: &Owned) -> Result<Owned, i32> {
    unsafe extern "C" fn add(key: Ref, value: Ref, context: *mut c_void) {
        // SAFETY: `context` 是 `merge` 传入的可变字典，键值由 CFDictionaryApplyFunction 在回调期间提供，
        // 可变字典自行 retain。
        unsafe {
            CFDictionarySetValue(context.cast_const(), key, value);
        }
    }
    // SAFETY: `query.0` 是存活的字典；返回空指针时由 `new` 转成错误。
    let combined = unsafe { Owned::new(CFDictionaryCreateMutableCopy(ptr::null(), 0, query.0))? };
    // SAFETY: `attributes.0` 是存活的字典，`combined.0` 是刚复制出的可变字典；
    // 回调签名与 CFDictionaryApplierFunction 一致。
    unsafe {
        CFDictionaryApplyFunction(attributes.0, add, combined.0.cast_mut());
    }
    Ok(combined)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{ffi::CString, process::Command};
    #[link(name = "Security", kind = "framework")]
    extern "C" {
        fn SecKeychainCreate(
            path: *const std::ffi::c_char,
            length: u32,
            password: Ref,
            prompt: u8,
            access: Ref,
            result: *mut Ref,
        ) -> i32;
        fn SecKeychainDelete(keychain: Ref) -> i32;
        fn SecKeychainSetUserInteractionAllowed(allowed: u8) -> i32;
        fn SecKeychainAddGenericPassword(
            keychain: Ref,
            service_len: u32,
            service: Ref,
            account_len: u32,
            account: Ref,
            password_len: u32,
            password: Ref,
            item: *mut Ref,
        ) -> i32;
        fn SecKeychainFindGenericPassword(
            keychain: Ref,
            service_len: u32,
            service: Ref,
            account_len: u32,
            account: Ref,
            password_len: *mut u32,
            password: *mut *mut c_void,
            item: *mut Ref,
        ) -> i32;
        fn SecKeychainItemFreeContent(attributes: Ref, data: *mut c_void) -> i32;
    }
    struct Synthetic(Store);
    impl Drop for Synthetic {
        fn drop(&mut self) {
            // Never receive a default/real keychain handle.
            // SAFETY: 合成夹具的 `keychain` 总是本测试 SecKeychainCreate 得到的句柄，此时 `Owned` 尚未释放。
            unsafe {
                SecKeychainDelete(self.0.keychain.as_ref().unwrap().0);
            }
        }
    }
    #[test]
    fn synthetic_keychain_legacy_compatibility_no_subprocess() {
        const CHILD: &str = "ILLUSTASH_SYNTHETIC_NATIVE_KEYCHAIN_119";
        if std::env::var(CHILD).as_deref() != Ok("1") {
            let root = std::env::temp_dir().join(format!("illustash-119-synthetic-keychain-{}", std::process::id()));
            assert!(!root.exists());
            std::fs::create_dir(&root).unwrap();
            // A trap executable catches any regression to PATH-based security.
            let trap = root.join("security");
            std::fs::write(&trap, "#!/bin/sh\nexit 99\n").unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&trap, std::fs::Permissions::from_mode(0o700)).unwrap();
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "platform_credentials::macos::tests::synthetic_keychain_legacy_compatibility_no_subprocess",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .env("ILLUSTASH_SYNTHETIC_KEYCHAIN_ROOT", &root)
                .env("PATH", &root)
                .output()
                .unwrap();
            std::fs::remove_dir_all(root).unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        assert!(Store::default().query("saucenao", false).is_err());
        assert!(Store::default().query("saucenao", true).is_err());
        // Disable interaction in this isolated child, so tests cannot unlock a
        // real keychain or request user authorization. No global settings change.
        // SAFETY: 无指针参数；只影响这个隔离的子进程。
        assert_eq!(unsafe { SecKeychainSetUserInteractionAllowed(0) }, 0);
        let root = std::path::PathBuf::from(std::env::var_os("ILLUSTASH_SYNTHETIC_KEYCHAIN_ROOT").unwrap());
        assert!(root.file_name().unwrap().to_string_lossy().starts_with("illustash-119-synthetic-keychain-"));
        let path = CString::new(root.join("synthetic.keychain").as_os_str().as_encoded_bytes()).unwrap();
        let password = b"ILLUSTASH-119-SYNTHETIC-ONLY";
        let mut keychain = ptr::null();
        // SAFETY: `path` 是存活的 NUL 结尾 C 字符串，口令按给出的长度读取，`keychain` 是本地出参。
        let status = unsafe {
            SecKeychainCreate(
                path.as_ptr(),
                password.len() as u32,
                password.as_ptr().cast(),
                0,
                ptr::null(),
                &mut keychain,
            )
        };
        assert_eq!(status, 0, "isolated create failed; do not unlock or change real keychain permissions");
        let fixture = Synthetic(Store { keychain: Some(Owned::new(keychain).unwrap()), search_list: None });
        let store = &fixture.0;
        let kind = "saucenao";
        assert_eq!(store.read(kind), Err(NOT_FOUND));
        // The former CLI stores this same legacy generic-password item type,
        // service and account. Seed via the legacy API, using only this handle.
        let legacy = b"ILLUSTASH-119-SYNTHETIC-LEGACY";
        assert_eq!(
            // SAFETY: `keychain` 由夹具持有且存活；各缓冲区按给出的长度读取，item 出参为空。
            unsafe {
                SecKeychainAddGenericPassword(
                    keychain,
                    9,
                    b"Illustash".as_ptr().cast(),
                    kind.len() as u32,
                    kind.as_ptr().cast(),
                    legacy.len() as u32,
                    legacy.as_ptr().cast(),
                    ptr::null_mut(),
                )
            },
            0
        );
        assert_eq!(store.read(kind).unwrap(), legacy);
        let replacement = "ILLUSTASH-119-SYNTHETIC-合成\0密钥".as_bytes();
        store.write(kind, replacement).unwrap();
        assert_eq!(store.read(kind).unwrap(), replacement);
        // Reverse compatibility: the legacy API still sees exactly the updated
        // bytes; update did not create another modern/Data Protection item.
        let mut length = 0;
        let mut data = ptr::null_mut();
        assert_eq!(
            // SAFETY: `keychain` 由夹具持有且存活；`length`、`data` 是本地出参。
            unsafe {
                SecKeychainFindGenericPassword(
                    keychain,
                    9,
                    b"Illustash".as_ptr().cast(),
                    kind.len() as u32,
                    kind.as_ptr().cast(),
                    &mut length,
                    &mut data,
                    ptr::null_mut(),
                )
            },
            0
        );
        // SAFETY: 上一步成功的 SecKeychainFindGenericPassword 返回了 `length` 字节的 `data`，下一行才释放。
        assert_eq!(unsafe { std::slice::from_raw_parts(data.cast::<u8>(), length as usize) }, replacement);
        // SAFETY: `data` 来自上面成功的查找，只在这里释放一次。
        assert_eq!(unsafe { SecKeychainItemFreeContent(ptr::null(), data) }, 0);
        // Two synthetic keychains may contain the same service/account. Only
        // the first selected item must change; update/delete must not expand
        // into all matching entries in the search list.
        let second_path = CString::new(root.join("synthetic-second.keychain").as_os_str().as_encoded_bytes()).unwrap();
        let mut second_keychain = ptr::null();
        assert_eq!(
            // SAFETY: `second_path` 是存活的 NUL 结尾 C 字符串，口令按给出的长度读取，`second_keychain` 是本地出参。
            unsafe {
                SecKeychainCreate(
                    second_path.as_ptr(),
                    password.len() as u32,
                    password.as_ptr().cast(),
                    0,
                    ptr::null(),
                    &mut second_keychain,
                )
            },
            0
        );
        let second = Synthetic(Store { keychain: Some(Owned::new(second_keychain).unwrap()), search_list: None });
        second.0.write(kind, b"ILLUSTASH-119-SYNTHETIC-SECOND").unwrap();
        let refs = [keychain, second_keychain];
        // SAFETY: `refs` 是两个存活钥匙串句柄的本地数组；kCFTypeArrayCallBacks 让数组 retain 它们。
        let list = unsafe {
            Owned::new(CFArrayCreate(ptr::null(), refs.as_ptr(), 2, ptr::addr_of!(kCFTypeArrayCallBacks).cast()))
                .unwrap()
        };
        let combined = Store { keychain: None, search_list: Some(list) };
        combined.write(kind, b"ILLUSTASH-119-SYNTHETIC-FIRST").unwrap();
        assert_eq!(store.read(kind).unwrap(), b"ILLUSTASH-119-SYNTHETIC-FIRST");
        assert_eq!(second.0.read(kind).unwrap(), b"ILLUSTASH-119-SYNTHETIC-SECOND");
        combined.remove(kind).unwrap();
        assert_eq!(store.read(kind), Err(NOT_FOUND));
        assert_eq!(second.0.read(kind).unwrap(), b"ILLUSTASH-119-SYNTHETIC-SECOND");
        store.remove(kind).unwrap();
        store.remove(kind).unwrap();
        assert_eq!(store.read(kind), Err(NOT_FOUND));
        store.write("pixiv", b"ILLUSTASH-119-SYNTHETIC-NEW").unwrap();
        assert_eq!(store.read("pixiv").unwrap(), b"ILLUSTASH-119-SYNTHETIC-NEW");
        store.remove("pixiv").unwrap();
    }
}

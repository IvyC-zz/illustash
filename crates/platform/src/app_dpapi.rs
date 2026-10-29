//! Windows current-user DPAPI; no machine-wide key or plaintext fallback.
use crate::{Error, Result};
use std::{ffi::c_void, ptr};

#[repr(C)]
struct Blob {
    len: u32,
    data: *mut u8,
}
#[link(name = "crypt32")]
extern "system" {
    fn CryptProtectData(
        input: *const Blob,
        description: *const u16,
        entropy: *const Blob,
        reserved: *mut c_void,
        prompt: *mut c_void,
        flags: u32,
        output: *mut Blob,
    ) -> i32;
    fn CryptUnprotectData(
        input: *const Blob,
        description: *mut *mut u16,
        entropy: *const Blob,
        reserved: *mut c_void,
        prompt: *mut c_void,
        flags: u32,
        output: *mut Blob,
    ) -> i32;
}
#[link(name = "kernel32")]
extern "system" {
    fn LocalFree(memory: *mut c_void) -> *mut c_void;
}

fn transform(bytes: &[u8], kind: &str, encrypt: bool) -> Result<Vec<u8>> {
    let len = u32::try_from(bytes.len()).map_err(|_| Error::Invalid("Windows 凭据过大".into()))?;
    let input = Blob { len, data: bytes.as_ptr() as *mut u8 };
    // Bind each blob to its account kind. Entropy is a purpose label, not a key.
    let purpose = format!("Illustash/accounts/v1/{kind}");
    let entropy = Blob { len: purpose.len() as u32, data: purpose.as_ptr() as *mut u8 };
    let mut output = Blob { len: 0, data: ptr::null_mut() };
    // SAFETY: all blobs remain alive for the synchronous call; output is allocated
    // by DPAPI and freed with LocalFree. UI_FORBIDDEN = 1; LOCAL_MACHINE is absent.
    let ok = unsafe {
        if encrypt {
            CryptProtectData(&input, ptr::null(), &entropy, ptr::null_mut(), ptr::null_mut(), 1, &mut output)
        } else {
            CryptUnprotectData(&input, ptr::null_mut(), &entropy, ptr::null_mut(), ptr::null_mut(), 1, &mut output)
        }
    };
    if ok == 0 {
        return Err(Error::Invalid(
            if encrypt {
                "Windows 凭据加密失败"
            } else {
                "Windows 凭据解密失败，请在原 Windows 账号下使用或重新绑定"
            }
            .into(),
        ));
    }
    // SAFETY: successful DPAPI returns a buffer of output.len bytes.
    let result = unsafe { std::slice::from_raw_parts(output.data, output.len as usize).to_vec() };
    // SAFETY: `output.data` 是成功调用后 DPAPI 分配的 `output.len` 字节缓冲区；先清零再用 LocalFree 释放一次，
    // 之后不再使用。
    unsafe {
        // Do not leave decrypted credentials in the Windows-allocated buffer.
        if !encrypt {
            for i in 0..output.len as usize {
                ptr::write_volatile(output.data.add(i), 0);
            }
        }
        LocalFree(output.data.cast());
    }
    Ok(result)
}
pub fn protect(bytes: &[u8], kind: &str) -> Result<Vec<u8>> {
    transform(bytes, kind, true)
}
pub fn unprotect(bytes: &[u8], kind: &str) -> Result<Vec<u8>> {
    transform(bytes, kind, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn synthetic_roundtrip_is_encrypted_and_bound_to_purpose() {
        let secret = "synthetic-only-合成凭据".as_bytes();
        let blob = protect(secret, "pixiv").unwrap();
        assert!(!blob.windows(secret.len()).any(|w| w == secret));
        assert_eq!(unprotect(&blob, "pixiv").unwrap(), secret);
        assert!(unprotect(&blob, "saucenao").is_err());
        assert!(unprotect(b"invalid synthetic blob", "pixiv").is_err());
        let mut corrupt = blob;
        corrupt.truncate(corrupt.len() / 2);
        assert!(unprotect(&corrupt, "pixiv").is_err());
    }
}

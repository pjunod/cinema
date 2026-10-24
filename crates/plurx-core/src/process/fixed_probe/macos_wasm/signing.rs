//! Query the running process, not the signature of a re-opened executable path.
//! Wasmtime's current macOS code publisher uses mprotect(RX), not MAP_JIT.
//! Hardened Runtime therefore needs Apple's unsigned executable memory exception.
use std::ffi::{c_char, c_void};
use std::ptr;

type Cf = *const c_void;

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    fn SecCodeCopySelf(flags: u32, code: *mut Cf) -> i32;
    fn SecCodeCopySigningInformation(code: Cf, flags: u32, information: *mut Cf) -> i32;
    static kSecCodeInfoStatus: Cf;
    fn SecTaskCreateFromSelf(allocator: Cf) -> Cf;
    fn SecTaskCopyValueForEntitlement(task: Cf, entitlement: Cf, error: *mut Cf) -> Cf;
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(value: Cf);
    fn CFGetTypeID(value: Cf) -> usize;
    fn CFDictionaryGetTypeID() -> usize;
    fn CFDictionaryGetValue(dictionary: Cf, key: Cf) -> Cf;
    fn CFNumberGetTypeID() -> usize;
    fn CFNumberGetValue(number: Cf, kind: i32, value: *mut c_void) -> u8;
    fn CFStringCreateWithCString(allocator: Cf, string: *const c_char, encoding: u32) -> Cf;
    fn CFBooleanGetTypeID() -> usize;
    fn CFBooleanGetValue(boolean: Cf) -> u8;
}

struct Owned(Cf);
impl Owned {
    fn new(value: Cf) -> Result<Self, String> {
        if value.is_null() {
            return Err("source parser signing inspection returned no object".into());
        }
        Ok(Self(value))
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) }
    }
}

pub(super) fn require_execution_policy() -> Result<(), String> {
    // kSecCSDynamicInformation returns the running kernel status word. Apple's
    // public Security implementation adds dcode->status(), rather than flags
    // from a filename: OSX/libsecurity_codesigning/lib/SecCode.cpp. The runtime
    // flag is sticky. SecTask resolves the live audit token's entitlements.
    unsafe {
        let mut code = ptr::null();
        let status = SecCodeCopySelf(0, &mut code);
        if status != 0 {
            return Err(format!(
                "source parser cannot inspect running code: {status}"
            ));
        }
        let code = Owned::new(code)?;
        let mut info = ptr::null();
        let status = SecCodeCopySigningInformation(code.0, 1 << 3, &mut info);
        if status != 0 {
            return Err(format!(
                "source parser cannot inspect running signing policy: {status}"
            ));
        }
        let info = Owned::new(info)?;
        if CFGetTypeID(info.0) != CFDictionaryGetTypeID() {
            return Err("invalid running signing policy".into());
        }
        let flags = CFDictionaryGetValue(info.0, kSecCodeInfoStatus);
        if flags.is_null() || CFGetTypeID(flags) != CFNumberGetTypeID() {
            return Err("running signing status is unavailable".into());
        }
        let mut flags_value = 0i64;
        if CFNumberGetValue(flags, 4, (&mut flags_value as *mut i64).cast()) == 0 {
            return Err("running signing status is not numeric".into());
        }
        // kSecCodeSignatureRuntime (CSCommon.h).
        if flags_value & 0x10000 == 0 {
            return Ok(());
        }
        let task = Owned::new(SecTaskCreateFromSelf(ptr::null()))?;
        let key = Owned::new(CFStringCreateWithCString(
            ptr::null(),
            c"com.apple.security.cs.allow-unsigned-executable-memory".as_ptr(),
            0x08000100,
        ))?;
        let mut error = ptr::null();
        let value = SecTaskCopyValueForEntitlement(task.0, key.0, &mut error);
        if !error.is_null() {
            CFRelease(error);
            if !value.is_null() {
                CFRelease(value);
            }
            return Err("running JIT entitlement inspection failed".into());
        }
        let permitted = if value.is_null() {
            false
        } else {
            let value = Owned::new(value)?;
            CFGetTypeID(value.0) == CFBooleanGetTypeID() && CFBooleanGetValue(value.0) != 0
        };
        if permitted {
            Ok(())
        } else {
            Err(
                "hardened daemon requires the packaged source parser JIT signing entitlement"
                    .into(),
            )
        }
    }
}

//! DLL module handle, COM reference count, and per-process session ID generator.

use std::ffi::c_void;
use std::path::PathBuf;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicPtr, AtomicU32, Ordering};

use windows::Win32::Foundation::HINSTANCE;
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;

static DLL_INSTANCE: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
static DLL_REF_COUNT: AtomicU32 = AtomicU32::new(0);
static NEXT_SESSION_SEQ: AtomicU32 = AtomicU32::new(1);

pub fn set_dll_instance(hinst: HINSTANCE) {
    DLL_INSTANCE.store(hinst.0, Ordering::Release);
}

pub fn dll_hinstance() -> HINSTANCE {
    HINSTANCE(DLL_INSTANCE.load(Ordering::Acquire))
}

pub fn dll_add_ref() {
    DLL_REF_COUNT.fetch_add(1, Ordering::Relaxed);
}

pub fn dll_release() {
    let prev = DLL_REF_COUNT.fetch_sub(1, Ordering::AcqRel);
    debug_assert!(prev > 0);
}

pub fn dll_ref_count() -> u32 {
    DLL_REF_COUNT.load(Ordering::Acquire)
}

pub fn next_session_id() -> u64 {
    let pid = std::process::id() as u64;
    let seq = NEXT_SESSION_SEQ.fetch_add(1, Ordering::Relaxed) as u64;
    (pid << 32) | seq
}

pub fn dll_module_path() -> Option<PathBuf> {
    let hinst = dll_hinstance();
    if hinst.0.is_null() {
        return None;
    }
    let mut buf = vec![0u16; 1024];
    let len = unsafe { GetModuleFileNameW(Some(hinst.into()), &mut buf) } as usize;
    if len == 0 || len >= buf.len() {
        return None;
    }
    Some(PathBuf::from(String::from_utf16_lossy(&buf[..len])))
}

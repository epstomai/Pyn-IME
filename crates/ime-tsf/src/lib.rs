//! Thin Windows Text Services Framework (TSF) COM in-process DLL (`ime_tsf.dll`).

#![allow(non_snake_case)]

mod candidate_ui;
mod display_attr;
mod edit_session;
mod globals;
pub mod guids;
mod lang_bar;
mod register;
mod text_service;

use std::ffi::c_void;
use std::ptr::null_mut;

use windows::Win32::Foundation::{
    CLASS_E_CLASSNOTAVAILABLE, E_INVALIDARG, E_NOINTERFACE, HINSTANCE, S_FALSE, S_OK,
};
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};
use windows::Win32::System::SystemServices::{DLL_PROCESS_ATTACH, DLL_PROCESS_DETACH};
use windows::Win32::UI::TextServices::ITfTextInputProcessorEx;
use windows_core::{implement, BOOL, ComObject, GUID, HRESULT, Interface, PCWSTR, Ref, Result};

use crate::globals::{dll_add_ref, dll_ref_count, dll_release, set_dll_instance};
use crate::guids::active_guids;
use crate::text_service::TextService;

#[implement(IClassFactory)]
struct ClassFactory;

impl ClassFactory {
    fn new() -> Self {
        dll_add_ref();
        Self
    }
}

impl Drop for ClassFactory {
    fn drop(&mut self) {
        dll_release();
    }
}

impl IClassFactory_Impl for ClassFactory_Impl {
    fn CreateInstance(
        &self,
        punkouter: Ref<windows_core::IUnknown>,
        riid: *const GUID,
        ppvobject: *mut *mut c_void,
    ) -> Result<()> {
        if ppvobject.is_null() {
            return Err(windows_core::Error::from(E_INVALIDARG));
        }
        unsafe {
            *ppvobject = null_mut();
        }
        if !punkouter.is_null() {
            return Err(windows_core::Error::from(E_NOINTERFACE));
        }
        if riid.is_null() {
            return Err(windows_core::Error::from(E_INVALIDARG));
        }
        let tip = ComObject::new(TextService::new()).into_interface::<ITfTextInputProcessorEx>();
        let unk: windows_core::IUnknown = tip.into();
        let hr = unsafe { unk.query(riid, ppvobject) };
        if hr.is_ok() {
            Ok(())
        } else {
            Err(windows_core::Error::from(hr))
        }
    }

    fn LockServer(&self, flock: BOOL) -> Result<()> {
        if flock.as_bool() {
            dll_add_ref();
        } else {
            dll_release();
        }
        Ok(())
    }
}

#[no_mangle]
pub unsafe extern "system" fn DllMain(
    hinst_dll: HINSTANCE,
    fdw_reason: u32,
    _lpv_reserved: *mut c_void,
) -> BOOL {
    if fdw_reason == DLL_PROCESS_ATTACH {
        set_dll_instance(hinst_dll);
    } else if fdw_reason == DLL_PROCESS_DETACH {
        // Loader lock: do nothing blocking here.
    }
    BOOL(1)
}

#[no_mangle]
pub unsafe extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut c_void,
) -> HRESULT {
    if ppv.is_null() {
        return E_INVALIDARG;
    }
    *ppv = null_mut();
    if rclsid.is_null() || riid.is_null() {
        return E_INVALIDARG;
    }
    if *rclsid != active_guids().clsid {
        return CLASS_E_CLASSNOTAVAILABLE;
    }
    let factory = ComObject::new(ClassFactory::new()).into_interface::<IClassFactory>();
    let unk: windows_core::IUnknown = factory.into();
    unk.query(riid, ppv)
}

#[no_mangle]
pub unsafe extern "system" fn DllCanUnloadNow() -> HRESULT {
    if dll_ref_count() == 0 {
        S_OK
    } else {
        S_FALSE
    }
}

#[no_mangle]
pub unsafe extern "system" fn DllInstall(b_install: BOOL, psz_cmd_line: PCWSTR) -> HRESULT {
    if b_install.as_bool() && !psz_cmd_line.is_null() {
        let mut len = 0usize;
        while *psz_cmd_line.0.add(len) != 0 {
            len += 1;
        }
        let slice = std::slice::from_raw_parts(psz_cmd_line.0, len);
        let cmd = String::from_utf16_lossy(slice);
        if cmd.trim().eq_ignore_ascii_case("activate") {
            return match register::activate_profile_for_session() {
                Ok(()) => S_OK,
                Err(e) => e.code(),
            };
        }
    }
    S_OK
}

#[no_mangle]
pub unsafe extern "system" fn DllRegisterServer() -> HRESULT {
    match register::register_server() {
        Ok(()) => S_OK,
        Err(e) => e.code(),
    }
}

#[no_mangle]
pub unsafe extern "system" fn DllUnregisterServer() -> HRESULT {
    match register::unregister_server() {
        Ok(()) => S_OK,
        Err(e) => e.code(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::candidate_ui::{CandidateViewState, CandidateWindow};
    use windows::Win32::UI::TextServices::{
        ITfCompartmentEventSink, ITfCompositionSink, ITfDisplayAttributeProvider, ITfKeyEventSink,
        ITfTextInputProcessor, ITfTextInputProcessorEx, ITfThreadMgrEventSink,
    };

    #[test]
    fn com_factory_interfaces_and_candidate_window_lifecycle() {
        let baseline = globals::dll_ref_count();
        let mut factory_ptr: *mut c_void = null_mut();
        let hr = unsafe {
            DllGetClassObject(&active_guids().clsid, &IClassFactory::IID, &mut factory_ptr)
        };
        assert_eq!(hr, S_OK);
        assert_eq!(unsafe { DllCanUnloadNow() }, S_FALSE);

        let factory: IClassFactory = unsafe { IClassFactory::from_raw(factory_ptr) };
        let tip: ITfTextInputProcessorEx = unsafe { factory.CreateInstance(None).unwrap() };

        assert!(tip.cast::<ITfTextInputProcessor>().is_ok());
        assert!(tip.cast::<ITfTextInputProcessorEx>().is_ok());
        assert!(tip.cast::<ITfKeyEventSink>().is_ok());
        assert!(tip.cast::<ITfThreadMgrEventSink>().is_ok());
        assert!(tip.cast::<ITfCompositionSink>().is_ok());
        assert!(tip.cast::<ITfDisplayAttributeProvider>().is_ok());
        assert!(tip.cast::<ITfCompartmentEventSink>().is_ok());

        let dap = tip.cast::<ITfDisplayAttributeProvider>().unwrap();
        let attr = unsafe {
            dap.GetDisplayAttributeInfo(&active_guids().display_attr_input)
                .unwrap()
        };
        assert_eq!(unsafe { attr.GetGUID().unwrap() }, active_guids().display_attr_input);

        drop(dap);
        drop(attr);
        drop(tip);
        drop(factory);
        if baseline == 0 {
            assert!(globals::dll_ref_count() <= 1);
        }

        // Candidate window creates, renders, hides, and destroys without error.
        let mut wnd = CandidateWindow::new().unwrap();
        wnd.update_and_show(
            CandidateViewState {
                preedit: "nihao".into(),
                mode_label: "中".into(),
                page_text: "1/2".into(),
                candidates: vec!["你好".into(), "拟好".into()],
                selected_in_page: 0,
                is_toast: false,
            },
            None,
        );
        wnd.show_mode_toast("英", None);
        wnd.hide();
        drop(wnd);
    }
}

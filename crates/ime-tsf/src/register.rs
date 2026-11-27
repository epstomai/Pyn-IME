//! COM CLSID and TSF Profile/Category registration (`DllRegisterServer` / `DllUnregisterServer`).

use std::ptr::null_mut;

use ime_ipc::pipe::to_wide_null;
use windows::Win32::Foundation::E_FAIL;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegSetValueExW, HKEY, HKEY_LOCAL_MACHINE,
    KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ,
};
use windows::Win32::UI::Input::KeyboardAndMouse::HKL;
use windows::Win32::UI::TextServices::{
    CLSID_TF_CategoryMgr, CLSID_TF_InputProcessorProfiles, CLSID_TF_ThreadMgr,
    GUID_TFCAT_DISPLAYATTRIBUTEPROVIDER, GUID_TFCAT_TIPCAP_COMLESS,
    GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT, GUID_TFCAT_TIPCAP_INPUTMODECOMPARTMENT,
    GUID_TFCAT_TIPCAP_SECUREMODE, GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT,
    GUID_TFCAT_TIPCAP_UIELEMENTENABLED, GUID_TFCAT_TIP_KEYBOARD, ITfCategoryMgr,
    ITfInputProcessorProfileMgr, ITfInputProcessorProfiles, ITfThreadMgr,
    TF_IPPMF_DONTCARECURRENTINPUTLANGUAGE, TF_IPPMF_ENABLEPROFILE, TF_IPPMF_FORSESSION,
    TF_PROFILETYPE_INPUTPROCESSOR,
};
use windows_core::{Error, GUID, HRESULT, Interface, PCWSTR};

use crate::debug_log::{refresh_enabled, tsf_log};
use crate::globals::dll_module_path;
use crate::guids::{active_guids, guid_to_braced_string, LANGID_ZH_CN, TIP_DISPLAY_NAME};

const CATEGORIES: [&GUID; 8] = [
    &GUID_TFCAT_TIP_KEYBOARD,
    &GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT,
    &GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT,
    &GUID_TFCAT_TIPCAP_UIELEMENTENABLED,
    &GUID_TFCAT_TIPCAP_SECUREMODE,
    &GUID_TFCAT_TIPCAP_COMLESS,
    &GUID_TFCAT_TIPCAP_INPUTMODECOMPARTMENT,
    &GUID_TFCAT_DISPLAYATTRIBUTEPROVIDER,
];

fn set_reg_sz(hkey: HKEY, name: Option<&str>, value: &str) -> windows_core::Result<()> {
    let w_name = name.map(to_wide_null);
    let w_val = to_wide_null(value);
    let bytes = unsafe {
        std::slice::from_raw_parts(w_val.as_ptr() as *const u8, w_val.len() * 2)
    };
    let name_ptr = w_name
        .as_ref()
        .map_or(PCWSTR::null(), |v| PCWSTR(v.as_ptr()));
    let err = unsafe { RegSetValueExW(hkey, name_ptr, None, REG_SZ, Some(bytes)) };
    if err.is_err() {
        Err(Error::from(err))
    } else {
        Ok(())
    }
}

fn write_clsid_registry(clsid: &GUID, dll_path: &str) -> windows_core::Result<()> {
    let clsid_str = guid_to_braced_string(clsid);
    let subkey = format!("SOFTWARE\\Classes\\CLSID\\{clsid_str}");
    let w_sub = to_wide_null(&subkey);
    let mut hkey = HKEY::default();
    let err = unsafe {
        RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(w_sub.as_ptr()),
            None,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut hkey,
            None,
        )
    };
    if err.is_err() {
        return Err(Error::from(err));
    }
    let res = set_reg_sz(hkey, None, TIP_DISPLAY_NAME);
    unsafe {
        let _ = RegCloseKey(hkey);
    }
    res?;

    let inproc_key = format!("{subkey}\\InprocServer32");
    let w_inproc = to_wide_null(&inproc_key);
    let mut h_inproc = HKEY::default();
    let err2 = unsafe {
        RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(w_inproc.as_ptr()),
            None,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut h_inproc,
            None,
        )
    };
    if err2.is_err() {
        return Err(Error::from(err2));
    }
    let res1 = set_reg_sz(h_inproc, None, dll_path);
    let res2 = set_reg_sz(h_inproc, Some("ThreadingModel"), "Apartment");
    unsafe {
        let _ = RegCloseKey(h_inproc);
    }
    res1.and(res2)
}

fn delete_clsid_registry(clsid: &GUID) {
    let clsid_str = guid_to_braced_string(clsid);
    let subkey = format!("SOFTWARE\\Classes\\CLSID\\{clsid_str}");
    let w_sub = to_wide_null(&subkey);
    unsafe {
        let _ = RegDeleteTreeW(HKEY_LOCAL_MACHINE, PCWSTR(w_sub.as_ptr()));
    }
}

struct ComScope {
    should_uninit: bool,
}

impl ComScope {
    fn enter() -> windows_core::Result<Self> {
        let hr: HRESULT = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if hr.is_ok() {
            Ok(Self {
                should_uninit: true,
            })
        } else {
            // RPC_E_CHANGED_MODE (0x80010106) means caller already initialized MTA.
            Ok(Self {
                should_uninit: false,
            })
        }
    }
}

impl Drop for ComScope {
    fn drop(&mut self) {
        if self.should_uninit {
            unsafe { CoUninitialize() };
        }
    }
}

fn res_hr_u32<T>(res: &windows_core::Result<T>) -> u32 {
    match res {
        Ok(_) => 0,
        Err(e) => e.code().0 as u32,
    }
}

pub fn register_server() -> windows_core::Result<()> {
    refresh_enabled();
    let g = active_guids();
    let clsid_str = guid_to_braced_string(&g.clsid);
    let dll_path = dll_module_path().ok_or_else(|| Error::from(E_FAIL))?;
    let dll_str = dll_path.to_string_lossy();
    let clsid_res = write_clsid_registry(&g.clsid, &dll_str);
    if let Err(e) = &clsid_res {
        tsf_log!(
            "register",
            "register_server clsid={} write_clsid hr=0x{:08x}",
            clsid_str,
            e.code().0 as u32
        );
    }
    clsid_res?;

    let _com = ComScope::enter()?;

    let profile_mgr: ITfInputProcessorProfileMgr =
        unsafe { CoCreateInstance(&CLSID_TF_InputProcessorProfiles, None, CLSCTX_INPROC_SERVER)? };

    let desc_w: Vec<u16> = TIP_DISPLAY_NAME.encode_utf16().collect();
    let icon_w: Vec<u16> = dll_str.encode_utf16().collect();
    // Note: RegisterProfile expects TF_RP_* flags (0 = persistent HKLM LanguageProfile).
    // Passing TF_IPP_CAPS_UIELEMENTENABLED (0x4) collides with TF_RP_LOCALPROCESS and suppresses HKLM persistence.
    let reg_prof_res = unsafe {
        profile_mgr.RegisterProfile(
            &g.clsid,
            LANGID_ZH_CN,
            &g.profile,
            &desc_w,
            &icon_w,
            0,
            HKL(null_mut()),
            0,
            true,
            0,
        )
    };
    let reg_prof_hr = res_hr_u32(&reg_prof_res);
    if reg_prof_res.is_err() {
        tsf_log!(
            "register",
            "register_server clsid={} RegisterProfile hr=0x{:08x}",
            clsid_str,
            reg_prof_hr
        );
    }
    reg_prof_res?;

    let cat_mgr: ITfCategoryMgr =
        unsafe { CoCreateInstance(&CLSID_TF_CategoryMgr, None, CLSCTX_INPROC_SERVER)? };
    for &cat in &CATEGORIES {
        let r = unsafe { cat_mgr.RegisterCategory(&g.clsid, cat, &g.clsid) };
        if let Err(e) = r {
            let hr = e.code().0 as u32;
            tsf_log!(
                "register",
                "register_server clsid={} RegisterCategory({}) hr=0x{:08x}",
                clsid_str,
                guid_to_braced_string(cat),
                hr
            );
            return Err(e);
        }
    }
    tsf_log!(
        "register",
        "register_server clsid={} profile_hr=0x{:08x} cats_hr=0x00000000",
        clsid_str,
        reg_prof_hr
    );
    Ok(())
}

pub fn activate_profile_for_session() -> windows_core::Result<()> {
    refresh_enabled();
    let g = active_guids();
    let clsid_str = guid_to_braced_string(&g.clsid);
    let _com = ComScope::enter()?;

    let tm_activated: Option<ITfThreadMgr> = unsafe {
        match CoCreateInstance::<_, ITfThreadMgr>(&CLSID_TF_ThreadMgr, None, CLSCTX_INPROC_SERVER) {
            Ok(tm) => {
                let act_res = tm.Activate();
                tsf_log!(
                    "activate",
                    "tm.Activate clsid={} hr=0x{:08x}",
                    clsid_str,
                    res_hr_u32(&act_res)
                );
                if act_res.is_ok() {
                    Some(tm)
                } else {
                    None
                }
            }
            Err(e) => {
                tsf_log!(
                    "activate",
                    "CoCreateInstance(ThreadMgr) clsid={} hr=0x{:08x}",
                    clsid_str,
                    e.code().0 as u32
                );
                None
            }
        }
    };

    let profile_mgr: ITfInputProcessorProfileMgr =
        unsafe { CoCreateInstance(&CLSID_TF_InputProcessorProfiles, None, CLSCTX_INPROC_SERVER)? };

    match profile_mgr.cast::<ITfInputProcessorProfiles>() {
        Ok(profiles) => {
            let enable_res = unsafe {
                profiles.EnableLanguageProfile(&g.clsid, LANGID_ZH_CN, &g.profile, true)
            };
            tsf_log!(
                "activate",
                "EnableLanguageProfile clsid={} hr=0x{:08x}",
                clsid_str,
                res_hr_u32(&enable_res)
            );
            let default_res = unsafe {
                profiles.SetDefaultLanguageProfile(LANGID_ZH_CN, &g.clsid, &g.profile)
            };
            tsf_log!(
                "activate",
                "SetDefaultLanguageProfile clsid={} hr=0x{:08x}",
                clsid_str,
                res_hr_u32(&default_res)
            );
        }
        Err(e) => {
            tsf_log!(
                "activate",
                "cast(ITfInputProcessorProfiles) clsid={} hr=0x{:08x}",
                clsid_str,
                e.code().0 as u32
            );
        }
    }

    let flag_attempts = [
        TF_IPPMF_FORSESSION | TF_IPPMF_ENABLEPROFILE | TF_IPPMF_DONTCARECURRENTINPUTLANGUAGE,
        TF_IPPMF_FORSESSION | TF_IPPMF_ENABLEPROFILE,
        TF_IPPMF_FORSESSION | TF_IPPMF_DONTCARECURRENTINPUTLANGUAGE,
        TF_IPPMF_FORSESSION,
    ];
    let mut res = Ok(());
    for flags in flag_attempts {
        res = unsafe {
            profile_mgr.ActivateProfile(
                TF_PROFILETYPE_INPUTPROCESSOR,
                LANGID_ZH_CN,
                &g.clsid,
                &g.profile,
                HKL(null_mut()),
                flags,
            )
        };
        tsf_log!(
            "activate",
            "ActivateProfile clsid={} flags=0x{:08x} hr=0x{:08x}",
            clsid_str,
            flags,
            res_hr_u32(&res)
        );
        if res.is_ok() {
            break;
        }
    }

    if let Some(tm) = tm_activated {
        unsafe {
            let _ = tm.Deactivate();
        }
    }
    res
}

pub fn unregister_server() -> windows_core::Result<()> {
    refresh_enabled();
    let g = active_guids();
    let clsid_str = guid_to_braced_string(&g.clsid);
    let _com = ComScope::enter()?;
    let mut cat_err = 0u32;
    if let Ok(cat_mgr) =
        unsafe { CoCreateInstance::<_, ITfCategoryMgr>(&CLSID_TF_CategoryMgr, None, CLSCTX_INPROC_SERVER) }
    {
        for &cat in &CATEGORIES {
            unsafe {
                let r = cat_mgr.UnregisterCategory(&g.clsid, cat, &g.clsid);
                if let Err(e) = r {
                    cat_err = e.code().0 as u32;
                }
            }
        }
    }
    let mut prof_hr = 0u32;
    if let Ok(profile_mgr) = unsafe {
        CoCreateInstance::<_, ITfInputProcessorProfileMgr>(
            &CLSID_TF_InputProcessorProfiles,
            None,
            CLSCTX_INPROC_SERVER,
        )
    } {
        let r = unsafe { profile_mgr.UnregisterProfile(&g.clsid, LANGID_ZH_CN, &g.profile, 0) };
        prof_hr = res_hr_u32(&r);
    }
    delete_clsid_registry(&g.clsid);
    tsf_log!(
        "register",
        "unregister_server clsid={} profile_hr=0x{:08x} last_cat_hr=0x{:08x}",
        clsid_str,
        prof_hr,
        cat_err
    );
    Ok(())
}

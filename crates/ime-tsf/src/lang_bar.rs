//! Windows 10/11 TSF system tray IME mode indicator (`GUID_LBI_INPUTMODE`).
//!
//! Registers an `ITfLangBarItemButton` with `GUID_LBI_INPUTMODE` so the Windows taskbar
//! displays a crisp DPI-aware `中` / `英` mode icon beside the Pyn language icon and lets
//! users click it directly to toggle Chinese/English mode.

use std::cell::RefCell;
use std::ptr::null_mut;

use ime_ipc::pipe::to_wide_null;
use ime_ipc::session::InputMode;
use windows::Win32::Foundation::{COLORREF, E_INVALIDARG, E_POINTER, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    CreateBitmap, CreateCompatibleDC, CreateDIBSection, CreateFontW, DeleteDC, DeleteObject,
    DrawTextW, GdiFlush, SelectObject, SetBkMode, SetTextColor, ANTIALIASED_QUALITY, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_PITCH, DIB_RGB_COLORS,
    DT_CENTER, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, FF_SWISS, FW_MEDIUM, HBITMAP, HFONT,
    OUT_DEFAULT_PRECIS, TRANSPARENT,
};
use windows::Win32::System::Ole::{CONNECT_E_CANNOTCONNECT, CONNECT_E_NOCONNECTION};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows::Win32::UI::HiDpi::{GetDpiForSystem, GetSystemMetricsForDpi};
use windows::Win32::UI::TextServices::{
    ITfLangBarItemButton, ITfLangBarItemButton_Impl, ITfLangBarItemSink, ITfLangBarItem_Impl,
    ITfMenu, ITfSource, ITfSource_Impl, TfLBIClick, GUID_LBI_INPUTMODE, TF_LANGBARITEMINFO,
    TF_LBI_CLK_LEFT, TF_LBI_ICON, TF_LBI_STATUS, TF_LBI_STYLE_BTN_BUTTON,
    TF_LBI_STYLE_SHOWNINTRAY, TF_LBI_TEXT, TF_LBI_TOOLTIP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, HICON, ICONINFO, SM_CXSMICON,
};
use windows_core::{
    implement, BOOL, BSTR, Error, GUID, IUnknown, Interface, PCWSTR, Ref, Result,
};

use crate::globals::{dll_add_ref, dll_release};

const UPDATE_ALL_FLAGS: u32 = TF_LBI_ICON | TF_LBI_STATUS | TF_LBI_TOOLTIP | TF_LBI_TEXT;

struct LangBarInner {
    mode: InputMode,
    chinese_punct: bool,
    next_cookie: u32,
    sinks: Vec<(u32, ITfLangBarItemSink)>,
    on_click_toggle: Option<Box<dyn Fn()>>,
}

#[implement(ITfLangBarItemButton, ITfSource)]
pub struct LangBarModeButton {
    clsid: GUID,
    inner: RefCell<LangBarInner>,
}

impl LangBarModeButton {
    pub fn new(
        clsid: GUID,
        mode: InputMode,
        chinese_punct: bool,
        on_click_toggle: Box<dyn Fn()>,
    ) -> Self {
        dll_add_ref();
        Self {
            clsid,
            inner: RefCell::new(LangBarInner {
                mode,
                chinese_punct,
                next_cookie: 1,
                sinks: Vec::new(),
                on_click_toggle: Some(on_click_toggle),
            }),
        }
    }

    pub fn update_state(&self, mode: InputMode, chinese_punct: bool) {
        let sinks = {
            let mut st = self.inner.borrow_mut();
            st.mode = mode;
            st.chinese_punct = chinese_punct;
            st.sinks.iter().map(|(_, s)| s.clone()).collect::<Vec<_>>()
        };
        for sink in sinks {
            unsafe {
                let _ = sink.OnUpdate(UPDATE_ALL_FLAGS);
            }
        }
    }

    pub fn detach(&self) {
        let mut st = self.inner.borrow_mut();
        st.sinks.clear();
        st.on_click_toggle = None;
    }
}

impl Drop for LangBarModeButton {
    fn drop(&mut self) {
        dll_release();
    }
}

impl ITfLangBarItem_Impl for LangBarModeButton_Impl {
    fn GetInfo(&self, pinfo: *mut TF_LANGBARITEMINFO) -> Result<()> {
        if pinfo.is_null() {
            return Err(Error::from(E_POINTER));
        }
        let mut desc = [0u16; 32];
        for (slot, ch) in desc.iter_mut().take(31).zip("Pyn Input Mode".encode_utf16()) {
            *slot = ch;
        }
        unsafe {
            (*pinfo).clsidService = self.clsid;
            (*pinfo).guidItem = GUID_LBI_INPUTMODE;
            (*pinfo).dwStyle = TF_LBI_STYLE_BTN_BUTTON | TF_LBI_STYLE_SHOWNINTRAY;
            (*pinfo).ulSort = 0;
            (*pinfo).szDescription = desc;
        }
        Ok(())
    }

    fn GetStatus(&self) -> Result<u32> {
        Ok(0)
    }

    fn Show(&self, _fshow: BOOL) -> Result<()> {
        Ok(())
    }

    fn GetTooltipString(&self) -> Result<BSTR> {
        let st = self.inner.borrow();
        let tip = match (st.mode, st.chinese_punct) {
            (InputMode::Chinese, true) => "Pyn 拼音 · 中文（中文标点）",
            (InputMode::Chinese, false) => "Pyn 拼音 · 中文（英文标点）",
            (InputMode::English, _) => "Pyn 拼音 · 英文",
        };
        Ok(BSTR::from(tip))
    }
}

impl ITfLangBarItemButton_Impl for LangBarModeButton_Impl {
    fn OnClick(&self, click: TfLBIClick, _pt: &POINT, _prcarea: *const RECT) -> Result<()> {
        if click == TF_LBI_CLK_LEFT {
            // Take a raw borrow-safe snapshot of whether we can invoke the callback:
            // temporarily move the callback out so re-entrant `update_state` never conflicts.
            let cb = self.inner.borrow_mut().on_click_toggle.take();
            if let Some(f) = cb {
                f();
                let mut st = self.inner.borrow_mut();
                if st.on_click_toggle.is_none() {
                    st.on_click_toggle = Some(f);
                }
            }
        }
        Ok(())
    }

    fn InitMenu(&self, _pmenu: Ref<ITfMenu>) -> Result<()> {
        Ok(())
    }

    fn OnMenuSelect(&self, _wid: u32) -> Result<()> {
        Ok(())
    }

    fn GetIcon(&self) -> Result<HICON> {
        let (mode, chinese_punct) = {
            let st = self.inner.borrow();
            (st.mode, st.chinese_punct)
        };
        let is_light_taskbar = read_system_uses_light_theme();
        create_mode_hicon(mode, chinese_punct, is_light_taskbar)
    }

    fn GetText(&self) -> Result<BSTR> {
        let label = match self.inner.borrow().mode {
            InputMode::Chinese => "中",
            InputMode::English => "英",
        };
        Ok(BSTR::from(label))
    }
}

impl ITfSource_Impl for LangBarModeButton_Impl {
    fn AdviseSink(&self, riid: *const GUID, punk: Ref<IUnknown>) -> Result<u32> {
        if riid.is_null() {
            return Err(Error::from(E_INVALIDARG));
        }
        if unsafe { *riid } != ITfLangBarItemSink::IID {
            return Err(Error::from(CONNECT_E_CANNOTCONNECT));
        }
        let unk = punk.ok()?;
        let sink: ITfLangBarItemSink = unk.cast().map_err(|_| Error::from(CONNECT_E_CANNOTCONNECT))?;
        let mut st = self.inner.borrow_mut();
        let cookie = st.next_cookie;
        st.next_cookie = st.next_cookie.wrapping_add(1).max(1);
        st.sinks.push((cookie, sink));
        Ok(cookie)
    }

    fn UnadviseSink(&self, dwcookie: u32) -> Result<()> {
        let mut st = self.inner.borrow_mut();
        let before = st.sinks.len();
        st.sinks.retain(|(c, _)| *c != dwcookie);
        if st.sinks.len() == before {
            return Err(Error::from(CONNECT_E_NOCONNECTION));
        }
        Ok(())
    }
}

fn read_system_uses_light_theme() -> bool {
    let subkey = to_wide_null(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
    let value = to_wide_null("SystemUsesLightTheme");
    let mut data = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    let err = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            PCWSTR(value.as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut data as *mut u32).cast()),
            Some(&mut size),
        )
    };
    err.is_ok() && data != 0
}

/// Renders a crisp, DPI-aware 32-bit premultiplied ARGB `HICON` (`中` or `英`) for the
/// Windows taskbar tray. Callers (`ITfLangBarItemButton::GetIcon` consumers) own the
/// returned `HICON` and free it via `DestroyIcon`.
pub fn create_mode_hicon(
    mode: InputMode,
    chinese_punct: bool,
    is_light_taskbar: bool,
) -> Result<HICON> {
    let dpi = unsafe { GetDpiForSystem() }.max(96);
    let size = unsafe { GetSystemMetricsForDpi(SM_CXSMICON, dpi) }.clamp(16, 48);

    let hdc = unsafe { CreateCompatibleDC(None) };
    if hdc.is_invalid() {
        return Err(Error::from_thread());
    }

    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size,
            biHeight: -size,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };

    let mut bits_ptr: *mut std::ffi::c_void = null_mut();
    let hbm_color: HBITMAP =
        unsafe { CreateDIBSection(Some(hdc), &bmi, DIB_RGB_COLORS, &mut bits_ptr, None, 0)? };
    if hbm_color.is_invalid() || bits_ptr.is_null() {
        unsafe {
            let _ = DeleteDC(hdc);
        }
        return Err(Error::from_thread());
    }

    let old_bmp = unsafe { SelectObject(hdc, hbm_color.into()) };
    let pixel_count = (size * size) as usize;
    let pixels = unsafe { std::slice::from_raw_parts_mut(bits_ptr as *mut u32, pixel_count) };
    pixels.fill(0);

    let font_h = -((size as f32 * 0.86).round() as i32).max(12);
    let face = to_wide_null("Microsoft YaHei UI");
    let hfont: HFONT = unsafe {
        CreateFontW(
            font_h,
            0,
            0,
            0,
            FW_MEDIUM.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            ANTIALIASED_QUALITY,
            (DEFAULT_PITCH.0 | FF_SWISS.0) as u32,
            PCWSTR(face.as_ptr()),
        )
    };
    let old_font = if !hfont.is_invalid() {
        Some(unsafe { SelectObject(hdc, hfont.into()) })
    } else {
        None
    };

    let glyph = match mode {
        InputMode::Chinese => "中",
        InputMode::English => "英",
    };
    let mut glyph_w: Vec<u16> = glyph.encode_utf16().collect();
    let mut rc = RECT {
        left: 0,
        top: 0,
        right: size,
        bottom: size,
    };

    unsafe {
        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, COLORREF(0x00FF_FFFF));
        let _ = DrawTextW(
            hdc,
            &mut glyph_w,
            &mut rc,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );
        let _ = GdiFlush();
    }

    let (fg_r, fg_g, fg_b): (u32, u32, u32) = if is_light_taskbar {
        (0x17, 0x17, 0x17)
    } else {
        (0xFF, 0xFF, 0xFF)
    };

    for px in pixels.iter_mut() {
        let cov = ((*px >> 8) & 0xFF) as f32 / 255.0;
        if cov <= 0.002 {
            *px = 0;
        } else {
            let alpha = (cov.powf(0.75) * 255.0).round().clamp(0.0, 255.0) as u32;
            let r = (fg_r * alpha + 127) / 255;
            let g = (fg_g * alpha + 127) / 255;
            let b = (fg_b * alpha + 127) / 255;
            *px = (alpha << 24) | (r << 16) | (g << 8) | b;
        }
    }

    // Subtle bottom-right dot when Chinese mode is using half-width English punctuation.
    if mode == InputMode::Chinese && !chinese_punct {
        let dot = (size / 7).clamp(2, 5);
        let x0 = size - dot;
        let y0 = size - dot;
        let solid = (0xFFu32 << 24) | (fg_r << 16) | (fg_g << 8) | fg_b;
        for y in y0..size {
            let row = (y * size) as usize;
            for x in x0..size {
                pixels[row + x as usize] = solid;
            }
        }
    }

    if let Some(prev_f) = old_font {
        unsafe {
            let _ = SelectObject(hdc, prev_f);
            let _ = DeleteObject(hfont.into());
        }
    }
    unsafe {
        let _ = SelectObject(hdc, old_bmp);
        let _ = DeleteDC(hdc);
    }

    let hbm_mask: HBITMAP = unsafe { CreateBitmap(size, size, 1, 1, None) };
    let icon_info = ICONINFO {
        fIcon: BOOL(1),
        xHotspot: 0,
        yHotspot: 0,
        hbmMask: hbm_mask,
        hbmColor: hbm_color,
    };
    let res = unsafe { CreateIconIndirect(&icon_info) };

    unsafe {
        if !hbm_mask.is_invalid() {
            let _ = DeleteObject(hbm_mask.into());
        }
        let _ = DeleteObject(hbm_color.into());
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;
    use windows::Win32::UI::TextServices::{ITfLangBarItem, ITfLangBarItemSink_Impl};
    use windows::Win32::UI::WindowsAndMessaging::DestroyIcon;
    use windows_core::ComObject;

    #[implement(ITfLangBarItemSink)]
    struct MockSink {
        updates: Rc<Cell<u32>>,
    }

    impl ITfLangBarItemSink_Impl for MockSink_Impl {
        fn OnUpdate(&self, dwflags: u32) -> Result<()> {
            self.updates.set(self.updates.get() + 1);
            assert_ne!(dwflags & TF_LBI_ICON, 0);
            Ok(())
        }
    }

    #[test]
    fn lang_bar_mode_button_interfaces_icons_and_click_toggle() {
        let clicks = Rc::new(Cell::new(0u32));
        let clicks_cb = Rc::clone(&clicks);
        let clsid = crate::guids::active_guids().clsid;
        let btn = ComObject::new(LangBarModeButton::new(
            clsid,
            InputMode::Chinese,
            true,
            Box::new(move || clicks_cb.set(clicks_cb.get() + 1)),
        ));

        let button: ITfLangBarItemButton = btn.to_interface();
        let item: ITfLangBarItem = button.clone().into();
        let source: ITfSource = btn.to_interface();

        let mut info = TF_LANGBARITEMINFO::default();
        unsafe { item.GetInfo(&mut info).unwrap() };
        assert_eq!(info.clsidService, clsid);
        assert_eq!(info.guidItem, GUID_LBI_INPUTMODE);
        assert_ne!(info.dwStyle & TF_LBI_STYLE_BTN_BUTTON, 0);
        assert_ne!(info.dwStyle & TF_LBI_STYLE_SHOWNINTRAY, 0);
        assert_eq!(unsafe { item.GetStatus().unwrap() }, 0);
        assert_eq!(unsafe { button.GetText().unwrap() }.to_string(), "中");
        assert!(unsafe { item.GetTooltipString().unwrap() }
            .to_string()
            .contains("中文"));

        // Advise sink and verify update notifications fire cleanly.
        let updates = Rc::new(Cell::new(0u32));
        let sink: ITfLangBarItemSink = MockSink {
            updates: Rc::clone(&updates),
        }
        .into();
        let cookie = unsafe { source.AdviseSink(&ITfLangBarItemSink::IID, &sink).unwrap() };

        // Generate HICONs for Chinese, Chinese(en-punct), and English in both dark/light themes.
        for (mode, punct, light) in [
            (InputMode::Chinese, true, false),
            (InputMode::Chinese, false, false),
            (InputMode::English, true, false),
            (InputMode::Chinese, true, true),
            (InputMode::English, true, true),
        ] {
            let hicon = create_mode_hicon(mode, punct, light).unwrap();
            assert!(!hicon.is_invalid());
            unsafe {
                let _ = DestroyIcon(hicon);
            }
        }

        // Update state -> sink notified, GetText & GetIcon reflect English.
        btn.update_state(InputMode::English, true);
        assert_eq!(updates.get(), 1);
        assert_eq!(unsafe { button.GetText().unwrap() }.to_string(), "英");
        let hicon = unsafe { button.GetIcon().unwrap() };
        assert!(!hicon.is_invalid());
        unsafe {
            let _ = DestroyIcon(hicon);
        }

        // Left-click invokes callback cleanly.
        let pt = POINT { x: 0, y: 0 };
        let rc = RECT::default();
        unsafe { button.OnClick(TF_LBI_CLK_LEFT, pt, &rc).unwrap() };
        assert_eq!(clicks.get(), 1);

        unsafe { source.UnadviseSink(cookie).unwrap() };
        btn.detach();
    }
}

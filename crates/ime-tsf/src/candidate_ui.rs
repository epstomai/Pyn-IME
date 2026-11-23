//! Floating Windows 11 DPI-aware GDI candidate window styled after Vercel's Geist UI
//! (true monochrome Light/Dark themes, modern CJK typography, SDF-antialiased pills, native DWM rounded border).

use std::cell::Cell;
use std::collections::HashSet;
use std::ptr::null_mut;
use std::sync::{Once, OnceLock};

use ime_ipc::config::{CandidateLayout, HighlightStyle, ImeConfig, ThemeMode};
use ime_ipc::pipe::to_wide_null;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    DWM_WINDOW_CORNER_PREFERENCE,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, ClientToScreen, CreateCompatibleDC, CreateDIBSection, CreateFontW,
    DeleteDC, DeleteObject, DrawTextW, EndPaint, EnumFontFamiliesExW, GdiFlush, GetDC,
    GetMonitorInfoW, GetTextExtentPoint32W, InvalidateRect, MonitorFromPoint, ReleaseDC,
    SelectObject, SetBkMode, SetTextColor, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_PITCH, DIB_RGB_COLORS,
    DT_CENTER, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_VCENTER,
    FF_SWISS, HDC, HFONT, LOGFONTW, MONITORINFO, MONITOR_DEFAULTTONEAREST, OUT_DEFAULT_PRECIS,
    PAINTSTRUCT, SRCCOPY, TEXTMETRICW, TRANSPARENT,
};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetCursorPos,
    GetGUIThreadInfo, KillTimer, RegisterClassExW, SetTimer, SetWindowPos, ShowWindow, CS_HREDRAW,
    CS_VREDRAW, GUITHREADINFO, GWLP_USERDATA, HWND_TOPMOST, SWP_NOACTIVATE, SWP_SHOWWINDOW,
    SW_HIDE, WM_PAINT, WM_TIMER, WNDCLASSEXW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_POPUP,
};
use windows_core::PCWSTR;

use crate::globals::dll_hinstance;
use crate::guids::active_guids;

const TOAST_TIMER_ID: usize = 1;
const CARET_TIMER_ID: usize = 2;
const CARET_BLINK_MS: u32 = 530;
static REGISTER_CLASS_ONCE: Once = Once::new();
static WINDOW_CLASS_NAME: OnceLock<String> = OnceLock::new();
static DETECTED_CJK_FONT: OnceLock<String> = OnceLock::new();

fn window_class_name() -> &'static str {
    WINDOW_CLASS_NAME
        .get_or_init(|| {
            let gen = active_guids().generation;
            if gen == 0 {
                "InputEngineCandidateWnd".to_owned()
            } else {
                format!("InputEngineCandidateWnd_g{gen:04x}")
            }
        })
        .as_str()
}

const PREFERRED_CJK_FONTS: &[&str] = &[
    "MiSans",
    "OPPO Sans 4.0",
    "HarmonyOS Sans SC",
    "Noto Sans SC",
    "Source Han Sans",
    "思源黑体",
    "Microsoft YaHei",
    "Microsoft YaHei UI",
];

#[derive(Debug, Clone, Default)]
pub struct CandidateViewState {
    pub preedit: String,
    pub mode_label: String,
    pub page_text: String,
    pub candidates: Vec<String>,
    pub selected_in_page: usize,
    pub is_toast: bool,
}

#[derive(Debug, Clone)]
struct PanelRenderState {
    view: CandidateViewState,
    config: ImeConfig,
    draw_fallback_border: bool,
    caret_visible: Cell<bool>,
}

impl Default for PanelRenderState {
    fn default() -> Self {
        Self {
            view: CandidateViewState::default(),
            config: ImeConfig::default(),
            draw_fallback_border: false,
            caret_visible: Cell::new(true),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rgb(u8, u8, u8);

impl Rgb {
    #[inline]
    const fn to_colorref(self) -> COLORREF {
        COLORREF((self.0 as u32) | ((self.1 as u32) << 8) | ((self.2 as u32) << 16))
    }

    #[inline]
    const fn to_bgra(self) -> u32 {
        0xFF00_0000 | ((self.0 as u32) << 16) | ((self.1 as u32) << 8) | (self.2 as u32)
    }
}

#[derive(Debug, Clone, Copy)]
struct GeistPalette {
    bg: Rgb,
    border: Rgb,
    divider: Rgb,
    preedit: Rgb,
    caret: Rgb,
    text: Rgb,
    index: Rgb,
    muted: Rgb,
    sel_bg: Rgb,
    sel_border: Option<Rgb>,
    sel_fg: Rgb,
    sel_index: Rgb,
    badge_zh_bg: Rgb,
    badge_zh_border: Rgb,
    badge_zh_fg: Rgb,
    badge_en_bg: Rgb,
    badge_en_border: Rgb,
    badge_en_fg: Rgb,
    toast_bg: Rgb,
    toast_fg: Rgb,
}

fn resolve_is_light(theme: ThemeMode) -> bool {
    match theme {
        ThemeMode::Auto => read_apps_use_light_theme(),
        ThemeMode::Dark => false,
        ThemeMode::Light => true,
    }
}

fn palette_for(theme: ThemeMode, highlight: HighlightStyle) -> GeistPalette {
    let is_light = resolve_is_light(theme);
    match (is_light, highlight) {
        (true, HighlightStyle::Subtle) => GeistPalette {
            bg: Rgb(0xFF, 0xFF, 0xFF),
            border: Rgb(0xE5, 0xE5, 0xE5),
            divider: Rgb(0xEB, 0xEB, 0xEB),
            preedit: Rgb(0x17, 0x17, 0x17),
            caret: Rgb(0x17, 0x17, 0x17),
            text: Rgb(0x66, 0x66, 0x66),
            index: Rgb(0xA3, 0xA3, 0xA3),
            muted: Rgb(0x8A, 0x8A, 0x8A),
            sel_bg: Rgb(0xF4, 0xF4, 0xF5),
            sel_border: Some(Rgb(0xD4, 0xD4, 0xD8)),
            sel_fg: Rgb(0x0A, 0x0A, 0x0A),
            sel_index: Rgb(0x73, 0x73, 0x73),
            badge_zh_bg: Rgb(0xF5, 0xF5, 0xF5),
            badge_zh_border: Rgb(0xE5, 0xE5, 0xE5),
            badge_zh_fg: Rgb(0x52, 0x52, 0x52),
            badge_en_bg: Rgb(0x17, 0x17, 0x17),
            badge_en_border: Rgb(0x17, 0x17, 0x17),
            badge_en_fg: Rgb(0xFF, 0xFF, 0xFF),
            toast_bg: Rgb(0xFF, 0xFF, 0xFF),
            toast_fg: Rgb(0x17, 0x17, 0x17),
        },
        (true, HighlightStyle::Inverted) => GeistPalette {
            bg: Rgb(0xFF, 0xFF, 0xFF),
            border: Rgb(0xE5, 0xE5, 0xE5),
            divider: Rgb(0xEB, 0xEB, 0xEB),
            preedit: Rgb(0x17, 0x17, 0x17),
            caret: Rgb(0x17, 0x17, 0x17),
            text: Rgb(0x40, 0x40, 0x40),
            index: Rgb(0xA3, 0xA3, 0xA3),
            muted: Rgb(0x8A, 0x8A, 0x8A),
            sel_bg: Rgb(0x17, 0x17, 0x17),
            sel_border: None,
            sel_fg: Rgb(0xFF, 0xFF, 0xFF),
            sel_index: Rgb(0xA3, 0xA3, 0xA3),
            badge_zh_bg: Rgb(0xF5, 0xF5, 0xF5),
            badge_zh_border: Rgb(0xE5, 0xE5, 0xE5),
            badge_zh_fg: Rgb(0x52, 0x52, 0x52),
            badge_en_bg: Rgb(0x17, 0x17, 0x17),
            badge_en_border: Rgb(0x17, 0x17, 0x17),
            badge_en_fg: Rgb(0xFF, 0xFF, 0xFF),
            toast_bg: Rgb(0xFF, 0xFF, 0xFF),
            toast_fg: Rgb(0x17, 0x17, 0x17),
        },
        (false, HighlightStyle::Subtle) => GeistPalette {
            bg: Rgb(0x0A, 0x0A, 0x0A),
            border: Rgb(0x2E, 0x2E, 0x2E),
            divider: Rgb(0x1F, 0x1F, 0x1F),
            preedit: Rgb(0xFA, 0xFA, 0xFA),
            caret: Rgb(0xFA, 0xFA, 0xFA),
            text: Rgb(0xA3, 0xA3, 0xA3),
            index: Rgb(0x5C, 0x5C, 0x5C),
            muted: Rgb(0x73, 0x73, 0x73),
            sel_bg: Rgb(0x1F, 0x1F, 0x1F),
            sel_border: Some(Rgb(0x3A, 0x3A, 0x3A)),
            sel_fg: Rgb(0xFF, 0xFF, 0xFF),
            sel_index: Rgb(0xA3, 0xA3, 0xA3),
            badge_zh_bg: Rgb(0x17, 0x17, 0x17),
            badge_zh_border: Rgb(0x2E, 0x2E, 0x2E),
            badge_zh_fg: Rgb(0xA3, 0xA3, 0xA3),
            badge_en_bg: Rgb(0x26, 0x26, 0x26),
            badge_en_border: Rgb(0x45, 0x45, 0x45),
            badge_en_fg: Rgb(0xFA, 0xFA, 0xFA),
            toast_bg: Rgb(0x0A, 0x0A, 0x0A),
            toast_fg: Rgb(0xFA, 0xFA, 0xFA),
        },
        (false, HighlightStyle::Inverted) => GeistPalette {
            bg: Rgb(0x0A, 0x0A, 0x0A),
            border: Rgb(0x2E, 0x2E, 0x2E),
            divider: Rgb(0x1F, 0x1F, 0x1F),
            preedit: Rgb(0xFA, 0xFA, 0xFA),
            caret: Rgb(0xFA, 0xFA, 0xFA),
            text: Rgb(0xD4, 0xD4, 0xD4),
            index: Rgb(0x66, 0x66, 0x66),
            muted: Rgb(0x73, 0x73, 0x73),
            sel_bg: Rgb(0xF5, 0xF5, 0xF5),
            sel_border: None,
            sel_fg: Rgb(0x0A, 0x0A, 0x0A),
            sel_index: Rgb(0x66, 0x66, 0x66),
            badge_zh_bg: Rgb(0x17, 0x17, 0x17),
            badge_zh_border: Rgb(0x2E, 0x2E, 0x2E),
            badge_zh_fg: Rgb(0xA3, 0xA3, 0xA3),
            badge_en_bg: Rgb(0x26, 0x26, 0x26),
            badge_en_border: Rgb(0x45, 0x45, 0x45),
            badge_en_fg: Rgb(0xFA, 0xFA, 0xFA),
            toast_bg: Rgb(0x0A, 0x0A, 0x0A),
            toast_fg: Rgb(0xFA, 0xFA, 0xFA),
        },
    }
}

fn read_apps_use_light_theme() -> bool {
    let subkey = to_wide_null(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
    let value = to_wide_null("AppsUseLightTheme");
    let mut data = 1u32;
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
    if err.is_ok() {
        data != 0
    } else {
        true
    }
}

unsafe extern "system" fn collect_font_proc(
    lpelfe: *const LOGFONTW,
    _lpntme: *const TEXTMETRICW,
    _font_type: u32,
    lparam: LPARAM,
) -> i32 {
    if lpelfe.is_null() || lparam.0 == 0 {
        return 1;
    }
    let lf = &*lpelfe;
    let end = lf
        .lfFaceName
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(lf.lfFaceName.len());
    let name = String::from_utf16_lossy(&lf.lfFaceName[..end]);
    if !name.is_empty() && !name.starts_with('@') {
        let set = &mut *(lparam.0 as *mut HashSet<String>);
        set.insert(name);
    }
    1
}

fn enumerate_installed_fonts() -> HashSet<String> {
    let mut set = HashSet::new();
    unsafe {
        let hdc = GetDC(None);
        if !hdc.is_invalid() {
            let lf = LOGFONTW {
                lfCharSet: DEFAULT_CHARSET,
                ..Default::default()
            };
            let _ = EnumFontFamiliesExW(
                hdc,
                &lf,
                Some(collect_font_proc),
                LPARAM(&mut set as *mut HashSet<String> as isize),
                0,
            );
            ReleaseDC(None, hdc);
        }
    }
    set
}

pub fn detect_best_cjk_font() -> &'static str {
    DETECTED_CJK_FONT
        .get_or_init(|| {
            let installed = enumerate_installed_fonts();
            for &candidate in PREFERRED_CJK_FONTS {
                if installed.contains(candidate) {
                    return candidate.to_owned();
                }
            }
            "Microsoft YaHei UI".to_owned()
        })
        .as_str()
}

fn resolve_font_face<'a>(cfg: &'a ImeConfig) -> &'a str {
    let trimmed = cfg.font_family.trim();
    if trimmed.is_empty() {
        detect_best_cjk_font()
    } else {
        trimmed
    }
}

#[cfg(target_pointer_width = "64")]
unsafe fn get_window_user_data(hwnd: HWND) -> isize {
    windows::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(hwnd, GWLP_USERDATA)
}

#[cfg(target_pointer_width = "32")]
unsafe fn get_window_user_data(hwnd: HWND) -> isize {
    windows::Win32::UI::WindowsAndMessaging::GetWindowLongW(hwnd, GWLP_USERDATA) as isize
}

#[cfg(target_pointer_width = "64")]
unsafe fn set_window_user_data(hwnd: HWND, value: isize) {
    windows::Win32::UI::WindowsAndMessaging::SetWindowLongPtrW(hwnd, GWLP_USERDATA, value);
}

#[cfg(target_pointer_width = "32")]
unsafe fn set_window_user_data(hwnd: HWND, value: isize) {
    windows::Win32::UI::WindowsAndMessaging::SetWindowLongW(hwnd, GWLP_USERDATA, value as i32);
}

pub struct CandidateWindow {
    hwnd: HWND,
    state: Box<PanelRenderState>,
    last_dwm_border: Option<u32>,
}

impl CandidateWindow {
    pub fn new() -> windows_core::Result<Self> {
        let hinst = dll_hinstance();
        let class_name = window_class_name();
        REGISTER_CLASS_ONCE.call_once(|| {
            let class_w = to_wide_null(class_name);
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(wnd_proc),
                hInstance: hinst,
                lpszClassName: PCWSTR(class_w.as_ptr()),
                ..Default::default()
            };
            unsafe {
                RegisterClassExW(&wc);
            }
        });

        let class_w = to_wide_null(class_name);
        let title_w = to_wide_null("Input Engine");
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_NOACTIVATE | WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                PCWSTR(class_w.as_ptr()),
                PCWSTR(title_w.as_ptr()),
                WS_POPUP,
                0,
                0,
                200,
                40,
                None,
                None,
                Some(hinst),
                None,
            )?
        };

        let mut state = Box::new(PanelRenderState::default());
        let pal = palette_for(state.config.theme, state.config.highlight_style);
        let border_colorref = pal.border.to_colorref().0;
        let dwm_border_ok = unsafe {
            let corner: DWM_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                (&corner as *const DWM_WINDOW_CORNER_PREFERENCE).cast(),
                std::mem::size_of_val(&corner) as u32,
            );
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_BORDER_COLOR,
                (&border_colorref as *const u32).cast(),
                std::mem::size_of_val(&border_colorref) as u32,
            )
            .is_ok()
        };
        state.draw_fallback_border = !dwm_border_ok;

        let ptr = (&*state as *const PanelRenderState) as isize;
        unsafe {
            set_window_user_data(hwnd, ptr);
        }
        Ok(Self {
            hwnd,
            state,
            last_dwm_border: dwm_border_ok.then_some(border_colorref),
        })
    }

    pub fn set_config(&mut self, cfg: ImeConfig) {
        self.state.config = cfg;
        self.sync_dwm_border();
    }

    fn sync_dwm_border(&mut self) {
        let pal = palette_for(self.state.config.theme, self.state.config.highlight_style);
        let border_colorref = pal.border.to_colorref().0;
        if self.last_dwm_border == Some(border_colorref) {
            return;
        }
        let ok = unsafe {
            DwmSetWindowAttribute(
                self.hwnd,
                DWMWA_BORDER_COLOR,
                (&border_colorref as *const u32).cast(),
                std::mem::size_of_val(&border_colorref) as u32,
            )
            .is_ok()
        };
        self.state.draw_fallback_border = !ok;
        self.last_dwm_border = ok.then_some(border_colorref);
    }

    pub fn update_and_show(&mut self, view: CandidateViewState, caret_rect: Option<RECT>) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TOAST_TIMER_ID);
        }
        self.state.caret_visible.set(true);
        self.sync_dwm_border();
        self.state.view = view;
        let state_ptr = (&*self.state as *const PanelRenderState) as isize;
        unsafe {
            set_window_user_data(self.hwnd, state_ptr);
        }

        let dpi = unsafe { GetDpiForWindow(self.hwnd) };
        let scale = if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 };
        let px = |v: i32| ((v as f32) * scale).round() as i32;

        let (anchor_x, anchor_top, anchor_bottom, work) = monitor_work_rect(caret_rect, &px);
        let max_win_w = (work.right - work.left - px(24)).max(px(320));
        let (win_w, win_h) = measure_window_size(
            self.hwnd,
            &self.state.view,
            &self.state.config,
            max_win_w,
            &px,
        );
        let (x, y) = compute_window_pos_in_work(
            anchor_x,
            anchor_top,
            anchor_bottom,
            work,
            win_w,
            win_h,
            &px,
        );

        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                x,
                y,
                win_w,
                win_h,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            let _ = InvalidateRect(Some(self.hwnd), None, false);
            if self.state.config.show_preedit_caret && !self.state.view.preedit.is_empty() {
                let _ = SetTimer(Some(self.hwnd), CARET_TIMER_ID, CARET_BLINK_MS, None);
            } else {
                let _ = KillTimer(Some(self.hwnd), CARET_TIMER_ID);
            }
        }
    }

    pub fn show_mode_toast(&mut self, label: &str, caret_rect: Option<RECT>) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TOAST_TIMER_ID);
            let _ = KillTimer(Some(self.hwnd), CARET_TIMER_ID);
        }
        self.sync_dwm_border();
        self.state.view = CandidateViewState {
            preedit: label.to_owned(),
            is_toast: true,
            ..Default::default()
        };
        let state_ptr = (&*self.state as *const PanelRenderState) as isize;
        unsafe {
            set_window_user_data(self.hwnd, state_ptr);
        }

        let dpi = unsafe { GetDpiForWindow(self.hwnd) };
        let scale = if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 };
        let px = |v: i32| ((v as f32) * scale).round() as i32;

        let (anchor_x, anchor_top, anchor_bottom, work) = monitor_work_rect(caret_rect, &px);
        let (win_w, win_h) = measure_toast_size(self.hwnd, label, &self.state.config, &px);
        let (x, y) = compute_window_pos_in_work(
            anchor_x,
            anchor_top,
            anchor_bottom,
            work,
            win_w,
            win_h,
            &px,
        );

        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                x,
                y,
                win_w,
                win_h,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            let _ = InvalidateRect(Some(self.hwnd), None, false);
            let _ = SetTimer(Some(self.hwnd), TOAST_TIMER_ID, 850, None);
        }
    }

    pub fn hide(&self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TOAST_TIMER_ID);
            let _ = KillTimer(Some(self.hwnd), CARET_TIMER_ID);
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }
}

impl Drop for CandidateWindow {
    fn drop(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TOAST_TIMER_ID);
            let _ = KillTimer(Some(self.hwnd), CARET_TIMER_ID);
            set_window_user_data(self.hwnd, 0);
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

struct PanelFonts {
    preedit: HFONT,
    cand: HFONT,
    cand_sel: HFONT,
    index: HFONT,
    meta: HFONT,
    badge: HFONT,
    toast: HFONT,
}

impl PanelFonts {
    unsafe fn create(cfg: &ImeConfig, px: &impl Fn(i32) -> i32) -> Self {
        let face = resolve_font_face(cfg);
        let base_sz = cfg.font_size.clamp(12, 22);
        let pre_sz = (base_sz - 1).max(12);
        let small_sz = (base_sz - 4).clamp(10, 14);
        let toast_sz = (base_sz - 1).clamp(13, 18);
        let sel_weight = cfg.selected_weight.clamp(400, 700) as i32;
        Self {
            preedit: create_font(face, px(pre_sz), 400),
            cand: create_font(face, px(base_sz), 400),
            cand_sel: create_font(face, px(base_sz), sel_weight),
            index: create_font("Segoe UI Variable Text", px(small_sz), 500),
            meta: create_font("Segoe UI Variable Text", px(small_sz), 500),
            badge: create_font(face, px(small_sz), 400),
            toast: create_font(face, px(toast_sz), 400),
        }
    }

    unsafe fn destroy(self) {
        let _ = DeleteObject(self.preedit.into());
        let _ = DeleteObject(self.cand.into());
        let _ = DeleteObject(self.cand_sel.into());
        let _ = DeleteObject(self.index.into());
        let _ = DeleteObject(self.meta.into());
        let _ = DeleteObject(self.badge.into());
        let _ = DeleteObject(self.toast.into());
    }
}

struct CandidateItemLayout {
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    idx_w: i32,
}

struct PanelLayout {
    header_pad_x: i32,
    header_top: i32,
    header_h: i32,
    div_y: i32,
    row_pad_x: i32,
    item_pad_x: i32,
    idx_gap: i32,
    preedit_w: i32,
    page_w: i32,
    badge_w: i32,
    badge_h: i32,
    badge_gap: i32,
    items: Vec<CandidateItemLayout>,
    win_w: i32,
    win_h: i32,
}

unsafe fn compute_panel_layout(
    hdc: HDC,
    state: &CandidateViewState,
    cfg: &ImeConfig,
    max_win_w: i32,
    fonts: &PanelFonts,
    px: &impl Fn(i32) -> i32,
) -> PanelLayout {
    let base_sz = cfg.font_size.clamp(12, 22);
    let header_pad_x = px(11);
    let header_top = px(6);
    let header_h = px((base_sz + 7).max(22));
    let header_bottom_pad = px(6);
    let div_y = header_top + header_h + header_bottom_pad;
    let row_pad_x = px(6);
    let row_pad_top = px(6);
    let row_pad_bottom = px(6);
    let row_y = div_y + 1 + row_pad_top;
    let row_h = px((base_sz + 13).max(27));
    let item_pad_x = px(9);
    let idx_gap = px(5);
    let horiz_gap = px(3);
    let vert_gap = px(2);

    let badge_h = px((base_sz + 4).clamp(18, 24));
    let badge_gap = px(8);

    let old_font = SelectObject(hdc, fonts.preedit.into());
    let preedit_w = text_width(hdc, &state.preedit);

    SelectObject(hdc, fonts.meta.into());
    let page_w = text_width(hdc, &state.page_text);

    let badge_w = if state.mode_label.is_empty() {
        0
    } else {
        SelectObject(hdc, fonts.badge.into());
        (text_width(hdc, &state.mode_label) + px(10)).max(px(21))
    };

    let right_meta_w = if page_w > 0 && badge_w > 0 {
        page_w + badge_gap + badge_w
    } else {
        page_w + badge_w
    };
    let caret_extra = if cfg.show_preedit_caret && !state.preedit.is_empty() {
        px(6)
    } else {
        0
    };
    let header_content_w = preedit_w + caret_extra + px(14) + right_meta_w;
    let header_total_w = header_content_w + header_pad_x * 2;
    let max_w = max_win_w.clamp(px(320), px(960));

    let mut items = Vec::with_capacity(state.candidates.len());
    let (win_w, win_h) = if cfg.layout == CandidateLayout::Vertical && !state.candidates.is_empty()
    {
        let mut widest_item = 0i32;
        let mut idx_widths = Vec::with_capacity(state.candidates.len());
        for (i, cand) in state.candidates.iter().enumerate() {
            let idx_str = (i + 1).to_string();
            SelectObject(hdc, fonts.index.into());
            let idx_w = text_width(hdc, &idx_str);

            SelectObject(hdc, fonts.cand_sel.into());
            let cand_w = text_width(hdc, cand);
            widest_item = widest_item.max(item_pad_x * 2 + idx_w + idx_gap + cand_w);
            idx_widths.push(idx_w);
        }
        let win_w = header_total_w
            .max(widest_item + row_pad_x * 2)
            .clamp(px(176), max_w);
        let item_x0 = row_pad_x;
        let item_x1 = (win_w - row_pad_x).max(item_x0);
        let mut cur_y = row_y;
        for idx_w in idx_widths {
            items.push(CandidateItemLayout {
                x0: item_x0,
                y0: cur_y,
                x1: item_x1,
                y1: cur_y + row_h,
                idx_w,
            });
            cur_y += row_h + vert_gap;
        }
        let last_y1 = items.last().map(|it| it.y1).unwrap_or(row_y);
        (win_w, last_y1 + row_pad_bottom)
    } else {
        if !state.candidates.is_empty() {
            let mut cur_x = row_pad_x;
            for (i, cand) in state.candidates.iter().enumerate() {
                let idx_str = (i + 1).to_string();
                SelectObject(hdc, fonts.index.into());
                let idx_w = text_width(hdc, &idx_str);

                SelectObject(hdc, fonts.cand_sel.into());
                let cand_w = text_width(hdc, cand);

                let item_w = item_pad_x * 2 + idx_w + idx_gap + cand_w;
                let x0 = cur_x;
                let x1 = x0 + item_w;
                items.push(CandidateItemLayout {
                    x0,
                    y0: row_y,
                    x1,
                    y1: row_y + row_h,
                    idx_w,
                });
                cur_x = x1 + horiz_gap;
            }
        }
        let cands_total_w = items.last().map(|it| it.x1 + row_pad_x).unwrap_or(0);
        let win_w = header_total_w.max(cands_total_w).clamp(px(136), max_w);
        let win_h = if state.candidates.is_empty() {
            header_top * 2 + header_h
        } else {
            row_y + row_h + row_pad_bottom
        };
        (win_w, win_h)
    };

    SelectObject(hdc, old_font);

    PanelLayout {
        header_pad_x,
        header_top,
        header_h,
        div_y,
        row_pad_x,
        item_pad_x,
        idx_gap,
        preedit_w,
        page_w,
        badge_w,
        badge_h,
        badge_gap,
        items,
        win_w,
        win_h,
    }
}

fn measure_window_size(
    hwnd: HWND,
    state: &CandidateViewState,
    cfg: &ImeConfig,
    max_win_w: i32,
    px: &impl Fn(i32) -> i32,
) -> (i32, i32) {
    unsafe {
        let hdc = GetDC(Some(hwnd));
        if !hdc.is_invalid() {
            let fonts = PanelFonts::create(cfg, px);
            let layout = compute_panel_layout(hdc, state, cfg, max_win_w, &fonts, px);
            fonts.destroy();
            ReleaseDC(Some(hwnd), hdc);
            return (layout.win_w, layout.win_h);
        }
    }

    let base_sz = cfg.font_size.clamp(12, 22);
    let header_top = px(6);
    let header_h = px((base_sz + 7).max(22));
    let row_h = px((base_sz + 13).max(27));
    let win_w = px(156).min(max_win_w.clamp(px(320), px(960)));
    let win_h = if state.candidates.is_empty() {
        header_top * 2 + header_h
    } else {
        header_top + header_h + px(6) + 1 + px(6) + row_h + px(6)
    };
    (win_w, win_h)
}

fn measure_toast_size(
    hwnd: HWND,
    label: &str,
    cfg: &ImeConfig,
    px: &impl Fn(i32) -> i32,
) -> (i32, i32) {
    let toast_sz = (cfg.font_size.clamp(12, 22) - 1).clamp(13, 18);
    unsafe {
        let hdc = GetDC(Some(hwnd));
        if !hdc.is_invalid() {
            let font = create_font(resolve_font_face(cfg), px(toast_sz), 400);
            let old = SelectObject(hdc, font.into());
            let tw = text_width(hdc, label);
            SelectObject(hdc, old);
            let _ = DeleteObject(font.into());
            ReleaseDC(Some(hwnd), hdc);
            return ((tw + px(18)).max(px(38)), px(32));
        }
    }
    (px(38), px(32))
}

fn gui_caret_anchor(margin: i32, fallback_h: i32) -> Option<(i32, i32, i32)> {
    let mut gti = GUITHREADINFO {
        cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    unsafe {
        if GetGUIThreadInfo(0, &mut gti).is_err() || gti.hwndCaret.0.is_null() {
            return None;
        }
        if gti.rcCaret.bottom <= gti.rcCaret.top && gti.rcCaret.right <= gti.rcCaret.left {
            return None;
        }
        let mut tl = POINT {
            x: gti.rcCaret.left,
            y: gti.rcCaret.top,
        };
        let mut br = POINT {
            x: gti.rcCaret.right,
            y: gti.rcCaret.bottom,
        };
        if !ClientToScreen(gti.hwndCaret, &mut tl).as_bool()
            || !ClientToScreen(gti.hwndCaret, &mut br).as_bool()
        {
            return None;
        }
        let bottom = if br.y > tl.y { br.y } else { tl.y + fallback_h };
        Some((tl.x, tl.y, bottom + margin))
    }
}

fn monitor_work_rect(
    caret_rect: Option<RECT>,
    px: &impl Fn(i32) -> i32,
) -> (i32, i32, i32, RECT) {
    let margin = px(6);
    let (anchor_x, anchor_top, anchor_bottom) = match caret_rect {
        Some(rc) => (rc.left, rc.top, rc.bottom + margin),
        None => match gui_caret_anchor(margin, px(18)) {
            Some(anchor) => anchor,
            None => {
                let mut pt = POINT::default();
                unsafe {
                    let _ = GetCursorPos(&mut pt);
                }
                (pt.x, pt.y - px(20), pt.y + px(18))
            }
        },
    };

    let hmon = unsafe {
        MonitorFromPoint(
            POINT {
                x: anchor_x,
                y: anchor_bottom,
            },
            MONITOR_DEFAULTTONEAREST,
        )
    };
    let mut mi = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let ok = unsafe { GetMonitorInfoW(hmon, &mut mi).as_bool() };
    let work = if ok {
        mi.rcWork
    } else {
        RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        }
    };
    (anchor_x, anchor_top, anchor_bottom, work)
}

fn compute_window_pos_in_work(
    anchor_x: i32,
    anchor_top: i32,
    anchor_bottom: i32,
    work: RECT,
    win_w: i32,
    win_h: i32,
    px: &impl Fn(i32) -> i32,
) -> (i32, i32) {
    let margin = px(6);
    let max_x = (work.right - win_w).max(work.left);
    let x = anchor_x.clamp(work.left, max_x);
    let mut y = anchor_bottom;
    if y + win_h > work.bottom {
        y = anchor_top - win_h - margin;
    }
    let max_y = (work.bottom - win_h).max(work.top);
    y = y.clamp(work.top, max_y);
    (x, y)
}

unsafe fn create_font(face_name: &str, height: i32, weight: i32) -> HFONT {
    let face = to_wide_null(face_name);
    CreateFontW(
        -height,
        0,
        0,
        0,
        weight,
        0,
        0,
        0,
        DEFAULT_CHARSET,
        OUT_DEFAULT_PRECIS,
        CLIP_DEFAULT_PRECIS,
        CLEARTYPE_QUALITY,
        (DEFAULT_PITCH.0 as u32) | (FF_SWISS.0 as u32),
        PCWSTR(face.as_ptr()),
    )
}

unsafe fn text_width(hdc: HDC, text: &str) -> i32 {
    if text.is_empty() {
        return 0;
    }
    let w: Vec<u16> = text.encode_utf16().collect();
    let mut sz = SIZE::default();
    if GetTextExtentPoint32W(hdc, &w, &mut sz).as_bool() {
        sz.cx
    } else {
        0
    }
}

#[inline]
fn blend_over(dst: u32, r: f32, g: f32, b: f32, alpha: f32) -> u32 {
    let a = alpha.clamp(0.0, 1.0);
    if a <= 0.001 {
        return dst;
    }
    if a >= 0.999 {
        let rr = r.round().clamp(0.0, 255.0) as u32;
        let gg = g.round().clamp(0.0, 255.0) as u32;
        let bb = b.round().clamp(0.0, 255.0) as u32;
        return 0xFF00_0000 | (rr << 16) | (gg << 8) | bb;
    }
    let dr = ((dst >> 16) & 0xFF) as f32;
    let dg = ((dst >> 8) & 0xFF) as f32;
    let db = (dst & 0xFF) as f32;
    let inv = 1.0 - a;
    let rr = (r * a + dr * inv).round().clamp(0.0, 255.0) as u32;
    let gg = (g * a + dg * inv).round().clamp(0.0, 255.0) as u32;
    let bb = (b * a + db * inv).round().clamp(0.0, 255.0) as u32;
    0xFF00_0000 | (rr << 16) | (gg << 8) | bb
}

fn fill_rect_px(
    pixels: &mut [u32],
    buf_w: i32,
    buf_h: i32,
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    color: Rgb,
) {
    let min_x = x0.max(0);
    let max_x = x1.min(buf_w);
    let min_y = y0.max(0);
    let max_y = y1.min(buf_h);
    if max_x <= min_x || max_y <= min_y {
        return;
    }
    let bgra = color.to_bgra();
    for y in min_y..max_y {
        let row = (y * buf_w) as usize;
        pixels[row + min_x as usize..row + max_x as usize].fill(bgra);
    }
}

/// Draws a subpixel-antialiased SDF rounded rectangle with optional crisp 1px inset stroke.
#[allow(clippy::too_many_arguments)]
fn draw_aa_rounded_rect(
    pixels: &mut [u32],
    buf_w: i32,
    buf_h: i32,
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    radius: f32,
    fill: Rgb,
    border: Option<Rgb>,
) {
    if x1 <= x0 || y1 <= y0 || buf_w <= 0 || buf_h <= 0 {
        return;
    }
    let fx0 = x0 as f32;
    let fy0 = y0 as f32;
    let fx1 = x1 as f32;
    let fy1 = y1 as f32;
    let cx = (fx0 + fx1) * 0.5;
    let cy = (fy0 + fy1) * 0.5;
    let hw = (fx1 - fx0) * 0.5;
    let hh = (fy1 - fy0) * 0.5;
    let r = radius.min(hw).min(hh).max(0.0);
    let bx = (hw - r).max(0.0);
    let by = (hh - r).max(0.0);

    let min_x = (x0 - 1).max(0);
    let max_x = (x1 + 1).min(buf_w);
    let min_y = (y0 - 1).max(0);
    let max_y = (y1 + 1).min(buf_h);

    for py in min_y..max_y {
        let dy = (py as f32 + 0.5 - cy).abs() - by;
        let row_off = (py * buf_w) as usize;
        for px in min_x..max_x {
            let dx = (px as f32 + 0.5 - cx).abs() - bx;
            let qx = dx.max(0.0);
            let qy = dy.max(0.0);
            let sd = (qx * qx + qy * qy).sqrt() + dx.max(dy).min(0.0) - r;
            let outer_cov = (0.5 - sd).clamp(0.0, 1.0);
            if outer_cov <= 0.001 {
                continue;
            }
            let idx = row_off + px as usize;
            if let Some(bc) = border {
                let inner_cov = (-0.5 - sd).clamp(0.0, 1.0);
                let border_cov = (outer_cov - inner_cov).max(0.0);
                let inv_outer = 1.0 / outer_cov;
                let sr = (fill.0 as f32 * inner_cov + bc.0 as f32 * border_cov) * inv_outer;
                let sg = (fill.1 as f32 * inner_cov + bc.1 as f32 * border_cov) * inv_outer;
                let sb = (fill.2 as f32 * inner_cov + bc.2 as f32 * border_cov) * inv_outer;
                pixels[idx] = blend_over(pixels[idx], sr, sg, sb, outer_cov);
            } else {
                pixels[idx] = blend_over(
                    pixels[idx],
                    fill.0 as f32,
                    fill.1 as f32,
                    fill.2 as f32,
                    outer_cov,
                );
            }
        }
    }
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        windows::Win32::UI::WindowsAndMessaging::WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let ptr = get_window_user_data(hwnd) as *const PanelRenderState;
            if !ptr.is_null() {
                paint_candidate_panel(hwnd, hdc, &*ptr);
            }
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_TIMER => {
            if wparam.0 == TOAST_TIMER_ID {
                let _ = KillTimer(Some(hwnd), TOAST_TIMER_ID);
                let _ = ShowWindow(hwnd, SW_HIDE);
            } else if wparam.0 == CARET_TIMER_ID {
                let ptr = get_window_user_data(hwnd) as *const PanelRenderState;
                if !ptr.is_null() {
                    let panel = &*ptr;
                    if panel.config.show_preedit_caret
                        && !panel.view.is_toast
                        && !panel.view.preedit.is_empty()
                    {
                        panel.caret_visible.set(!panel.caret_visible.get());
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    } else {
                        let _ = KillTimer(Some(hwnd), CARET_TIMER_ID);
                    }
                }
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn paint_candidate_panel(hwnd: HWND, hdc: HDC, panel: &PanelRenderState) {
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let w = (rc.right - rc.left).max(1);
    let h = (rc.bottom - rc.top).max(1);

    let dpi = GetDpiForWindow(hwnd);
    let scale = if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 };
    let pal = palette_for(panel.config.theme, panel.config.highlight_style);

    let mem_dc = CreateCompatibleDC(Some(hdc));
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h, // top-down DIB
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits_ptr: *mut core::ffi::c_void = null_mut();
    let Ok(dib) = CreateDIBSection(
        Some(hdc),
        &bmi,
        DIB_RGB_COLORS,
        &mut bits_ptr,
        None,
        0,
    ) else {
        let _ = DeleteDC(mem_dc);
        return;
    };
    if bits_ptr.is_null() {
        let _ = DeleteObject(dib.into());
        let _ = DeleteDC(mem_dc);
        return;
    }

    let old_bmp = SelectObject(mem_dc, dib.into());
    let pixels = std::slice::from_raw_parts_mut(bits_ptr.cast::<u32>(), (w * h) as usize);
    render_panel_into_surface(mem_dc, pixels, w, h, scale, pal, panel);

    let _ = BitBlt(hdc, 0, 0, w, h, Some(mem_dc), 0, 0, SRCCOPY);
    SelectObject(mem_dc, old_bmp);
    let _ = DeleteObject(dib.into());
    let _ = DeleteDC(mem_dc);
}

unsafe fn render_panel_into_surface(
    mem_dc: HDC,
    pixels: &mut [u32],
    w: i32,
    h: i32,
    scale: f32,
    pal: GeistPalette,
    panel: &PanelRenderState,
) {
    let px = |v: i32| ((v as f32) * scale).round() as i32;
    let state = &panel.view;
    let cfg = &panel.config;

    let surface_bg = if state.is_toast { pal.toast_bg } else { pal.bg };
    pixels.fill(surface_bg.to_bgra());

    if panel.draw_fallback_border {
        fill_rect_px(pixels, w, h, 0, 0, w, 1, pal.border);
        fill_rect_px(pixels, w, h, 0, h - 1, w, h, pal.border);
        fill_rect_px(pixels, w, h, 0, 0, 1, h, pal.border);
        fill_rect_px(pixels, w, h, w - 1, 0, w, h, pal.border);
    }

    SetBkMode(mem_dc, TRANSPARENT);
    let fonts = PanelFonts::create(cfg, &px);
    let old_font = SelectObject(mem_dc, fonts.preedit.into());

    if state.is_toast {
        let _ = GdiFlush();
        SelectObject(mem_dc, fonts.toast.into());
        SetTextColor(mem_dc, pal.toast_fg.to_colorref());
        let mut toast_w: Vec<u16> = state.preedit.encode_utf16().collect();
        let mut toast_rc = RECT {
            left: 0,
            top: px(1),
            right: w,
            bottom: h,
        };
        if !toast_w.is_empty() {
            DrawTextW(
                mem_dc,
                &mut toast_w,
                &mut toast_rc,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            );
        }
    } else {
        let layout = compute_panel_layout(mem_dc, state, cfg, w, &fonts, &px);

        // 1. Pre-draw vector geometry into the 32bpp DIB before ClearType text.
        // Header mode <Kbd> badge pill (right-aligned)
        let badge_x1 = w - layout.header_pad_x;
        let badge_x0 = (badge_x1 - layout.badge_w).max(layout.header_pad_x);
        let badge_y0 = layout.header_top + (layout.header_h - layout.badge_h) / 2;
        let badge_y1 = badge_y0 + layout.badge_h;
        let is_en_badge = state.mode_label == "英";
        if layout.badge_w > 0 && badge_x1 > badge_x0 {
            let (b_bg, b_border) = if is_en_badge {
                (pal.badge_en_bg, pal.badge_en_border)
            } else {
                (pal.badge_zh_bg, pal.badge_zh_border)
            };
            draw_aa_rounded_rect(
                pixels,
                w,
                h,
                badge_x0,
                badge_y0,
                badge_x1,
                badge_y1,
                4.5 * scale,
                b_bg,
                Some(b_border),
            );
        }

        // Subtle vertical preedit cursor bar right after preedit text
        let meta_left = if layout.badge_w > 0 && layout.page_w > 0 {
            badge_x0 - layout.badge_gap - layout.page_w
        } else if layout.badge_w > 0 {
            badge_x0
        } else if layout.page_w > 0 {
            w - layout.header_pad_x - layout.page_w
        } else {
            w - layout.header_pad_x
        };
        let pre_right = (meta_left - px(8)).max(layout.header_pad_x);
        if cfg.show_preedit_caret
            && panel.caret_visible.get()
            && !state.preedit.is_empty()
            && layout.header_pad_x + layout.preedit_w + px(5) <= pre_right
        {
            let pre_sz = (cfg.font_size.clamp(12, 22) - 1).max(12);
            let caret_x = layout.header_pad_x + layout.preedit_w + px(3);
            let caret_w = (scale.floor() as i32).max(1);
            let caret_h = px(pre_sz + 3).min(layout.header_h - px(2)).max(px(12));
            let caret_y0 = layout.header_top + (layout.header_h - caret_h) / 2;
            fill_rect_px(
                pixels,
                w,
                h,
                caret_x,
                caret_y0,
                caret_x + caret_w,
                caret_y0 + caret_h,
                pal.caret,
            );
        }

        // Full-bleed Geist hairline divider + selected candidate pill
        let max_cand_right = (w - layout.row_pad_x).max(layout.row_pad_x);
        if !state.candidates.is_empty() {
            let div_x0 = if panel.draw_fallback_border { 1 } else { 0 };
            let div_x1 = if panel.draw_fallback_border { w - 1 } else { w };
            fill_rect_px(pixels, w, h, div_x0, layout.div_y, div_x1, layout.div_y + 1, pal.divider);

            if let Some(sel_item) = layout.items.get(state.selected_in_page) {
                if sel_item.x0 < max_cand_right {
                    let sel_x1 = sel_item.x1.min(max_cand_right);
                    draw_aa_rounded_rect(
                        pixels,
                        w,
                        h,
                        sel_item.x0,
                        sel_item.y0,
                        sel_x1,
                        sel_item.y1,
                        6.0 * scale,
                        pal.sel_bg,
                        pal.sel_border,
                    );
                }
            }
        }

        let _ = GdiFlush();

        // 2. Draw crisp native ClearType text on top of the opaque antialiased surfaces.
        SelectObject(mem_dc, fonts.preedit.into());
        SetTextColor(mem_dc, pal.preedit.to_colorref());
        let mut pre_w: Vec<u16> = state.preedit.encode_utf16().collect();
        let mut pre_rc = RECT {
            left: layout.header_pad_x,
            top: layout.header_top,
            right: pre_right,
            bottom: layout.header_top + layout.header_h,
        };
        if !pre_w.is_empty() && pre_rc.right > pre_rc.left {
            DrawTextW(
                mem_dc,
                &mut pre_w,
                &mut pre_rc,
                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS,
            );
        }

        if layout.page_w > 0 {
            SelectObject(mem_dc, fonts.meta.into());
            SetTextColor(mem_dc, pal.muted.to_colorref());
            let page_right = if layout.badge_w > 0 {
                badge_x0 - layout.badge_gap
            } else {
                w - layout.header_pad_x
            };
            let mut page_w_utf16: Vec<u16> = state.page_text.encode_utf16().collect();
            let mut page_rc = RECT {
                left: (page_right - layout.page_w - px(2)).max(layout.header_pad_x),
                top: layout.header_top,
                right: page_right,
                bottom: layout.header_top + layout.header_h,
            };
            if page_rc.right > page_rc.left {
                DrawTextW(
                    mem_dc,
                    &mut page_w_utf16,
                    &mut page_rc,
                    DT_RIGHT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
                );
            }
        }

        if layout.badge_w > 0 && badge_x1 > badge_x0 {
            SelectObject(mem_dc, fonts.badge.into());
            let badge_fg = if is_en_badge {
                pal.badge_en_fg
            } else {
                pal.badge_zh_fg
            };
            SetTextColor(mem_dc, badge_fg.to_colorref());
            let mut badge_utf16: Vec<u16> = state.mode_label.encode_utf16().collect();
            let mut badge_rc = RECT {
                left: badge_x0,
                top: badge_y0 + px(1),
                right: badge_x1,
                bottom: badge_y1,
            };
            DrawTextW(
                mem_dc,
                &mut badge_utf16,
                &mut badge_rc,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            );
        }

        if !state.candidates.is_empty() {
            for (i, (cand, item)) in state
                .candidates
                .iter()
                .zip(layout.items.iter())
                .enumerate()
            {
                if item.x0 >= max_cand_right {
                    break;
                }
                let x1_clamped = item.x1.min(max_cand_right);
                let is_sel = i == state.selected_in_page;

                let idx_left = item.x0 + layout.item_pad_x;
                let idx_right = (idx_left + item.idx_w).min(x1_clamped);
                if idx_right > idx_left {
                    SelectObject(mem_dc, fonts.index.into());
                    SetTextColor(
                        mem_dc,
                        if is_sel { pal.sel_index } else { pal.index }.to_colorref(),
                    );
                    let mut idx_utf16: Vec<u16> = (i + 1).to_string().encode_utf16().collect();
                    let mut idx_rc = RECT {
                        left: idx_left,
                        top: item.y0 + px(1),
                        right: idx_right,
                        bottom: item.y1,
                    };
                    DrawTextW(
                        mem_dc,
                        &mut idx_utf16,
                        &mut idx_rc,
                        DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
                    );
                }

                let text_left = idx_left + item.idx_w + layout.idx_gap;
                let text_right = (x1_clamped - layout.item_pad_x + px(2)).min(x1_clamped);
                if text_right > text_left {
                    SelectObject(
                        mem_dc,
                        if is_sel { fonts.cand_sel } else { fonts.cand }.into(),
                    );
                    SetTextColor(
                        mem_dc,
                        if is_sel { pal.sel_fg } else { pal.text }.to_colorref(),
                    );
                    let mut cand_utf16: Vec<u16> = cand.encode_utf16().collect();
                    let mut cand_rc = RECT {
                        left: text_left,
                        top: item.y0 + px(1),
                        right: text_right,
                        bottom: item.y1,
                    };
                    DrawTextW(
                        mem_dc,
                        &mut cand_utf16,
                        &mut cand_rc,
                        DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS,
                    );
                }
            }
        }
    }

    SelectObject(mem_dc, old_font);
    fonts.destroy();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn horizontal_and_vertical_layout_measure_properly() {
        let mut wnd = CandidateWindow::new().unwrap();
        let px = |v: i32| v;
        let cfg_h = ImeConfig::default();
        let mut cfg_v = ImeConfig::default();
        cfg_v.layout = CandidateLayout::Vertical;

        let view_two = CandidateViewState {
            preedit: "nihao".into(),
            mode_label: "中".into(),
            page_text: "1/2".into(),
            candidates: vec!["你好".into(), "拟好".into()],
            selected_in_page: 0,
            is_toast: false,
        };
        let view_five = CandidateViewState {
            preedit: "nihao".into(),
            mode_label: "中".into(),
            page_text: "1/2".into(),
            candidates: vec![
                "你好".into(),
                "拟好".into(),
                "霓好".into(),
                "尼好".into(),
                "泥好".into(),
            ],
            selected_in_page: 0,
            is_toast: false,
        };

        let (w2, h2) = measure_window_size(wnd.hwnd, &view_two, &cfg_h, 960, &px);
        let (w5, h5) = measure_window_size(wnd.hwnd, &view_five, &cfg_h, 960, &px);
        assert_eq!(h2, h5);
        assert!(w5 > h5);
        assert!(w5 > w2);

        let (vw, vh) = measure_window_size(wnd.hwnd, &view_five, &cfg_v, 960, &px);
        assert!(vh > vw);
        assert!(vh > h5);

        wnd.set_config(cfg_h);
        wnd.update_and_show(view_five.clone(), None);
        let mut view_sel = view_five;
        view_sel.selected_in_page = 3;
        wnd.update_and_show(view_sel, None);
        wnd.show_mode_toast("英", None);
        wnd.hide();
    }

    #[test]
    fn rgb_conversions_and_font_detection_work() {
        let c = Rgb(0x12, 0x34, 0x56);
        assert_eq!(c.to_colorref().0, 0x0056_3412);
        assert_eq!(c.to_bgra(), 0xFF12_3456);
        assert!(!detect_best_cjk_font().is_empty());
    }
}

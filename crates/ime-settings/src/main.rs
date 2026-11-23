//! Native Vercel / Geist-styled GUI Settings application for Pyn (`ime-settings.exe`).
//! Hot-reloads changes live to running `ime-server.exe` and `ime_tsf.dll` without app restarts.

#![cfg_attr(not(test), windows_subsystem = "windows")]

use std::collections::HashSet;
use std::path::PathBuf;
use std::ptr::null_mut;

use ime_ipc::config::{CandidateLayout, HighlightStyle, ImeConfig, ThemeMode};
use ime_ipc::pipe::{to_wide_null, NamedPipeClient};
use ime_ipc::protocol::{IpcRequest, IpcResponse, DEFAULT_PIPE_NAME};
use windows::Win32::Foundation::{
    COLORREF, ERROR_ALREADY_EXISTS, GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, RECT, SIZE,
    WPARAM,
};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE,
    DWMWCP_ROUND, DWM_WINDOW_CORNER_PREFERENCE,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleDC, CreateDIBSection, CreateFontW, DeleteDC, DeleteObject,
    DrawTextW, EndPaint, EnumFontFamiliesExW, GdiFlush, GetDC, GetDeviceCaps,
    GetTextExtentPoint32W, InvalidateRect, ReleaseDC, SelectObject, SetBkMode, SetTextColor,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS,
    DEFAULT_CHARSET, DEFAULT_PITCH, DIB_RGB_COLORS, DRAW_TEXT_FORMAT, DT_CENTER, DT_END_ELLIPSIS,
    DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, FF_SWISS, GB2312_CHARSET, HDC,
    HFONT, LOGFONTW, LOGPIXELSX, OUT_DEFAULT_PRECIS, PAINTSTRUCT, SRCCOPY, TEXTMETRICW,
    TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::{
    GetDpiForWindow, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_DOWN, VK_ESCAPE,
    VK_NEXT, VK_PRIOR, VK_RETURN, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRectEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    FindWindowW, GetClientRect, GetMessageW, GetSystemMetrics, LoadCursorW, LoadImageW,
    PostQuitMessage, RegisterClassExW, SetCursor, SetForegroundWindow, ShowWindow,
    TranslateMessage, CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, HICON, HTCLIENT, IDC_ARROW, IDC_HAND,
    IMAGE_ICON, LR_DEFAULTCOLOR, MSG, SM_CXICON, SM_CXSCREEN, SM_CXSMICON, SM_CYICON, SM_CYSCREEN,
    SM_CYSMICON, SW_RESTORE, SW_SHOW, WM_CHAR, WM_CLOSE, WM_DESTROY, WM_ERASEBKGND, WM_KEYDOWN,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_PAINT, WM_SETCURSOR,
    WNDCLASSEXW, WS_CAPTION, WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU,
};
use windows_core::{BOOL, PCWSTR};

const CLASS_NAME: &str = "PynSettingsWnd";
const WINDOW_TITLE: &str = "Pyn 设置";
const MUTEX_NAME: &str = r"Local\PynSettingsMutex_v1";
const LOGICAL_W: i32 = 580;
const LOGICAL_H: i32 = 746;

const CURATED_CJK_FONTS: &[&str] = &[
    "MiSans",
    "OPPO Sans 4.0",
    "HarmonyOS Sans SC",
    "Noto Sans SC",
    "Source Han Sans",
    "思源黑体",
    "等线",
    "Microsoft YaHei",
    "Microsoft YaHei UI",
    "Segoe UI Variable Text",
];

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
struct SettingsPalette {
    window_bg: Rgb,
    card_bg: Rgb,
    card_border: Rgb,
    stage_bg: Rgb,
    stage_dot: Rgb,
    divider: Rgb,
    text_primary: Rgb,
    text_secondary: Rgb,
    text_muted: Rgb,
    seg_bg: Rgb,
    seg_border: Rgb,
    seg_active_bg: Rgb,
    seg_active_fg: Rgb,
    seg_hover_bg: Rgb,
    btn_bg: Rgb,
    btn_border: Rgb,
    btn_hover_bg: Rgb,
    btn_fg: Rgb,
    primary_bg: Rgb,
    primary_hover_bg: Rgb,
    primary_fg: Rgb,
    toggle_off_bg: Rgb,
    toggle_off_border: Rgb,
    toggle_off_knob: Rgb,
    toggle_on_bg: Rgb,
    toggle_on_knob: Rgb,
    status_ok: Rgb,
    status_warn: Rgb,
}

fn settings_palette(theme: ThemeMode) -> SettingsPalette {
    if resolve_is_light(theme) {
        SettingsPalette {
            window_bg: Rgb(0xFA, 0xFA, 0xFA),
            card_bg: Rgb(0xFF, 0xFF, 0xFF),
            card_border: Rgb(0xE5, 0xE5, 0xE5),
            stage_bg: Rgb(0xF4, 0xF4, 0xF5),
            stage_dot: Rgb(0xE2, 0xE2, 0xE6),
            divider: Rgb(0xF0, 0xF0, 0xF0),
            text_primary: Rgb(0x17, 0x17, 0x17),
            text_secondary: Rgb(0x52, 0x52, 0x52),
            text_muted: Rgb(0x8A, 0x8A, 0x8A),
            seg_bg: Rgb(0xF4, 0xF4, 0xF5),
            seg_border: Rgb(0xE4, 0xE4, 0xE7),
            seg_active_bg: Rgb(0xFF, 0xFF, 0xFF),
            seg_active_fg: Rgb(0x17, 0x17, 0x17),
            seg_hover_bg: Rgb(0xEA, 0xEA, 0xEC),
            btn_bg: Rgb(0xFF, 0xFF, 0xFF),
            btn_border: Rgb(0xE4, 0xE4, 0xE7),
            btn_hover_bg: Rgb(0xF4, 0xF4, 0xF5),
            btn_fg: Rgb(0x17, 0x17, 0x17),
            primary_bg: Rgb(0x17, 0x17, 0x17),
            primary_hover_bg: Rgb(0x2E, 0x2E, 0x2E),
            primary_fg: Rgb(0xFF, 0xFF, 0xFF),
            toggle_off_bg: Rgb(0xE4, 0xE4, 0xE7),
            toggle_off_border: Rgb(0xD4, 0xD4, 0xD8),
            toggle_off_knob: Rgb(0xFF, 0xFF, 0xFF),
            toggle_on_bg: Rgb(0x17, 0x17, 0x17),
            toggle_on_knob: Rgb(0xFF, 0xFF, 0xFF),
            status_ok: Rgb(0x16, 0xA3, 0x4A),
            status_warn: Rgb(0xD9, 0x77, 0x06),
        }
    } else {
        SettingsPalette {
            window_bg: Rgb(0x00, 0x00, 0x00),
            card_bg: Rgb(0x0A, 0x0A, 0x0A),
            card_border: Rgb(0x24, 0x24, 0x24),
            stage_bg: Rgb(0x11, 0x11, 0x11),
            stage_dot: Rgb(0x20, 0x20, 0x20),
            divider: Rgb(0x1A, 0x1A, 0x1A),
            text_primary: Rgb(0xFA, 0xFA, 0xFA),
            text_secondary: Rgb(0xA3, 0xA3, 0xA3),
            text_muted: Rgb(0x73, 0x73, 0x73),
            seg_bg: Rgb(0x14, 0x14, 0x14),
            seg_border: Rgb(0x26, 0x26, 0x26),
            seg_active_bg: Rgb(0x26, 0x26, 0x26),
            seg_active_fg: Rgb(0xFA, 0xFA, 0xFA),
            seg_hover_bg: Rgb(0x1C, 0x1C, 0x1C),
            btn_bg: Rgb(0x14, 0x14, 0x14),
            btn_border: Rgb(0x2E, 0x2E, 0x2E),
            btn_hover_bg: Rgb(0x1F, 0x1F, 0x1F),
            btn_fg: Rgb(0xED, 0xED, 0xED),
            primary_bg: Rgb(0xFA, 0xFA, 0xFA),
            primary_hover_bg: Rgb(0xE5, 0xE5, 0xE5),
            primary_fg: Rgb(0x0A, 0x0A, 0x0A),
            toggle_off_bg: Rgb(0x22, 0x22, 0x22),
            toggle_off_border: Rgb(0x33, 0x33, 0x33),
            toggle_off_knob: Rgb(0x8A, 0x8A, 0x8A),
            toggle_on_bg: Rgb(0xFA, 0xFA, 0xFA),
            toggle_on_knob: Rgb(0x0A, 0x0A, 0x0A),
            status_ok: Rgb(0x22, 0xC5, 0x5E),
            status_warn: Rgb(0xF5, 0x9E, 0x0B),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct CandPalette {
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
    badge_bg: Rgb,
    badge_border: Rgb,
    badge_fg: Rgb,
}

fn candidate_preview_palette(theme: ThemeMode, highlight: HighlightStyle) -> CandPalette {
    let is_light = resolve_is_light(theme);
    match (is_light, highlight) {
        (true, HighlightStyle::Subtle) => CandPalette {
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
            badge_bg: Rgb(0xF5, 0xF5, 0xF5),
            badge_border: Rgb(0xE5, 0xE5, 0xE5),
            badge_fg: Rgb(0x52, 0x52, 0x52),
        },
        (true, HighlightStyle::Inverted) => CandPalette {
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
            badge_bg: Rgb(0xF5, 0xF5, 0xF5),
            badge_border: Rgb(0xE5, 0xE5, 0xE5),
            badge_fg: Rgb(0x52, 0x52, 0x52),
        },
        (false, HighlightStyle::Subtle) => CandPalette {
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
            badge_bg: Rgb(0x17, 0x17, 0x17),
            badge_border: Rgb(0x2E, 0x2E, 0x2E),
            badge_fg: Rgb(0xA3, 0xA3, 0xA3),
        },
        (false, HighlightStyle::Inverted) => CandPalette {
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
            badge_bg: Rgb(0x17, 0x17, 0x17),
            badge_border: Rgb(0x2E, 0x2E, 0x2E),
            badge_fg: Rgb(0xA3, 0xA3, 0xA3),
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

fn resolve_is_light(theme: ThemeMode) -> bool {
    match theme {
        ThemeMode::Auto => read_apps_use_light_theme(),
        ThemeMode::Dark => false,
        ThemeMode::Light => true,
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

struct FontCatalog {
    auto_detected: String,
    curated: Vec<String>,
    cjk_others: Vec<String>,
    all_others: Vec<String>,
}

const EXCLUDED_BITMAP_FONTS: &[&str] = &["Fixedsys", "System", "Terminal", "8514oem"];

unsafe fn enumerate_fonts_for_charset(charset: u8) -> HashSet<String> {
    let mut set = HashSet::new();
    let hdc = GetDC(None);
    if !hdc.is_invalid() {
        let mut lf = LOGFONTW::default();
        lf.lfCharSet = windows::Win32::Graphics::Gdi::FONT_CHARSET(charset);
        let _ = EnumFontFamiliesExW(
            hdc,
            &lf,
            Some(collect_font_proc),
            LPARAM(&mut set as *mut HashSet<String> as isize),
            0,
        );
        ReleaseDC(None, hdc);
    }
    set
}

fn build_font_catalog() -> FontCatalog {
    let gb_set = unsafe { enumerate_fonts_for_charset(GB2312_CHARSET.0) };
    let all_set = unsafe { enumerate_fonts_for_charset(DEFAULT_CHARSET.0) };
    let is_excluded = |n: &str| EXCLUDED_BITMAP_FONTS.iter().any(|&ex| ex.eq_ignore_ascii_case(n));

    let auto_detected = CURATED_CJK_FONTS
        .iter()
        .copied()
        .find(|name| all_set.contains(*name))
        .unwrap_or("Microsoft YaHei UI")
        .to_owned();

    let mut curated = Vec::new();
    let mut seen_curated = HashSet::new();
    for &name in CURATED_CJK_FONTS {
        if all_set.contains(name) && !is_excluded(name) {
            curated.push(name.to_owned());
            seen_curated.insert(name);
        }
    }

    let mut cjk_others: Vec<String> = gb_set
        .iter()
        .filter(|n| !seen_curated.contains(n.as_str()) && !is_excluded(n.as_str()))
        .cloned()
        .collect();
    cjk_others.sort_by_key(|a| a.to_lowercase());

    let mut all_others: Vec<String> = all_set
        .into_iter()
        .filter(|n| {
            !seen_curated.contains(n.as_str())
                && !gb_set.contains(n.as_str())
                && !is_excluded(n.as_str())
        })
        .collect();
    all_others.sort_by_key(|a| a.to_ascii_lowercase());

    FontCatalog {
        auto_detected,
        curated,
        cjk_others,
        all_others,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ControlAction {
    SetTheme(ThemeMode),
    SetLayout(CandidateLayout),
    SetHighlight(HighlightStyle),
    PickFont,
    SetFontCjkOnly(bool),
    ClearFontSearch,
    SelectFontItem(usize),
    FontSizeDec,
    FontSizeInc,
    SetSelectedWeight(u16),
    PageSizeDec,
    PageSizeInc,
    ToggleDefaultPunct,
    ToggleShift,
    ToggleInitialUpper,
    ToggleCaret,
    ToggleToast,
    ToggleLearning,
    ResetDefaults,
    CloseWindow,
}

#[derive(Debug, Clone, Copy)]
struct HitRegion {
    rect: RECT,
    action: ControlAction,
}

struct AppState {
    hwnd: HWND,
    config: ImeConfig,
    config_path: PathBuf,
    fonts: FontCatalog,
    server_connected: bool,
    status_note: String,
    hit_regions: Vec<HitRegion>,
    hovered: Option<ControlAction>,
    tracking_mouse: bool,
    last_dark_titlebar: Option<bool>,
    font_picker_open: bool,
    font_cjk_only: bool,
    font_search: String,
    font_scroll: usize,
    font_picker_rect: RECT,
    scrollbar_track_rect: RECT,
    font_visible_rows: usize,
    font_total_items: usize,
    dragging_scrollbar: bool,
}

impl AppState {
    fn new(config: ImeConfig, server_connected: bool) -> Self {
        let status_note = if server_connected {
            "已连接引擎 · 修改实时生效".to_owned()
        } else {
            "引擎未运行 · 修改已保存到本地配置".to_owned()
        };
        Self {
            hwnd: HWND::default(),
            config,
            config_path: ImeConfig::default_config_path(),
            fonts: build_font_catalog(),
            server_connected,
            status_note,
            hit_regions: Vec::new(),
            hovered: None,
            tracking_mouse: false,
            last_dark_titlebar: None,
            font_picker_open: false,
            font_cjk_only: true,
            font_search: String::new(),
            font_scroll: 0,
            font_picker_rect: RECT::default(),
            scrollbar_track_rect: RECT::default(),
            font_visible_rows: 10,
            font_total_items: 0,
            dragging_scrollbar: false,
        }
    }

    fn effective_font_name(&self) -> &str {
        let t = self.config.font_family.trim();
        if t.is_empty() {
            &self.fonts.auto_detected
        } else {
            t
        }
    }

    fn filtered_font_items(&self) -> Vec<(String, String, bool)> {
        let q = self.font_search.trim().to_lowercase();
        let cur = self.config.font_family.trim();
        let matches_q = |text: &str, val: &str| -> bool {
            q.is_empty()
                || text.to_lowercase().contains(&q)
                || val.to_lowercase().contains(&q)
        };
        let mut out = Vec::new();
        let auto_label = format!("自动优选 ({})", self.fonts.auto_detected);
        if matches_q(&auto_label, "") {
            out.push((auto_label, String::new(), cur.is_empty()));
        }
        for name in &self.fonts.curated {
            if matches_q(name, name) {
                out.push((name.clone(), name.clone(), cur == name));
            }
        }
        for name in &self.fonts.cjk_others {
            if matches_q(name, name) {
                out.push((name.clone(), name.clone(), cur == name));
            }
        }
        if !self.font_cjk_only {
            for name in &self.fonts.all_others {
                if matches_q(name, name) {
                    out.push((name.clone(), name.clone(), cur == name));
                }
            }
        }
        out
    }

    fn open_font_picker(&mut self) {
        self.font_picker_open = true;
        self.font_search.clear();
        let cur = self.config.font_family.trim();
        if !cur.is_empty() && self.fonts.all_others.iter().any(|n| n == cur) {
            self.font_cjk_only = false;
        }
        let items = self.filtered_font_items();
        self.font_total_items = items.len();
        let vis = self.font_visible_rows.max(1);
        let max_scroll = items.len().saturating_sub(vis);
        self.font_scroll = items
            .iter()
            .position(|it| it.2)
            .map(|idx| {
                if idx >= vis {
                    idx.saturating_sub(2).min(max_scroll)
                } else {
                    0
                }
            })
            .unwrap_or(0);
    }

    fn persist_and_push(&mut self) {
        self.config = self.config.clone().normalized();
        let save_res = self.config.save_atomic(&self.config_path);
        match NamedPipeClient::connect(DEFAULT_PIPE_NAME, 120).and_then(|mut c| {
            c.request(&IpcRequest::SetConfig {
                config: self.config.clone(),
            })
        }) {
            Ok(IpcResponse::Config { .. }) => {
                self.server_connected = true;
                self.status_note = "已保存并实时同步到输入引擎".to_owned();
            }
            Ok(IpcResponse::Error { message }) => {
                self.server_connected = false;
                self.status_note = format!("保存失败：{message}");
            }
            Ok(_) | Err(_) => {
                self.server_connected = false;
                self.status_note = match save_res {
                    Ok(()) => "引擎未运行 · 已写入本地配置，下次打字生效".to_owned(),
                    Err(err) => format!("写入本地配置失败：{err}"),
                };
            }
        }
        sync_titlebar_theme(self.hwnd, self.config.theme, &mut self.last_dark_titlebar);
        if !self.hwnd.0.is_null() {
            unsafe {
                let _ = InvalidateRect(Some(self.hwnd), None, false);
            }
        }
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

fn sync_titlebar_theme(hwnd: HWND, theme: ThemeMode, last: &mut Option<bool>) {
    if hwnd.0.is_null() {
        return;
    }
    let dark = !resolve_is_light(theme);
    if *last == Some(dark) {
        return;
    }
    let dark_val: BOOL = BOOL::from(dark);
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            (&dark_val as *const BOOL).cast(),
            std::mem::size_of_val(&dark_val) as u32,
        );
    }
    *last = Some(dark);
}

fn load_initial_config() -> (ImeConfig, bool) {
    if let Ok(mut client) = NamedPipeClient::connect(DEFAULT_PIPE_NAME, 120) {
        if let Ok(IpcResponse::Config { config, .. }) = client.request(&IpcRequest::GetConfig) {
            return (config.normalized(), true);
        }
    }
    (ImeConfig::load_from(&ImeConfig::default_config_path()), false)
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

unsafe fn draw_text(hdc: HDC, text: &str, rc: RECT, color: Rgb, flags: DRAW_TEXT_FORMAT) {
    if rc.right <= rc.left || rc.bottom <= rc.top || text.is_empty() {
        return;
    }
    SetTextColor(hdc, color.to_colorref());
    let mut utf16: Vec<u16> = text.encode_utf16().collect();
    let mut r = rc;
    DrawTextW(hdc, &mut utf16, &mut r, flags);
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

fn draw_aa_circle(
    pixels: &mut [u32],
    buf_w: i32,
    buf_h: i32,
    cx: f32,
    cy: f32,
    radius: f32,
    fill: Rgb,
) {
    let min_x = ((cx - radius - 1.5).floor() as i32).max(0);
    let max_x = ((cx + radius + 1.5).ceil() as i32).min(buf_w);
    let min_y = ((cy - radius - 1.5).floor() as i32).max(0);
    let max_y = ((cy + radius + 1.5).ceil() as i32).min(buf_h);
    for py in min_y..max_y {
        let dy = py as f32 + 0.5 - cy;
        let row_off = (py * buf_w) as usize;
        for px in min_x..max_x {
            let dx = px as f32 + 0.5 - cx;
            let dist = (dx * dx + dy * dy).sqrt() - radius;
            let cov = (0.5 - dist).clamp(0.0, 1.0);
            if cov > 0.001 {
                let idx = row_off + px as usize;
                pixels[idx] = blend_over(pixels[idx], fill.0 as f32, fill.1 as f32, fill.2 as f32, cov);
            }
        }
    }
}

struct UiFonts {
    title: HFONT,
    section: HFONT,
    body: HFONT,
    body_med: HFONT,
    small: HFONT,
}

impl UiFonts {
    unsafe fn create(face: &str, px: &impl Fn(i32) -> i32) -> Self {
        Self {
            title: create_font(face, px(18), 500),
            section: create_font(face, px(12), 500),
            body: create_font(face, px(13), 400),
            body_med: create_font(face, px(13), 400),
            small: create_font(face, px(12), 400),
        }
    }

    unsafe fn destroy(self) {
        let _ = DeleteObject(self.title.into());
        let _ = DeleteObject(self.section.into());
        let _ = DeleteObject(self.body.into());
        let _ = DeleteObject(self.body_med.into());
        let _ = DeleteObject(self.small.into());
    }
}

struct PendingLabel {
    text: String,
    rc: RECT,
    color: Rgb,
    font: HFONT,
    flags: DRAW_TEXT_FORMAT,
}

unsafe fn render_settings_surface(
    mem_dc: HDC,
    pixels: &mut [u32],
    w: i32,
    h: i32,
    scale: f32,
    state: &mut AppState,
) {
    let px = |v: i32| ((v as f32) * scale).round() as i32;
    let pal = settings_palette(state.config.theme);
    let ui_face = state.fonts.auto_detected.as_str();

    pixels.fill(pal.window_bg.to_bgra());
    state.hit_regions.clear();

    let fonts = UiFonts::create(ui_face, &px);
    let old_font = SelectObject(mem_dc, fonts.body.into());
    SetBkMode(mem_dc, TRANSPARENT);
    let mut labels: Vec<PendingLabel> = Vec::with_capacity(64);

    let pad_x = px(20);
    let card_x0 = pad_x;
    let card_x1 = w - pad_x;
    let r_card = 8.0 * scale;
    let r_ctrl = 6.0 * scale;
    let r_seg_inner = 4.5 * scale;

    // Header title + subtitle (y = 14..50)
    labels.push(PendingLabel {
        text: "Pyn".to_owned(),
        rc: RECT {
            left: pad_x,
            top: px(14),
            right: card_x1,
            bottom: px(35),
        },
        color: pal.text_primary,
        font: fonts.title,
        flags: DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
    });
    labels.push(PendingLabel {
        text: "候选窗外观、字体与输入行为配置".to_owned(),
        rc: RECT {
            left: pad_x,
            top: px(35),
            right: card_x1,
            bottom: px(51),
        },
        color: pal.text_muted,
        font: fonts.small,
        flags: DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
    });

    // ---------------------------------------------------------
    // Card 1: 实时预览 (y = 58..170, fixed height 112)
    // ---------------------------------------------------------
    let mut y = px(58);
    let preview_h = px(112);
    draw_aa_rounded_rect(
        pixels,
        w,
        h,
        card_x0,
        y,
        card_x1,
        y + preview_h,
        r_card,
        pal.stage_bg,
        Some(pal.card_border),
    );
    let step = px(12);
    let mut dy = y + px(8);
    while dy < y + preview_h - px(6) {
        let mut dx = card_x0 + px(10);
        while dx < card_x1 - px(8) {
            fill_rect_px(pixels, w, h, dx, dy, dx + 1, dy + 1, pal.stage_dot);
            dx += step;
        }
        dy += step;
    }
    labels.push(PendingLabel {
        text: "实时预览".to_owned(),
        rc: RECT {
            left: card_x0 + px(12),
            top: y + px(7),
            right: card_x0 + px(90),
            bottom: y + px(23),
        },
        color: pal.text_muted,
        font: fonts.small,
        flags: DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
    });

    draw_candidate_preview_in_stage(
        mem_dc,
        pixels,
        w,
        h,
        scale,
        RECT {
            left: card_x0,
            top: y,
            right: card_x1,
            bottom: y + preview_h,
        },
        &state.config,
        state.effective_font_name(),
        &px,
    );

    // ---------------------------------------------------------
    // Card 2: 外观与字体 (title 182..200, card 204..456)
    // ---------------------------------------------------------
    y = px(182);
    labels.push(PendingLabel {
        text: "外观与字体".to_owned(),
        rc: RECT {
            left: card_x0 + px(2),
            top: y,
            right: card_x1,
            bottom: y + px(18),
        },
        color: pal.text_muted,
        font: fonts.section,
        flags: DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
    });
    y = px(204);

    let row_h = px(36);
    let titles = [
        "界面主题",
        "候选布局",
        "首选高亮样式",
        "候选窗字体",
        "候选字号",
        "首选词字重",
        "每页候选数",
    ];
    let card2_h = row_h * (titles.len() as i32);
    draw_aa_rounded_rect(
        pixels,
        w,
        h,
        card_x0,
        y,
        card_x1,
        y + card2_h,
        r_card,
        pal.card_bg,
        Some(pal.card_border),
    );

    let row_pad_x = px(14);
    for (idx, &title) in titles.iter().enumerate() {
        let ry = y + (idx as i32) * row_h;
        if idx > 0 {
            fill_rect_px(pixels, w, h, card_x0 + 1, ry, card_x1 - 1, ry + 1, pal.divider);
        }
        labels.push(PendingLabel {
            text: title.to_owned(),
            rc: RECT {
                left: card_x0 + row_pad_x,
                top: ry,
                right: card_x0 + px(210),
                bottom: ry + row_h,
            },
            color: pal.text_primary,
            font: fonts.body,
            flags: DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        });
    }

    // Row 0: 界面主题
    draw_segmented_control(
        pixels,
        w,
        h,
        r_ctrl,
        r_seg_inner,
        &pal,
        fonts.small,
        RECT {
            left: card_x1 - row_pad_x - px(186),
            top: y + px(5),
            right: card_x1 - row_pad_x,
            bottom: y + row_h - px(5),
        },
        &[
            ("自动", state.config.theme == ThemeMode::Auto, ControlAction::SetTheme(ThemeMode::Auto)),
            ("深色", state.config.theme == ThemeMode::Dark, ControlAction::SetTheme(ThemeMode::Dark)),
            ("浅色", state.config.theme == ThemeMode::Light, ControlAction::SetTheme(ThemeMode::Light)),
        ],
        state.hovered,
        &mut state.hit_regions,
        &mut labels,
    );

    // Row 1: 候选布局
    {
        let ry = y + row_h;
        draw_segmented_control(
            pixels,
            w,
            h,
            r_ctrl,
            r_seg_inner,
            &pal,
            fonts.small,
            RECT {
                left: card_x1 - row_pad_x - px(132),
                top: ry + px(5),
                right: card_x1 - row_pad_x,
                bottom: ry + row_h - px(5),
            },
            &[
                ("横排", state.config.layout == CandidateLayout::Horizontal, ControlAction::SetLayout(CandidateLayout::Horizontal)),
                ("竖排", state.config.layout == CandidateLayout::Vertical, ControlAction::SetLayout(CandidateLayout::Vertical)),
            ],
            state.hovered,
            &mut state.hit_regions,
            &mut labels,
        );
    }

    // Row 2: 首选高亮样式
    {
        let ry = y + 2 * row_h;
        draw_segmented_control(
            pixels,
            w,
            h,
            r_ctrl,
            r_seg_inner,
            &pal,
            fonts.small,
            RECT {
                left: card_x1 - row_pad_x - px(164),
                top: ry + px(5),
                right: card_x1 - row_pad_x,
                bottom: ry + row_h - px(5),
            },
            &[
                ("极简边框", state.config.highlight_style == HighlightStyle::Subtle, ControlAction::SetHighlight(HighlightStyle::Subtle)),
                ("黑白反色", state.config.highlight_style == HighlightStyle::Inverted, ControlAction::SetHighlight(HighlightStyle::Inverted)),
            ],
            state.hovered,
            &mut state.hit_regions,
            &mut labels,
        );
    }

    // Row 3: 候选窗字体
    let font_btn_rc;
    {
        let ry = y + 3 * row_h;
        let btn_rc = RECT {
            left: card_x1 - row_pad_x - px(236),
            top: ry + px(5),
            right: card_x1 - row_pad_x,
            bottom: ry + row_h - px(5),
        };
        font_btn_rc = btn_rc;
        let hov = state.hovered == Some(ControlAction::PickFont);
        draw_aa_rounded_rect(
            pixels,
            w,
            h,
            btn_rc.left,
            btn_rc.top,
            btn_rc.right,
            btn_rc.bottom,
            r_ctrl,
            if hov { pal.btn_hover_bg } else { pal.btn_bg },
            Some(pal.btn_border),
        );
        state.hit_regions.push(HitRegion {
            rect: btn_rc,
            action: ControlAction::PickFont,
        });
        let font_label = if state.config.font_family.is_empty() {
            format!("自动优选 ({})", state.fonts.auto_detected)
        } else {
            state.config.font_family.clone()
        };
        labels.push(PendingLabel {
            text: font_label,
            rc: RECT {
                left: btn_rc.left + px(10),
                top: btn_rc.top,
                right: btn_rc.right - px(22),
                bottom: btn_rc.bottom,
            },
            color: pal.btn_fg,
            font: fonts.small,
            flags: DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS,
        });
        labels.push(PendingLabel {
            text: "▾".to_owned(),
            rc: RECT {
                left: btn_rc.right - px(20),
                top: btn_rc.top,
                right: btn_rc.right - px(8),
                bottom: btn_rc.bottom,
            },
            color: pal.text_muted,
            font: fonts.small,
            flags: DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        });
    }

    // Row 4: 候选字号
    {
        let ry = y + 4 * row_h;
        draw_stepper(
            pixels,
            w,
            h,
            r_ctrl,
            &pal,
            fonts.body_med,
            RECT {
                left: card_x1 - row_pad_x - px(128),
                top: ry + px(5),
                right: card_x1 - row_pad_x,
                bottom: ry + row_h - px(5),
            },
            &format!("{} px", state.config.font_size),
            ControlAction::FontSizeDec,
            ControlAction::FontSizeInc,
            state.hovered,
            &mut state.hit_regions,
            &mut labels,
        );
    }

    // Row 5: 首选词字重
    {
        let ry = y + 5 * row_h;
        draw_segmented_control(
            pixels,
            w,
            h,
            r_ctrl,
            r_seg_inner,
            &pal,
            fonts.small,
            RECT {
                left: card_x1 - row_pad_x - px(228),
                top: ry + px(5),
                right: card_x1 - row_pad_x,
                bottom: ry + row_h - px(5),
            },
            &[
                ("常规 400", state.config.selected_weight == 400, ControlAction::SetSelectedWeight(400)),
                ("中等 500", state.config.selected_weight == 500, ControlAction::SetSelectedWeight(500)),
                ("半粗 600", state.config.selected_weight == 600, ControlAction::SetSelectedWeight(600)),
            ],
            state.hovered,
            &mut state.hit_regions,
            &mut labels,
        );
    }

    // Row 6: 每页候选数
    {
        let ry = y + 6 * row_h;
        draw_stepper(
            pixels,
            w,
            h,
            r_ctrl,
            &pal,
            fonts.body_med,
            RECT {
                left: card_x1 - row_pad_x - px(128),
                top: ry + px(5),
                right: card_x1 - row_pad_x,
                bottom: ry + row_h - px(5),
            },
            &format!("{} 项", state.config.page_size),
            ControlAction::PageSizeDec,
            ControlAction::PageSizeInc,
            state.hovered,
            &mut state.hit_regions,
            &mut labels,
        );
    }

    // ---------------------------------------------------------
    // Card 3: 输入与行为 (title 468..486, card 490..682)
    // ---------------------------------------------------------
    y = px(468);
    labels.push(PendingLabel {
        text: "输入与行为".to_owned(),
        rc: RECT {
            left: card_x0 + px(2),
            top: y,
            right: card_x1,
            bottom: y + px(18),
        },
        color: pal.text_muted,
        font: fonts.section,
        flags: DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
    });
    y = px(490);

    let toggle_rows = [
        ("默认中文标点（Ctrl + . 切换）", state.config.default_chinese_punct, ControlAction::ToggleDefaultPunct),
        ("单击 Shift 切换中英文", state.config.shift_toggle, ControlAction::ToggleShift),
        ("首字母大写进入英文组合", state.config.initial_upper_english, ControlAction::ToggleInitialUpper),
        ("显示拼音输入光标", state.config.show_preedit_caret, ControlAction::ToggleCaret),
        ("切换模式时显示状态浮标", state.config.show_mode_toast, ControlAction::ToggleToast),
        ("启用本地词频自学习", state.config.learning_enabled, ControlAction::ToggleLearning),
    ];
    let t_row_h = px(32);
    let card3_h = t_row_h * (toggle_rows.len() as i32);
    draw_aa_rounded_rect(
        pixels,
        w,
        h,
        card_x0,
        y,
        card_x1,
        y + card3_h,
        r_card,
        pal.card_bg,
        Some(pal.card_border),
    );

    let sw_w = px(36);
    let sw_h = px(20);
    for (idx, &(title, on, action)) in toggle_rows.iter().enumerate() {
        let ry = y + (idx as i32) * t_row_h;
        if idx > 0 {
            fill_rect_px(pixels, w, h, card_x0 + 1, ry, card_x1 - 1, ry + 1, pal.divider);
        }
        labels.push(PendingLabel {
            text: title.to_owned(),
            rc: RECT {
                left: card_x0 + row_pad_x,
                top: ry,
                right: card_x1 - row_pad_x - sw_w - px(12),
                bottom: ry + t_row_h,
            },
            color: pal.text_primary,
            font: fonts.body,
            flags: DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS,
        });

        let sx1 = card_x1 - row_pad_x;
        let sx0 = sx1 - sw_w;
        let sy0 = ry + (t_row_h - sw_h) / 2;
        let sy1 = sy0 + sw_h;
        let switch_r = (sw_h as f32) * 0.5;
        draw_aa_rounded_rect(
            pixels,
            w,
            h,
            sx0,
            sy0,
            sx1,
            sy1,
            switch_r,
            if on { pal.toggle_on_bg } else { pal.toggle_off_bg },
            if on { None } else { Some(pal.toggle_off_border) },
        );
        let knob_r = (sw_h as f32) * 0.36;
        let knob_cx = if on {
            sx1 as f32 - (sw_h as f32) * 0.5
        } else {
            sx0 as f32 + (sw_h as f32) * 0.5
        };
        let knob_cy = (sy0 + sy1) as f32 * 0.5;
        draw_aa_circle(
            pixels,
            w,
            h,
            knob_cx,
            knob_cy,
            knob_r,
            if on { pal.toggle_on_knob } else { pal.toggle_off_knob },
        );
        state.hit_regions.push(HitRegion {
            rect: RECT {
                left: card_x0 + px(12),
                top: ry,
                right: card_x1 - px(8),
                bottom: ry + t_row_h,
            },
            action,
        });
    }

    // ---------------------------------------------------------
    // Footer Status + Buttons (y = 696..728)
    // ---------------------------------------------------------
    let footer_y = y + card3_h + px(14);
    let btn_h = px(32);
    let dot_cy = (footer_y + btn_h / 2) as f32;
    draw_aa_circle(
        pixels,
        w,
        h,
        (card_x0 + px(6)) as f32,
        dot_cy,
        3.5 * scale,
        if state.server_connected { pal.status_ok } else { pal.status_warn },
    );

    let done_w = px(72);
    let reset_w = px(84);
    let btn_gap = px(8);
    let done_rc = RECT {
        left: card_x1 - done_w,
        top: footer_y,
        right: card_x1,
        bottom: footer_y + btn_h,
    };
    let reset_rc = RECT {
        left: done_rc.left - btn_gap - reset_w,
        top: footer_y,
        right: done_rc.left - btn_gap,
        bottom: footer_y + btn_h,
    };

    labels.push(PendingLabel {
        text: state.status_note.clone(),
        rc: RECT {
            left: card_x0 + px(16),
            top: footer_y,
            right: reset_rc.left - px(10),
            bottom: footer_y + btn_h,
        },
        color: pal.text_secondary,
        font: fonts.small,
        flags: DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS,
    });

    let hov_reset = state.hovered == Some(ControlAction::ResetDefaults);
    draw_aa_rounded_rect(
        pixels,
        w,
        h,
        reset_rc.left,
        reset_rc.top,
        reset_rc.right,
        reset_rc.bottom,
        r_ctrl,
        if hov_reset { pal.btn_hover_bg } else { pal.btn_bg },
        Some(pal.btn_border),
    );
    state.hit_regions.push(HitRegion {
        rect: reset_rc,
        action: ControlAction::ResetDefaults,
    });
    labels.push(PendingLabel {
        text: "恢复默认".to_owned(),
        rc: reset_rc,
        color: pal.btn_fg,
        font: fonts.small,
        flags: DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
    });

    let hov_done = state.hovered == Some(ControlAction::CloseWindow);
    draw_aa_rounded_rect(
        pixels,
        w,
        h,
        done_rc.left,
        done_rc.top,
        done_rc.right,
        done_rc.bottom,
        r_ctrl,
        if hov_done { pal.primary_hover_bg } else { pal.primary_bg },
        None,
    );
    state.hit_regions.push(HitRegion {
        rect: done_rc,
        action: ControlAction::CloseWindow,
    });
    labels.push(PendingLabel {
        text: "完成".to_owned(),
        rc: done_rc,
        color: pal.primary_fg,
        font: fonts.small,
        flags: DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
    });

    let _ = GdiFlush();

    for lbl in labels {
        SelectObject(mem_dc, lbl.font.into());
        draw_text(mem_dc, &lbl.text, lbl.rc, lbl.color, lbl.flags);
    }

    if state.font_picker_open {
        let _ = GdiFlush();
        draw_font_picker_popover(
            mem_dc,
            pixels,
            w,
            h,
            scale,
            &pal,
            &fonts,
            font_btn_rc,
            state,
            &px,
        );
    }

    SelectObject(mem_dc, old_font);
    fonts.destroy();
}

#[allow(clippy::too_many_arguments)]
fn draw_segmented_control(
    pixels: &mut [u32],
    buf_w: i32,
    buf_h: i32,
    outer_r: f32,
    inner_r: f32,
    pal: &SettingsPalette,
    font: HFONT,
    rect: RECT,
    opts: &[(&str, bool, ControlAction)],
    hovered: Option<ControlAction>,
    hits: &mut Vec<HitRegion>,
    labels: &mut Vec<PendingLabel>,
) {
    draw_aa_rounded_rect(
        pixels,
        buf_w,
        buf_h,
        rect.left,
        rect.top,
        rect.right,
        rect.bottom,
        outer_r,
        pal.seg_bg,
        Some(pal.seg_border),
    );
    let n = opts.len().max(1) as i32;
    let pad = 3;
    let inner_w = (rect.right - rect.left - pad * 2).max(n);
    let seg_w = inner_w / n;
    for (i, &(label, active, action)) in opts.iter().enumerate() {
        let sx0 = rect.left + pad + (i as i32) * seg_w;
        let sx1 = if i as i32 == n - 1 {
            rect.right - pad
        } else {
            sx0 + seg_w
        };
        let sy0 = rect.top + pad;
        let sy1 = rect.bottom - pad;
        if active {
            draw_aa_rounded_rect(
                pixels,
                buf_w,
                buf_h,
                sx0,
                sy0,
                sx1,
                sy1,
                inner_r,
                pal.seg_active_bg,
                Some(pal.btn_border),
            );
        } else if hovered == Some(action) {
            draw_aa_rounded_rect(
                pixels,
                buf_w,
                buf_h,
                sx0,
                sy0,
                sx1,
                sy1,
                inner_r,
                pal.seg_hover_bg,
                None,
            );
        }
        hits.push(HitRegion {
            rect: RECT {
                left: sx0,
                top: rect.top,
                right: sx1,
                bottom: rect.bottom,
            },
            action,
        });
        labels.push(PendingLabel {
            text: label.to_owned(),
            rc: RECT {
                left: sx0,
                top: sy0,
                right: sx1,
                bottom: sy1,
            },
            color: if active { pal.seg_active_fg } else { pal.text_secondary },
            font,
            flags: DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_stepper(
    pixels: &mut [u32],
    buf_w: i32,
    buf_h: i32,
    radius: f32,
    pal: &SettingsPalette,
    font: HFONT,
    rect: RECT,
    value_label: &str,
    dec_action: ControlAction,
    inc_action: ControlAction,
    hovered: Option<ControlAction>,
    hits: &mut Vec<HitRegion>,
    labels: &mut Vec<PendingLabel>,
) {
    let btn_w = (rect.bottom - rect.top).max(24);
    let left_rc = RECT {
        left: rect.left,
        top: rect.top,
        right: rect.left + btn_w,
        bottom: rect.bottom,
    };
    let right_rc = RECT {
        left: rect.right - btn_w,
        top: rect.top,
        right: rect.right,
        bottom: rect.bottom,
    };
    let mid_rc = RECT {
        left: left_rc.right,
        top: rect.top,
        right: right_rc.left,
        bottom: rect.bottom,
    };

    for (rc, action, sym) in [(left_rc, dec_action, "−"), (right_rc, inc_action, "+")] {
        let hov = hovered == Some(action);
        draw_aa_rounded_rect(
            pixels,
            buf_w,
            buf_h,
            rc.left,
            rc.top,
            rc.right,
            rc.bottom,
            radius,
            if hov { pal.btn_hover_bg } else { pal.btn_bg },
            Some(pal.btn_border),
        );
        hits.push(HitRegion { rect: rc, action });
        labels.push(PendingLabel {
            text: sym.to_owned(),
            rc,
            color: pal.btn_fg,
            font,
            flags: DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        });
    }
    labels.push(PendingLabel {
        text: value_label.to_owned(),
        rc: mid_rc,
        color: pal.text_primary,
        font,
        flags: DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
    });
}

#[allow(clippy::too_many_arguments)]
unsafe fn draw_candidate_preview_in_stage(
    mem_dc: HDC,
    pixels: &mut [u32],
    buf_w: i32,
    buf_h: i32,
    scale: f32,
    stage: RECT,
    cfg: &ImeConfig,
    font_face: &str,
    px: &impl Fn(i32) -> i32,
) {
    let cpal = candidate_preview_palette(cfg.theme, cfg.highlight_style);
    let stage_h = stage.bottom - stage.top;
    let max_stage_w = (stage.right - stage.left - px(28)).max(px(180));
    let max_panel_h = (stage_h - px(14)).max(px(64));

    let base_sz = if cfg.layout == CandidateLayout::Vertical {
        cfg.font_size.clamp(12, 15)
    } else {
        cfg.font_size.clamp(12, 22)
    };
    let pre_sz = (base_sz - 1).max(12);
    let small_sz = (base_sz - 4).clamp(10, 14);
    let sel_weight = cfg.selected_weight.clamp(400, 700) as i32;

    let f_pre = create_font(font_face, px(pre_sz), 400);
    let f_cand = create_font(font_face, px(base_sz), 400);
    let f_cand_sel = create_font(font_face, px(base_sz), sel_weight);
    let f_idx = create_font("Segoe UI Variable Text", px(small_sz), 500);
    let f_badge = create_font(font_face, px(small_sz), 400);

    let sample_pool = [
        "谢谢", "写写", "斜斜", "歇歇", "泄泄", "械械", "邂邂", "屑屑", "蟹蟹",
    ];
    let page_count = cfg.page_size.clamp(3, sample_pool.len());

    let (header_pad_x, header_top, header_h, header_bot, row_pad_top, row_pad_bot, row_h, v_gap) =
        if cfg.layout == CandidateLayout::Vertical {
            (
                px(10),
                px(4),
                px((base_sz + 4).clamp(18, 20)),
                px(3),
                px(3),
                px(4),
                px((base_sz + 5).clamp(19, 21)),
                px(1),
            )
        } else {
            (
                px(11),
                px(5),
                px((base_sz + 6).max(21)),
                px(5),
                px(5),
                px(5),
                px((base_sz + 12).max(26)),
                px(2),
            )
        };
    let div_off = header_top + header_h + header_bot;
    let row_pad_x = px(6);
    let item_pad_x = px(8);
    let idx_gap = px(5);
    let item_gap = px(3);
    let badge_h = px((base_sz + 3).clamp(16, 22));
    let badge_gap = px(7);

    SelectObject(mem_dc, f_pre.into());
    let preedit_str = "xiexie";
    let preedit_w = text_width(mem_dc, preedit_str);

    SelectObject(mem_dc, f_idx.into());
    let page_str = "1/3";
    let page_w = text_width(mem_dc, page_str);

    let mode_label = if cfg.default_chinese_punct { "中" } else { "中." };
    SelectObject(mem_dc, f_badge.into());
    let badge_w = (text_width(mem_dc, mode_label) + px(10)).max(px(21));

    let header_total_w =
        preedit_w + px(22) + page_w + badge_gap + badge_w + header_pad_x * 2;

    let mut samples: Vec<&str> = Vec::new();
    let mut idx_ws: Vec<i32> = Vec::new();
    let mut item_ws: Vec<i32> = Vec::new();
    let mut widest_item = 0i32;

    if cfg.layout == CandidateLayout::Vertical {
        let mut target_n = page_count.min(3);
        while target_n > 2 {
            let ph = div_off
                + 1
                + row_pad_top
                + (target_n as i32) * row_h
                + ((target_n as i32 - 1).max(0)) * v_gap
                + row_pad_bot;
            if ph <= max_panel_h {
                break;
            }
            target_n -= 1;
        }
        for (i, &cand) in sample_pool[..target_n].iter().enumerate() {
            SelectObject(mem_dc, f_idx.into());
            let iw = text_width(mem_dc, &(i + 1).to_string());
            SelectObject(mem_dc, if i == 0 { f_cand_sel } else { f_cand }.into());
            let cw = text_width(mem_dc, cand);
            let full_w = item_pad_x * 2 + iw + idx_gap + cw;
            widest_item = widest_item.max(full_w);
            samples.push(cand);
            idx_ws.push(iw);
            item_ws.push(full_w);
        }
    } else {
        let max_items_w = max_stage_w - row_pad_x * 2;
        let mut used_w = 0i32;
        for (i, &cand) in sample_pool[..page_count].iter().enumerate() {
            SelectObject(mem_dc, f_idx.into());
            let iw = text_width(mem_dc, &(i + 1).to_string());
            SelectObject(mem_dc, if i == 0 { f_cand_sel } else { f_cand }.into());
            let cw = text_width(mem_dc, cand);
            let full_w = item_pad_x * 2 + iw + idx_gap + cw;
            let next_used = if i == 0 { full_w } else { used_w + item_gap + full_w };
            if i >= 1 && next_used > max_items_w {
                break;
            }
            used_w = next_used;
            samples.push(cand);
            idx_ws.push(iw);
            item_ws.push(full_w);
        }
    }

    let (panel_w, panel_h) = if cfg.layout == CandidateLayout::Vertical {
        let pw = header_total_w
            .max(widest_item + row_pad_x * 2)
            .clamp(px(185), max_stage_w);
        let ph = div_off
            + 1
            + row_pad_top
            + (samples.len() as i32) * row_h
            + ((samples.len() as i32 - 1).max(0)) * v_gap
            + row_pad_bot;
        (pw, ph)
    } else {
        let total_horiz = item_ws.iter().sum::<i32>()
            + item_gap * ((samples.len() as i32 - 1).max(0))
            + row_pad_x * 2;
        let pw = header_total_w.max(total_horiz).clamp(px(180), max_stage_w);
        let ph = div_off + 1 + row_pad_top + row_h + row_pad_bot;
        (pw, ph)
    };

    let px0 = stage.left + ((stage.right - stage.left) - panel_w) / 2;
    let py0 = stage.top + ((stage_h - panel_h) / 2).max(px(6));
    let px1 = px0 + panel_w;
    let py1 = py0 + panel_h;

    draw_aa_rounded_rect(
        pixels,
        buf_w,
        buf_h,
        px0,
        py0,
        px1,
        py1,
        8.0 * scale,
        cpal.bg,
        Some(cpal.border),
    );

    // Header badge
    let bx1 = px1 - header_pad_x;
    let bx0 = bx1 - badge_w;
    let by0 = py0 + header_top + (header_h - badge_h) / 2;
    let by1 = by0 + badge_h;
    draw_aa_rounded_rect(
        pixels,
        buf_w,
        buf_h,
        bx0,
        by0,
        bx1,
        by1,
        4.5 * scale,
        cpal.badge_bg,
        Some(cpal.badge_border),
    );

    // Caret bar
    if cfg.show_preedit_caret {
        let cx = px0 + header_pad_x + preedit_w + px(3);
        let cw = (scale.floor() as i32).max(1);
        let ch = px(pre_sz + 3).min(header_h - px(2)).max(px(12));
        let cy0 = py0 + header_top + (header_h - ch) / 2;
        fill_rect_px(pixels, buf_w, buf_h, cx, cy0, cx + cw, cy0 + ch, cpal.caret);
    }

    // Divider
    fill_rect_px(
        pixels,
        buf_w,
        buf_h,
        px0 + 1,
        py0 + div_off,
        px1 - 1,
        py0 + div_off + 1,
        cpal.divider,
    );

    let mut item_rects = Vec::with_capacity(samples.len());
    if cfg.layout == CandidateLayout::Vertical {
        let mut cy = py0 + div_off + 1 + row_pad_top;
        for _ in 0..samples.len() {
            item_rects.push(RECT {
                left: px0 + row_pad_x,
                top: cy,
                right: px1 - row_pad_x,
                bottom: cy + row_h,
            });
            cy += row_h + v_gap;
        }
    } else {
        let mut cx = px0 + row_pad_x;
        let ry = py0 + div_off + 1 + row_pad_top;
        for &iw in &item_ws {
            let x1 = (cx + iw).min(px1 - row_pad_x);
            item_rects.push(RECT {
                left: cx,
                top: ry,
                right: x1,
                bottom: ry + row_h,
            });
            cx = x1 + item_gap;
        }
    }

    if let Some(sel_rc) = item_rects.first() {
        draw_aa_rounded_rect(
            pixels,
            buf_w,
            buf_h,
            sel_rc.left,
            sel_rc.top,
            sel_rc.right,
            sel_rc.bottom,
            6.0 * scale,
            cpal.sel_bg,
            cpal.sel_border,
        );
    }

    let _ = GdiFlush();

    // Header ClearType text
    SelectObject(mem_dc, f_pre.into());
    draw_text(
        mem_dc,
        preedit_str,
        RECT {
            left: px0 + header_pad_x,
            top: py0 + header_top,
            right: bx0 - badge_gap - page_w - px(8),
            bottom: py0 + header_top + header_h,
        },
        cpal.preedit,
        DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
    );
    SelectObject(mem_dc, f_idx.into());
    draw_text(
        mem_dc,
        page_str,
        RECT {
            left: bx0 - badge_gap - page_w - px(4),
            top: py0 + header_top,
            right: bx0 - badge_gap,
            bottom: py0 + header_top + header_h,
        },
        cpal.muted,
        DT_RIGHT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
    );
    SelectObject(mem_dc, f_badge.into());
    draw_text(
        mem_dc,
        mode_label,
        RECT {
            left: bx0,
            top: by0 + px(1),
            right: bx1,
            bottom: by1,
        },
        cpal.badge_fg,
        DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
    );

    // Candidate items ClearType text
    let max_right = px1 - row_pad_x;
    for (i, (&cand, (&rc, &idx_w))) in samples
        .iter()
        .zip(item_rects.iter().zip(idx_ws.iter()))
        .enumerate()
    {
        if rc.left >= max_right {
            break;
        }
        let is_sel = i == 0;
        let idx_left = rc.left + item_pad_x;
        let idx_right = (idx_left + idx_w).min(rc.right);
        SelectObject(mem_dc, f_idx.into());
        draw_text(
            mem_dc,
            &(i + 1).to_string(),
            RECT {
                left: idx_left,
                top: rc.top + px(1),
                right: idx_right,
                bottom: rc.bottom,
            },
            if is_sel { cpal.sel_index } else { cpal.index },
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );
        let text_left = idx_left + idx_w + idx_gap;
        let text_right = (rc.right - item_pad_x + px(2)).min(rc.right);
        SelectObject(
            mem_dc,
            if is_sel { f_cand_sel } else { f_cand }.into(),
        );
        draw_text(
            mem_dc,
            cand,
            RECT {
                left: text_left,
                top: rc.top + px(1),
                right: text_right,
                bottom: rc.bottom,
            },
            if is_sel { cpal.sel_fg } else { cpal.text },
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS,
        );
    }

    let _ = DeleteObject(f_pre.into());
    let _ = DeleteObject(f_cand.into());
    let _ = DeleteObject(f_cand_sel.into());
    let _ = DeleteObject(f_idx.into());
    let _ = DeleteObject(f_badge.into());
}

#[inline]
fn rect_contains(r: RECT, x: i32, y: i32) -> bool {
    r.right > r.left && r.bottom > r.top && x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

fn draw_aa_shadow(
    pixels: &mut [u32],
    buf_w: i32,
    buf_h: i32,
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    radius: f32,
    is_light: bool,
) {
    let spread = 8.0_f32;
    let y_off = 3.0_f32;
    let max_alpha = if is_light { 0.14_f32 } else { 0.65_f32 };
    let min_x = ((x0 as f32 - spread).floor() as i32).max(0);
    let max_x = ((x1 as f32 + spread).ceil() as i32).min(buf_w);
    let min_y = ((y0 as f32 - spread + y_off).floor() as i32).max(0);
    let max_y = ((y1 as f32 + spread + y_off).ceil() as i32).min(buf_h);

    let cx = (x0 + x1) as f32 * 0.5;
    let cy = (y0 + y1) as f32 * 0.5 + y_off;
    let hw = (x1 - x0) as f32 * 0.5;
    let hh = (y1 - y0) as f32 * 0.5;
    let r = radius.min(hw).min(hh).max(0.0);
    let bx = (hw - r).max(0.0);
    let by = (hh - r).max(0.0);

    for py in min_y..max_y {
        let dy = (py as f32 + 0.5 - cy).abs() - by;
        let row_off = (py * buf_w) as usize;
        for px in min_x..max_x {
            let dx = (px as f32 + 0.5 - cx).abs() - bx;
            let qx = dx.max(0.0);
            let qy = dy.max(0.0);
            let sd = (qx * qx + qy * qy).sqrt() + dx.max(dy).min(0.0) - r;
            if sd > 0.0 && sd < spread {
                let t = 1.0 - sd / spread;
                let a = max_alpha * t * t;
                let idx = row_off + px as usize;
                pixels[idx] = blend_over(pixels[idx], 0.0, 0.0, 0.0, a);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn draw_font_picker_popover(
    mem_dc: HDC,
    pixels: &mut [u32],
    w: i32,
    h: i32,
    scale: f32,
    pal: &SettingsPalette,
    fonts: &UiFonts,
    font_btn_rc: RECT,
    state: &mut AppState,
    px: &impl Fn(i32) -> i32,
) {
    let pick_btn_hit = state
        .hit_regions
        .iter()
        .find(|r| r.action == ControlAction::PickFont)
        .copied();
    state.hit_regions.clear();
    if let Some(hit) = pick_btn_hit {
        state.hit_regions.push(hit);
    }

    let pop_right = font_btn_rc.right;
    let pop_left = (pop_right - px(292)).max(px(16));
    let pop_top = font_btn_rc.bottom + px(4);
    let pop_bottom = px(682).min(h - px(14));
    let pop_rc = RECT {
        left: pop_left,
        top: pop_top,
        right: pop_right,
        bottom: pop_bottom,
    };
    state.font_picker_rect = pop_rc;

    let is_light = resolve_is_light(state.config.theme);
    let r_card = 8.0 * scale;
    let r_ctrl = 6.0 * scale;
    let r_seg_inner = 4.5 * scale;

    draw_aa_shadow(
        pixels, w, h, pop_left, pop_top, pop_right, pop_bottom, r_card, is_light,
    );
    draw_aa_rounded_rect(
        pixels,
        w,
        h,
        pop_left,
        pop_top,
        pop_right,
        pop_bottom,
        r_card,
        pal.card_bg,
        Some(pal.btn_border),
    );

    let mut pop_labels: Vec<PendingLabel> = Vec::with_capacity(24);
    let top_bar_h = px(36);
    let search_rc = RECT {
        left: pop_left + px(8),
        top: pop_top + px(6),
        right: pop_left + px(8) + px(188),
        bottom: pop_top + px(30),
    };
    draw_aa_rounded_rect(
        pixels,
        w,
        h,
        search_rc.left,
        search_rc.top,
        search_rc.right,
        search_rc.bottom,
        r_ctrl,
        pal.seg_bg,
        Some(pal.seg_border),
    );

    let text_right_limit = if state.font_search.is_empty() {
        search_rc.right - px(8)
    } else {
        let clear_rc = RECT {
            left: search_rc.right - px(20),
            top: search_rc.top,
            right: search_rc.right,
            bottom: search_rc.bottom,
        };
        let hov_clr = state.hovered == Some(ControlAction::ClearFontSearch);
        state.hit_regions.push(HitRegion {
            rect: clear_rc,
            action: ControlAction::ClearFontSearch,
        });
        pop_labels.push(PendingLabel {
            text: "×".to_owned(),
            rc: clear_rc,
            color: if hov_clr { pal.text_primary } else { pal.text_muted },
            font: fonts.small,
            flags: DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        });
        clear_rc.left - px(2)
    };

    if state.font_search.is_empty() {
        pop_labels.push(PendingLabel {
            text: "输入筛选字体...".to_owned(),
            rc: RECT {
                left: search_rc.left + px(8),
                top: search_rc.top,
                right: text_right_limit,
                bottom: search_rc.bottom,
            },
            color: pal.text_muted,
            font: fonts.small,
            flags: DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS,
        });
    } else {
        SelectObject(mem_dc, fonts.small.into());
        let tw = text_width(mem_dc, &state.font_search);
        let text_left = search_rc.left + px(8);
        let caret_x = (text_left + tw + px(1)).min(text_right_limit - px(2));
        let caret_ch = px(12);
        let caret_y0 = search_rc.top + (search_rc.bottom - search_rc.top - caret_ch) / 2;
        fill_rect_px(
            pixels,
            w,
            h,
            caret_x,
            caret_y0,
            caret_x + 1,
            caret_y0 + caret_ch,
            pal.text_secondary,
        );
        pop_labels.push(PendingLabel {
            text: state.font_search.clone(),
            rc: RECT {
                left: text_left,
                top: search_rc.top,
                right: text_right_limit,
                bottom: search_rc.bottom,
            },
            color: pal.text_primary,
            font: fonts.small,
            flags: DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS,
        });
    }

    let seg_rc = RECT {
        left: search_rc.right + px(8),
        top: pop_top + px(6),
        right: pop_right - px(8),
        bottom: pop_top + px(30),
    };
    draw_segmented_control(
        pixels,
        w,
        h,
        r_ctrl,
        r_seg_inner,
        pal,
        fonts.small,
        seg_rc,
        &[
            ("中文", state.font_cjk_only, ControlAction::SetFontCjkOnly(true)),
            ("全部", !state.font_cjk_only, ControlAction::SetFontCjkOnly(false)),
        ],
        state.hovered,
        &mut state.hit_regions,
        &mut pop_labels,
    );

    fill_rect_px(
        pixels,
        w,
        h,
        pop_left + 1,
        pop_top + top_bar_h,
        pop_right - 1,
        pop_top + top_bar_h + 1,
        pal.divider,
    );

    let list_top = pop_top + top_bar_h + px(4);
    let list_bottom = pop_bottom - px(6);
    let item_h = px(28);
    let visible_rows = ((list_bottom - list_top) / item_h).max(1) as usize;

    let items = state.filtered_font_items();
    let total = items.len();
    let max_scroll = total.saturating_sub(visible_rows);
    state.font_scroll = state.font_scroll.min(max_scroll);
    state.font_visible_rows = visible_rows;
    state.font_total_items = total;

    let need_scroll = total > visible_rows;
    let row_right = if need_scroll {
        pop_right - px(14)
    } else {
        pop_right - px(6)
    };

    if need_scroll {
        let track_top = list_top;
        let track_bottom = list_top + (visible_rows as i32) * item_h;
        state.scrollbar_track_rect = RECT {
            left: pop_right - px(12),
            top: track_top,
            right: pop_right - px(2),
            bottom: track_bottom,
        };
        let track_h = (track_bottom - track_top).max(1);
        let thumb_h = ((track_h as usize * visible_rows) / total).max(px(20) as usize) as i32;
        let usable = (track_h - thumb_h).max(1);
        let thumb_y = track_top + ((usable as usize * state.font_scroll) / max_scroll.max(1)) as i32;
        let thumb_col = if state.dragging_scrollbar {
            pal.text_secondary
        } else {
            pal.text_muted
        };
        draw_aa_rounded_rect(
            pixels,
            w,
            h,
            pop_right - px(8),
            thumb_y,
            pop_right - px(4),
            thumb_y + thumb_h,
            2.0 * scale,
            thumb_col,
            None,
        );
    } else {
        state.scrollbar_track_rect = RECT::default();
    }

    if items.is_empty() {
        pop_labels.push(PendingLabel {
            text: "未找到匹配字体".to_owned(),
            rc: RECT {
                left: pop_left + px(12),
                top: list_top,
                right: pop_right - px(12),
                bottom: list_top + px(120),
            },
            color: pal.text_muted,
            font: fonts.small,
            flags: DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        });
    } else {
        for (row_idx, (idx, (label, _val, is_sel))) in items
            .iter()
            .enumerate()
            .skip(state.font_scroll)
            .take(visible_rows)
            .enumerate()
        {
            let ry0 = list_top + (row_idx as i32) * item_h;
            let ry1 = ry0 + item_h;
            let row_rc = RECT {
                left: pop_left + px(6),
                top: ry0,
                right: row_right,
                bottom: ry1,
            };
            let act = ControlAction::SelectFontItem(idx);
            let hov = state.hovered == Some(act);
            if hov {
                draw_aa_rounded_rect(
                    pixels,
                    w,
                    h,
                    row_rc.left,
                    row_rc.top,
                    row_rc.right,
                    row_rc.bottom,
                    r_seg_inner,
                    pal.btn_hover_bg,
                    None,
                );
            } else if *is_sel {
                draw_aa_rounded_rect(
                    pixels,
                    w,
                    h,
                    row_rc.left,
                    row_rc.top,
                    row_rc.right,
                    row_rc.bottom,
                    r_seg_inner,
                    pal.seg_bg,
                    None,
                );
            }
            state.hit_regions.push(HitRegion {
                rect: row_rc,
                action: act,
            });
            if *is_sel {
                pop_labels.push(PendingLabel {
                    text: "✓".to_owned(),
                    rc: RECT {
                        left: row_rc.left + px(6),
                        top: row_rc.top,
                        right: row_rc.left + px(20),
                        bottom: row_rc.bottom,
                    },
                    color: pal.text_primary,
                    font: fonts.small,
                    flags: DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
                });
            }
            pop_labels.push(PendingLabel {
                text: label.clone(),
                rc: RECT {
                    left: row_rc.left + px(22),
                    top: row_rc.top,
                    right: row_rc.right - px(8),
                    bottom: row_rc.bottom,
                },
                color: if *is_sel || hov {
                    pal.text_primary
                } else {
                    pal.text_secondary
                },
                font: fonts.small,
                flags: DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS,
            });
        }
    }

    let _ = GdiFlush();
    for lbl in pop_labels {
        SelectObject(mem_dc, lbl.font.into());
        draw_text(mem_dc, &lbl.text, lbl.rc, lbl.color, lbl.flags);
    }
}

fn scroll_to_track_y(state: &mut AppState, y: i32) {
    let track = state.scrollbar_track_rect;
    let track_h = (track.bottom - track.top).max(1);
    let max_scroll = state.font_total_items.saturating_sub(state.font_visible_rows);
    if max_scroll == 0 {
        state.font_scroll = 0;
        return;
    }
    let thumb_h = ((track_h as usize * state.font_visible_rows) / state.font_total_items.max(1))
        .max(20) as i32;
    let usable = (track_h - thumb_h).max(1);
    let rel = (y - track.top - thumb_h / 2).clamp(0, usable);
    state.font_scroll = ((rel as usize * max_scroll) + usable as usize / 2) / usable as usize;
}

unsafe fn handle_click(hwnd: HWND, state: &mut AppState, x: i32, y: i32) {
    if state.font_picker_open && rect_contains(state.scrollbar_track_rect, x, y) {
        state.dragging_scrollbar = true;
        let _ = SetCapture(hwnd);
        scroll_to_track_y(state, y);
        let _ = InvalidateRect(Some(hwnd), None, false);
        return;
    }

    let action = state
        .hit_regions
        .iter()
        .find(|r| rect_contains(r.rect, x, y))
        .map(|r| r.action);
    let Some(act) = action else {
        if state.font_picker_open && !rect_contains(state.font_picker_rect, x, y) {
            state.font_picker_open = false;
            state.hovered = None;
            state.dragging_scrollbar = false;
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
        return;
    };

    match act {
        ControlAction::SetTheme(t) => {
            if state.config.theme != t {
                state.config.theme = t;
                state.persist_and_push();
            }
        }
        ControlAction::SetLayout(l) => {
            if state.config.layout != l {
                state.config.layout = l;
                state.persist_and_push();
            }
        }
        ControlAction::SetHighlight(h) => {
            if state.config.highlight_style != h {
                state.config.highlight_style = h;
                state.persist_and_push();
            }
        }
        ControlAction::PickFont => {
            if state.font_picker_open {
                state.font_picker_open = false;
                state.hovered = None;
                state.dragging_scrollbar = false;
            } else {
                state.open_font_picker();
            }
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
        ControlAction::SetFontCjkOnly(only) => {
            if state.font_cjk_only != only {
                state.font_cjk_only = only;
                state.font_scroll = 0;
                state.dragging_scrollbar = false;
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
        }
        ControlAction::ClearFontSearch => {
            state.font_search.clear();
            state.font_scroll = 0;
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
        ControlAction::SelectFontItem(idx) => {
            let items = state.filtered_font_items();
            if let Some((_, chosen, _)) = items.get(idx) {
                state.config.font_family = chosen.clone();
                state.font_picker_open = false;
                state.hovered = None;
                state.dragging_scrollbar = false;
                state.persist_and_push();
            }
        }
        ControlAction::FontSizeDec => {
            if state.config.font_size > 12 {
                state.config.font_size -= 1;
                state.persist_and_push();
            }
        }
        ControlAction::FontSizeInc => {
            if state.config.font_size < 22 {
                state.config.font_size += 1;
                state.persist_and_push();
            }
        }
        ControlAction::SetSelectedWeight(w) => {
            if state.config.selected_weight != w {
                state.config.selected_weight = w;
                state.persist_and_push();
            }
        }
        ControlAction::PageSizeDec => {
            if state.config.page_size > 3 {
                state.config.page_size -= 1;
                state.persist_and_push();
            }
        }
        ControlAction::PageSizeInc => {
            if state.config.page_size < 9 {
                state.config.page_size += 1;
                state.persist_and_push();
            }
        }
        ControlAction::ToggleDefaultPunct => {
            state.config.default_chinese_punct = !state.config.default_chinese_punct;
            state.persist_and_push();
        }
        ControlAction::ToggleShift => {
            state.config.shift_toggle = !state.config.shift_toggle;
            state.persist_and_push();
        }
        ControlAction::ToggleInitialUpper => {
            state.config.initial_upper_english = !state.config.initial_upper_english;
            state.persist_and_push();
        }
        ControlAction::ToggleCaret => {
            state.config.show_preedit_caret = !state.config.show_preedit_caret;
            state.persist_and_push();
        }
        ControlAction::ToggleToast => {
            state.config.show_mode_toast = !state.config.show_mode_toast;
            state.persist_and_push();
        }
        ControlAction::ToggleLearning => {
            state.config.learning_enabled = !state.config.learning_enabled;
            state.persist_and_push();
        }
        ControlAction::ResetDefaults => {
            state.config = ImeConfig::default();
            state.persist_and_push();
        }
        ControlAction::CloseWindow => {
            let _ = DestroyWindow(hwnd);
        }
    }
}

unsafe fn paint_window(hwnd: HWND, hdc: HDC, state: &mut AppState) {
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let w = (rc.right - rc.left).max(1);
    let h = (rc.bottom - rc.top).max(1);
    let dpi = GetDpiForWindow(hwnd);
    let scale = if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 };

    let mem_dc = CreateCompatibleDC(Some(hdc));
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits_ptr: *mut core::ffi::c_void = null_mut();
    let Ok(dib) = CreateDIBSection(Some(hdc), &bmi, DIB_RGB_COLORS, &mut bits_ptr, None, 0) else {
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
    render_settings_surface(mem_dc, pixels, w, h, scale, state);
    let _ = BitBlt(hdc, 0, 0, w, h, Some(mem_dc), 0, 0, SRCCOPY);
    SelectObject(mem_dc, old_bmp);
    let _ = DeleteObject(dib.into());
    let _ = DeleteDC(mem_dc);
}

fn signed_lo_hi(lp: isize) -> (i32, i32) {
    let x = ((lp & 0xFFFF) as u16) as i16 as i32;
    let y = (((lp >> 16) & 0xFFFF) as u16) as i16 as i32;
    (x, y)
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let ptr = get_window_user_data(hwnd) as *mut AppState;
    if msg == WM_ERASEBKGND {
        return LRESULT(1);
    }
    if !ptr.is_null() {
        let state = &mut *ptr;
        match msg {
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                paint_window(hwnd, hdc, state);
                let _ = EndPaint(hwnd, &ps);
                return LRESULT(0);
            }
            WM_SETCURSOR => {
                if ((lparam.0 & 0xFFFF) as u32) == HTCLIENT {
                    if let Ok(cur) = LoadCursorW(
                        None,
                        if state.hovered.is_some() { IDC_HAND } else { IDC_ARROW },
                    ) {
                        SetCursor(Some(cur));
                    }
                    return LRESULT(1);
                }
            }
            WM_MOUSEMOVE => {
                if !state.tracking_mouse {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    let _ = TrackMouseEvent(&mut tme);
                    state.tracking_mouse = true;
                }
                let (x, y) = signed_lo_hi(lparam.0);
                if state.dragging_scrollbar && state.font_picker_open {
                    scroll_to_track_y(state, y);
                    let _ = InvalidateRect(Some(hwnd), None, false);
                    return LRESULT(0);
                }
                let hit = state
                    .hit_regions
                    .iter()
                    .find(|r| rect_contains(r.rect, x, y))
                    .map(|r| r.action);
                if let Ok(cur) = LoadCursorW(None, if hit.is_some() { IDC_HAND } else { IDC_ARROW })
                {
                    SetCursor(Some(cur));
                }
                if hit != state.hovered {
                    state.hovered = hit;
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                return LRESULT(0);
            }
            WM_MOUSELEAVE => {
                state.tracking_mouse = false;
                if state.hovered.is_some() {
                    state.hovered = None;
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                return LRESULT(0);
            }
            WM_LBUTTONDOWN => {
                let (x, y) = signed_lo_hi(lparam.0);
                handle_click(hwnd, state, x, y);
                return LRESULT(0);
            }
            WM_LBUTTONUP => {
                if state.dragging_scrollbar {
                    state.dragging_scrollbar = false;
                    let _ = ReleaseCapture();
                    let _ = InvalidateRect(Some(hwnd), None, false);
                    return LRESULT(0);
                }
            }
            WM_MOUSEWHEEL => {
                if state.font_picker_open {
                    let delta = ((wparam.0 >> 16) as u16) as i16;
                    let step = ((delta.abs() as usize) / 120).max(1) * 3;
                    let max_scroll = state
                        .font_total_items
                        .saturating_sub(state.font_visible_rows);
                    if delta > 0 {
                        state.font_scroll = state.font_scroll.saturating_sub(step);
                    } else if delta < 0 {
                        state.font_scroll = (state.font_scroll + step).min(max_scroll);
                    }
                    let _ = InvalidateRect(Some(hwnd), None, false);
                    return LRESULT(0);
                }
            }
            WM_CHAR => {
                if state.font_picker_open {
                    let code = wparam.0 as u32;
                    if code == 0x08 {
                        if state.font_search.pop().is_some() {
                            state.font_scroll = 0;
                            let _ = InvalidateRect(Some(hwnd), None, false);
                        }
                    } else if let Some(ch) = char::from_u32(code) {
                        if !ch.is_control() {
                            state.font_search.push(ch);
                            state.font_scroll = 0;
                            let _ = InvalidateRect(Some(hwnd), None, false);
                        }
                    }
                    return LRESULT(0);
                }
            }
            WM_KEYDOWN => {
                if state.font_picker_open {
                    let vk = wparam.0 as u16;
                    let max_scroll = state
                        .font_total_items
                        .saturating_sub(state.font_visible_rows);
                    let page = state.font_visible_rows.saturating_sub(1).max(1);
                    if vk == VK_ESCAPE.0 {
                        state.font_picker_open = false;
                        state.hovered = None;
                        state.dragging_scrollbar = false;
                        let _ = InvalidateRect(Some(hwnd), None, false);
                        return LRESULT(0);
                    } else if vk == VK_RETURN.0 {
                        let items = state.filtered_font_items();
                        if let Some((_, chosen, _)) = items.first() {
                            state.config.font_family = chosen.clone();
                            state.font_picker_open = false;
                            state.hovered = None;
                            state.dragging_scrollbar = false;
                            state.persist_and_push();
                        }
                        return LRESULT(0);
                    } else if vk == VK_UP.0 {
                        state.font_scroll = state.font_scroll.saturating_sub(1);
                        let _ = InvalidateRect(Some(hwnd), None, false);
                        return LRESULT(0);
                    } else if vk == VK_DOWN.0 {
                        state.font_scroll = (state.font_scroll + 1).min(max_scroll);
                        let _ = InvalidateRect(Some(hwnd), None, false);
                        return LRESULT(0);
                    } else if vk == VK_PRIOR.0 {
                        state.font_scroll = state.font_scroll.saturating_sub(page);
                        let _ = InvalidateRect(Some(hwnd), None, false);
                        return LRESULT(0);
                    } else if vk == VK_NEXT.0 {
                        state.font_scroll = (state.font_scroll + page).min(max_scroll);
                        let _ = InvalidateRect(Some(hwnd), None, false);
                        return LRESULT(0);
                    }
                }
            }
            WM_CLOSE => {
                let _ = DestroyWindow(hwnd);
                return LRESULT(0);
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                return LRESULT(0);
            }
            _ => {}
        }
    } else if msg == WM_DESTROY {
        PostQuitMessage(0);
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

fn render_offscreen_bmp_preview(
    path: &str,
    theme_override: Option<ThemeMode>,
    layout_override: Option<CandidateLayout>,
    highlight_override: Option<HighlightStyle>,
    open_font_picker: bool,
) -> Result<(), String> {
    let (mut cfg, connected) = load_initial_config();
    if let Some(t) = theme_override {
        cfg.theme = t;
    }
    if let Some(l) = layout_override {
        cfg.layout = l;
    }
    if let Some(hl) = highlight_override {
        cfg.highlight_style = hl;
    }
    let mut state = AppState::new(cfg, connected);
    if open_font_picker {
        state.open_font_picker();
    }
    let scale = 1.5_f32;
    let w = ((LOGICAL_W as f32) * scale).round() as i32;
    let h = ((LOGICAL_H as f32) * scale).round() as i32;

    unsafe {
        let sdc = GetDC(None);
        if sdc.is_invalid() {
            return Err("GetDC failed".into());
        }
        let mdc = CreateCompatibleDC(Some(sdc));
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits_ptr: *mut core::ffi::c_void = null_mut();
        let dib = CreateDIBSection(Some(sdc), &bmi, DIB_RGB_COLORS, &mut bits_ptr, None, 0)
            .map_err(|e| format!("CreateDIBSection: {e}"))?;
        let old_bmp = SelectObject(mdc, dib.into());
        let pixels = std::slice::from_raw_parts_mut(bits_ptr.cast::<u32>(), (w * h) as usize);
        render_settings_surface(mdc, pixels, w, h, scale, &mut state);
        let _ = GdiFlush();

        let row_bytes = (w * 4) as u32;
        let img_bytes = row_bytes * (h as u32);
        let file_size = 54 + img_bytes;
        let mut bmp = Vec::with_capacity(file_size as usize);
        bmp.extend_from_slice(b"BM");
        bmp.extend_from_slice(&file_size.to_le_bytes());
        bmp.extend_from_slice(&0u32.to_le_bytes());
        bmp.extend_from_slice(&54u32.to_le_bytes());
        bmp.extend_from_slice(&40u32.to_le_bytes());
        bmp.extend_from_slice(&w.to_le_bytes());
        bmp.extend_from_slice(&(-h).to_le_bytes());
        bmp.extend_from_slice(&1u16.to_le_bytes());
        bmp.extend_from_slice(&32u16.to_le_bytes());
        bmp.extend_from_slice(&0u32.to_le_bytes());
        bmp.extend_from_slice(&img_bytes.to_le_bytes());
        bmp.extend_from_slice(&0i32.to_le_bytes());
        bmp.extend_from_slice(&0i32.to_le_bytes());
        bmp.extend_from_slice(&0u32.to_le_bytes());
        bmp.extend_from_slice(&0u32.to_le_bytes());
        for &p in pixels.iter() {
            bmp.extend_from_slice(&p.to_le_bytes());
        }
        std::fs::write(path, bmp).map_err(|e| format!("write {path}: {e}"))?;

        SelectObject(mdc, old_bmp);
        let _ = DeleteObject(dib.into());
        let _ = DeleteDC(mdc);
        ReleaseDC(None, sdc);
    }
    Ok(())
}

fn main() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    let mut preview_path: Option<String> = None;
    let mut theme_override: Option<ThemeMode> = None;
    let mut layout_override: Option<CandidateLayout> = None;
    let mut highlight_override: Option<HighlightStyle> = None;
    let mut open_font_picker = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--render-preview" => {
                preview_path = args.next();
            }
            "--open-font-picker" => {
                open_font_picker = true;
            }
            "--theme" => {
                if let Some(t) = args.next() {
                    theme_override = match t.as_str() {
                        "dark" => Some(ThemeMode::Dark),
                        "light" => Some(ThemeMode::Light),
                        "auto" => Some(ThemeMode::Auto),
                        _ => None,
                    };
                }
            }
            "--layout" => {
                if let Some(l) = args.next() {
                    layout_override = match l.as_str() {
                        "horizontal" => Some(CandidateLayout::Horizontal),
                        "vertical" => Some(CandidateLayout::Vertical),
                        _ => None,
                    };
                }
            }
            "--highlight" => {
                if let Some(hl) = args.next() {
                    highlight_override = match hl.as_str() {
                        "subtle" => Some(HighlightStyle::Subtle),
                        "inverted" => Some(HighlightStyle::Inverted),
                        _ => None,
                    };
                }
            }
            _ => {}
        }
    }
    if let Some(ref path) = preview_path {
        if let Err(err) = render_offscreen_bmp_preview(
            path,
            theme_override,
            layout_override,
            highlight_override,
            open_font_picker,
        ) {
            eprintln!("render preview failed: {err}");
            std::process::exit(1);
        }
        return;
    }

    let mutex_w = to_wide_null(MUTEX_NAME);
    let _mutex = unsafe { CreateMutexW(None, false, PCWSTR(mutex_w.as_ptr())) }.ok();
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        let cls_w = to_wide_null(CLASS_NAME);
        let tit_w = to_wide_null(WINDOW_TITLE);
        if let Ok(existing) = unsafe { FindWindowW(PCWSTR(cls_w.as_ptr()), PCWSTR(tit_w.as_ptr())) }
        {
            if !existing.0.is_null() {
                unsafe {
                    let _ = ShowWindow(existing, SW_RESTORE);
                    let _ = SetForegroundWindow(existing);
                }
            }
        }
        return;
    }

    let hinst: HINSTANCE = unsafe { GetModuleHandleW(None) }
        .map(HINSTANCE::from)
        .unwrap_or_default();
    let cls_w = to_wide_null(CLASS_NAME);
    let tit_w = to_wide_null(WINDOW_TITLE);

    let icon_big = unsafe {
        LoadImageW(
            Some(hinst),
            PCWSTR(1usize as *const u16),
            IMAGE_ICON,
            GetSystemMetrics(SM_CXICON),
            GetSystemMetrics(SM_CYICON),
            LR_DEFAULTCOLOR,
        )
        .map(|h| HICON(h.0))
        .unwrap_or_default()
    };
    let icon_sm = unsafe {
        LoadImageW(
            Some(hinst),
            PCWSTR(1usize as *const u16),
            IMAGE_ICON,
            GetSystemMetrics(SM_CXSMICON),
            GetSystemMetrics(SM_CYSMICON),
            LR_DEFAULTCOLOR,
        )
        .map(|h| HICON(h.0))
        .unwrap_or_default()
    };

    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(wnd_proc),
        hInstance: hinst,
        hIcon: icon_big,
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW).ok() }.unwrap_or_default(),
        hIconSm: icon_sm,
        lpszClassName: PCWSTR(cls_w.as_ptr()),
        ..Default::default()
    };
    unsafe {
        RegisterClassExW(&wc);
    }

    let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
    let (cfg, connected) = load_initial_config();
    let mut app_state = Box::new(AppState::new(cfg, connected));

    let dpi = unsafe {
        let sdc = GetDC(None);
        let d = if !sdc.is_invalid() {
            let x = GetDeviceCaps(Some(sdc), LOGPIXELSX);
            ReleaseDC(None, sdc);
            x
        } else {
            96
        };
        d.max(96) as u32
    };
    let scale = dpi as f32 / 96.0;
    let client_w = ((LOGICAL_W as f32) * scale).round() as i32;
    let client_h = ((LOGICAL_H as f32) * scale).round() as i32;
    let mut wr = RECT {
        left: 0,
        top: 0,
        right: client_w,
        bottom: client_h,
    };
    unsafe {
        let _ = AdjustWindowRectEx(&mut wr, style, false, Default::default());
    }
    let outer_w = wr.right - wr.left;
    let outer_h = wr.bottom - wr.top;
    let sw = unsafe { GetSystemMetrics(SM_CXSCREEN) };
    let sh = unsafe { GetSystemMetrics(SM_CYSCREEN) };
    let wx = ((sw - outer_w) / 2).max(0);
    let wy = ((sh - outer_h) / 2).max(0);

    let Ok(hwnd) = (unsafe {
        CreateWindowExW(
            Default::default(),
            PCWSTR(cls_w.as_ptr()),
            PCWSTR(tit_w.as_ptr()),
            style,
            wx,
            wy,
            outer_w,
            outer_h,
            None,
            None,
            Some(hinst),
            None,
        )
    }) else {
        return;
    };

    app_state.hwnd = hwnd;
    let state_ptr = (&mut *app_state as *mut AppState) as isize;
    unsafe {
        set_window_user_data(hwnd, state_ptr);
        let corner: DWM_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            (&corner as *const DWM_WINDOW_CORNER_PREFERENCE).cast(),
            std::mem::size_of_val(&corner) as u32,
        );
    }
    sync_titlebar_theme(
        hwnd,
        app_state.config.theme,
        &mut app_state.last_dark_titlebar,
    );

    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = InvalidateRect(Some(hwnd), None, false);
    }

    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).into() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        set_window_user_data(hwnd, 0);
    }
}

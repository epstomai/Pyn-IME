//! Floating Windows 11 DPI-aware GDI candidate window (Light/Dark auto theme, rounded corners).

use std::sync::Once;

use ime_ipc::pipe::to_wide_null;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUNDSMALL,
    DWM_WINDOW_CORNER_PREFERENCE,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, ClientToScreen, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontW,
    CreatePen, CreateSolidBrush, DeleteDC, DeleteObject, DrawTextW, EndPaint, FillRect, GetDC,
    GetMonitorInfoW, GetTextExtentPoint32W, InvalidateRect, MonitorFromPoint, ReleaseDC, RoundRect,
    SelectObject, SetBkMode, SetTextColor, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS,
    DEFAULT_CHARSET, DEFAULT_PITCH, DT_CENTER, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_RIGHT,
    DT_SINGLELINE, DT_VCENTER, FF_SWISS, HDC, HFONT, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    OUT_DEFAULT_PRECIS, PAINTSTRUCT, PS_SOLID, SRCCOPY, TRANSPARENT,
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

const CLASS_NAME: &str = "InputEngineCandidateWnd";
const TOAST_TIMER_ID: usize = 1;
static REGISTER_CLASS_ONCE: Once = Once::new();

#[derive(Debug, Clone, Default)]
pub struct CandidateViewState {
    pub preedit: String,
    pub mode_label: String,
    pub page_text: String,
    pub candidates: Vec<String>,
    pub selected_in_page: usize,
    pub is_toast: bool,
}

struct Palette {
    bg: u32,
    border: u32,
    preedit: u32,
    muted: u32,
    sel_bg: u32,
    sel_fg: u32,
    text: u32,
    divider: u32,
}

fn current_palette() -> Palette {
    let is_light = read_apps_use_light_theme();
    if is_light {
        Palette {
            bg: 0x00FF_FFFF,
            border: 0x00E0_E0E0,
            preedit: 0x001A_1A1A,
            muted: 0x0075_7575,
            sel_bg: 0x00FF_F1E5,
            sel_fg: 0x00B8_4E00,
            text: 0x001A_1A1A,
            divider: 0x00EE_EEEE,
        }
    } else {
        Palette {
            bg: 0x0024_2424,
            border: 0x003D_3D3D,
            preedit: 0x00F3_F3F3,
            muted: 0x00A0_A0A0,
            sel_bg: 0x004D_3624,
            sel_fg: 0x00FF_AA60,
            text: 0x00F3_F3F3,
            divider: 0x0033_3333,
        }
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
    state: Box<CandidateViewState>,
}

impl CandidateWindow {
    pub fn new() -> windows_core::Result<Self> {
        let hinst = dll_hinstance();
        REGISTER_CLASS_ONCE.call_once(|| {
            let class_w = to_wide_null(CLASS_NAME);
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

        let class_w = to_wide_null(CLASS_NAME);
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

        let state = Box::new(CandidateViewState::default());
        let ptr = (&*state as *const CandidateViewState) as isize;
        unsafe {
            set_window_user_data(hwnd, ptr);
            let corner: DWM_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUNDSMALL;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                (&corner as *const DWM_WINDOW_CORNER_PREFERENCE).cast(),
                std::mem::size_of_val(&corner) as u32,
            );
        }
        Ok(Self { hwnd, state })
    }

    pub fn update_and_show(&mut self, view: CandidateViewState, caret_rect: Option<RECT>) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TOAST_TIMER_ID);
        }
        *self.state = view;
        let state_ptr = (&*self.state as *const CandidateViewState) as isize;
        unsafe {
            set_window_user_data(self.hwnd, state_ptr);
        }

        let dpi = unsafe { GetDpiForWindow(self.hwnd) };
        let scale = if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 };
        let px = |v: i32| ((v as f32) * scale).round() as i32;

        let (anchor_x, anchor_top, anchor_bottom, work) = monitor_work_rect(caret_rect, &px);
        let max_win_w = (work.right - work.left - px(24)).max(px(320));
        let (win_w, win_h) = measure_window_size(self.hwnd, &self.state, max_win_w, &px);
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
        }
    }

    pub fn show_mode_toast(&mut self, label: &str, caret_rect: Option<RECT>) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TOAST_TIMER_ID);
        }
        *self.state = CandidateViewState {
            preedit: label.to_owned(),
            is_toast: true,
            ..Default::default()
        };
        let state_ptr = (&*self.state as *const CandidateViewState) as isize;
        unsafe {
            set_window_user_data(self.hwnd, state_ptr);
        }

        let dpi = unsafe { GetDpiForWindow(self.hwnd) };
        let scale = if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 };
        let px = |v: i32| ((v as f32) * scale).round() as i32;

        let (anchor_x, anchor_top, anchor_bottom, work) = monitor_work_rect(caret_rect, &px);
        let win_w = px(42);
        let win_h = px(34);
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
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }
}

impl Drop for CandidateWindow {
    fn drop(&mut self) {
        unsafe {
            set_window_user_data(self.hwnd, 0);
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

struct HorizontalLayout {
    pad_x: i32,
    pad_y: i32,
    header_h: i32,
    div_y: i32,
    row_y: i32,
    row_h: i32,
    item_pad_x: i32,
    meta: String,
    meta_w: i32,
    item_rects: Vec<(i32, i32)>,
    win_w: i32,
    win_h: i32,
}

unsafe fn compute_horizontal_layout(
    hdc: HDC,
    state: &CandidateViewState,
    max_win_w: i32,
    font_main: HFONT,
    font_bold: HFONT,
    font_small: HFONT,
    px: &impl Fn(i32) -> i32,
) -> HorizontalLayout {
    let pad_x = px(12);
    let pad_y = px(7);
    let header_h = px(24);
    let div_y = pad_y + header_h + px(2);
    let row_y = pad_y + header_h + px(6);
    let row_h = px(28);
    let item_pad_x = px(8);
    let item_gap = px(6);

    let meta = if state.page_text.is_empty() {
        state.mode_label.clone()
    } else {
        format!("{}  {}", state.mode_label, state.page_text)
    };

    let old_font = SelectObject(hdc, font_main.into());
    let preedit_w = text_width(hdc, &state.preedit);

    SelectObject(hdc, font_small.into());
    let meta_w = text_width(hdc, &meta);
    let header_w = preedit_w + px(24) + meta_w;

    let mut item_rects = Vec::with_capacity(state.candidates.len());
    if !state.candidates.is_empty() {
        SelectObject(hdc, font_bold.into());
        let mut cur_x = pad_x;
        for (i, cand) in state.candidates.iter().enumerate() {
            let label = format!("{}. {}", i + 1, cand);
            let tw = text_width(hdc, &label);
            let item_w = tw + item_pad_x * 2;
            let x0 = cur_x;
            let x1 = x0 + item_w;
            item_rects.push((x0, x1));
            cur_x = x1 + item_gap;
        }
    }

    SelectObject(hdc, old_font);

    let cands_w = item_rects
        .last()
        .map(|&(_, x1)| (x1 - pad_x).max(0))
        .unwrap_or(0);
    let max_w = max_win_w.clamp(px(320), px(960));
    let win_w = (header_w.max(cands_w).max(px(160)) + pad_x * 2).clamp(px(190), max_w);
    let win_h = if state.candidates.is_empty() {
        pad_y * 2 + header_h
    } else {
        pad_y * 2 + header_h + px(6) + row_h
    };

    HorizontalLayout {
        pad_x,
        pad_y,
        header_h,
        div_y,
        row_y,
        row_h,
        item_pad_x,
        meta,
        meta_w,
        item_rects,
        win_w,
        win_h,
    }
}

fn measure_window_size(
    hwnd: HWND,
    state: &CandidateViewState,
    max_win_w: i32,
    px: &impl Fn(i32) -> i32,
) -> (i32, i32) {
    unsafe {
        let hdc = GetDC(Some(hwnd));
        if !hdc.is_invalid() {
            let font_main = create_ui_font(px(15), 400);
            let font_bold = create_ui_font(px(15), 600);
            let font_small = create_ui_font(px(12), 400);
            let layout = compute_horizontal_layout(
                hdc, state, max_win_w, font_main, font_bold, font_small, px,
            );
            let _ = DeleteObject(font_main.into());
            let _ = DeleteObject(font_bold.into());
            let _ = DeleteObject(font_small.into());
            ReleaseDC(Some(hwnd), hdc);
            return (layout.win_w, layout.win_h);
        }
    }

    let pad_x = px(12);
    let pad_y = px(7);
    let header_h = px(24);
    let row_h = px(28);
    let win_w = (px(190) + pad_x * 2).min(max_win_w.clamp(px(320), px(960)));
    let win_h = if state.candidates.is_empty() {
        pad_y * 2 + header_h
    } else {
        pad_y * 2 + header_h + px(6) + row_h
    };
    (win_w, win_h)
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

unsafe fn create_ui_font(height: i32, weight: i32) -> HFONT {
    let face = to_wide_null("Microsoft YaHei UI");
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

unsafe fn text_width(hdc: windows::Win32::Graphics::Gdi::HDC, text: &str) -> i32 {
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
            let ptr = get_window_user_data(hwnd) as *const CandidateViewState;
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
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn paint_candidate_panel(
    hwnd: HWND,
    hdc: windows::Win32::Graphics::Gdi::HDC,
    state: &CandidateViewState,
) {
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let w = (rc.right - rc.left).max(1);
    let h = (rc.bottom - rc.top).max(1);

    let dpi = GetDpiForWindow(hwnd);
    let scale = if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 };
    let px = |v: i32| ((v as f32) * scale).round() as i32;

    let pal = current_palette();
    let mem_dc = CreateCompatibleDC(Some(hdc));
    let bmp = CreateCompatibleBitmap(hdc, w, h);
    let old_bmp = SelectObject(mem_dc, bmp.into());

    let fill_color = if state.is_toast { pal.sel_bg } else { pal.bg };
    let bg_brush = CreateSolidBrush(COLORREF(fill_color));
    let border_brush = CreateSolidBrush(COLORREF(pal.border));
    FillRect(mem_dc, &rc, border_brush);
    let inner_rc = RECT {
        left: rc.left + 1,
        top: rc.top + 1,
        right: rc.right - 1,
        bottom: rc.bottom - 1,
    };
    FillRect(mem_dc, &inner_rc, bg_brush);
    let _ = DeleteObject(bg_brush.into());
    let _ = DeleteObject(border_brush.into());

    SetBkMode(mem_dc, TRANSPARENT);
    let font_main = create_ui_font(px(15), 400);
    let font_bold = create_ui_font(px(15), 600);
    let font_small = create_ui_font(px(12), 400);

    let old_font = SelectObject(mem_dc, font_main.into());
    if state.is_toast {
        SelectObject(mem_dc, font_bold.into());
        SetTextColor(mem_dc, COLORREF(pal.sel_fg));
        let mut toast_w: Vec<u16> = state.preedit.encode_utf16().collect();
        let mut toast_rc = inner_rc;
        if !toast_w.is_empty() {
            DrawTextW(
                mem_dc,
                &mut toast_w,
                &mut toast_rc,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            );
        }
    } else {
        let layout = compute_horizontal_layout(
            mem_dc, state, w, font_main, font_bold, font_small, &px,
        );

        // Header: preedit on left, mode + page on right
        SetTextColor(mem_dc, COLORREF(pal.preedit));
        let pre_right = if layout.meta_w > 0 {
            (w - layout.pad_x - layout.meta_w - px(8)).max(layout.pad_x)
        } else {
            (w - layout.pad_x).max(layout.pad_x)
        };
        let mut pre_w: Vec<u16> = state.preedit.encode_utf16().collect();
        let mut pre_rc = RECT {
            left: layout.pad_x,
            top: layout.pad_y,
            right: pre_right,
            bottom: layout.pad_y + layout.header_h,
        };
        if !pre_w.is_empty() && pre_rc.right > pre_rc.left {
            DrawTextW(
                mem_dc,
                &mut pre_w,
                &mut pre_rc,
                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS,
            );
        }

        SelectObject(mem_dc, font_small.into());
        SetTextColor(mem_dc, COLORREF(pal.muted));
        let mut meta_w: Vec<u16> = layout.meta.encode_utf16().collect();
        let mut meta_rc = RECT {
            left: layout.pad_x,
            top: layout.pad_y,
            right: w - layout.pad_x,
            bottom: layout.pad_y + layout.header_h,
        };
        if !meta_w.is_empty() {
            DrawTextW(
                mem_dc,
                &mut meta_w,
                &mut meta_rc,
                DT_RIGHT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            );
        }

        if !state.candidates.is_empty() {
            let div_rc = RECT {
                left: layout.pad_x,
                top: layout.div_y,
                right: w - layout.pad_x,
                bottom: layout.div_y + 1,
            };
            let div_brush = CreateSolidBrush(COLORREF(pal.divider));
            FillRect(mem_dc, &div_rc, div_brush);
            let _ = DeleteObject(div_brush.into());

            let max_right = (w - layout.pad_x).max(layout.pad_x);
            let y0 = layout.row_y;
            let y1 = layout.row_y + layout.row_h;
            let sel_brush = CreateSolidBrush(COLORREF(pal.sel_bg));
            let sel_pen = CreatePen(PS_SOLID, 1, COLORREF(pal.sel_bg));

            for (i, (cand, &(x0, x1))) in state
                .candidates
                .iter()
                .zip(layout.item_rects.iter())
                .enumerate()
            {
                if x0 >= max_right {
                    break;
                }
                let truncated = x1 > max_right;
                let x1_clamped = x1.min(max_right);
                let is_sel = i == state.selected_in_page;
                if is_sel {
                    let old_br = SelectObject(mem_dc, sel_brush.into());
                    let old_pen = SelectObject(mem_dc, sel_pen.into());
                    let r = px(6);
                    let _ = RoundRect(mem_dc, x0, y0, x1_clamped, y1, r, r);
                    SelectObject(mem_dc, old_br);
                    SelectObject(mem_dc, old_pen);
                    SelectObject(mem_dc, font_bold.into());
                    SetTextColor(mem_dc, COLORREF(pal.sel_fg));
                } else {
                    SelectObject(mem_dc, font_main.into());
                    SetTextColor(mem_dc, COLORREF(pal.text));
                }

                let label = format!("{}. {}", i + 1, cand);
                let mut item_w: Vec<u16> = label.encode_utf16().collect();
                let (mut item_rc, fmt) = if truncated {
                    (
                        RECT {
                            left: x0 + layout.item_pad_x,
                            top: y0,
                            right: (x1_clamped - px(4)).max(x0 + layout.item_pad_x),
                            bottom: y1,
                        },
                        DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS,
                    )
                } else {
                    (
                        RECT {
                            left: x0,
                            top: y0,
                            right: x1_clamped,
                            bottom: y1,
                        },
                        DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
                    )
                };
                if item_rc.right > item_rc.left {
                    DrawTextW(mem_dc, &mut item_w, &mut item_rc, fmt);
                }
            }

            let _ = DeleteObject(sel_brush.into());
            let _ = DeleteObject(sel_pen.into());
        }
    }

    let _ = BitBlt(hdc, 0, 0, w, h, Some(mem_dc), 0, 0, SRCCOPY);
    SelectObject(mem_dc, old_font);
    SelectObject(mem_dc, old_bmp);
    let _ = DeleteObject(font_main.into());
    let _ = DeleteObject(font_bold.into());
    let _ = DeleteObject(font_small.into());
    let _ = DeleteObject(bmp.into());
    let _ = DeleteDC(mem_dc);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn horizontal_layout_measures_fixed_height_and_growing_width() {
        let mut wnd = CandidateWindow::new().unwrap();
        let px = |v: i32| v;

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

        let (w2, h2) = measure_window_size(wnd.hwnd, &view_two, 960, &px);
        let (w5, h5) = measure_window_size(wnd.hwnd, &view_five, 960, &px);

        assert_eq!(h2, h5);
        assert!(w5 > h5);
        assert!(w5 > w2);

        wnd.update_and_show(view_five.clone(), None);
        let mut view_sel = view_five;
        view_sel.selected_in_page = 3;
        wnd.update_and_show(view_sel, None);
        wnd.hide();
    }
}

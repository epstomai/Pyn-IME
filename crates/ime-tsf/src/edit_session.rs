//! Synchronous TSF `ITfEditSession` implementations for context reading, composition, and commit.

use std::cell::RefCell;
use std::mem::ManuallyDrop;
use std::rc::Rc;

use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::System::Variant::{
    VariantClear, VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_I4,
};
use windows::Win32::UI::TextServices::{
    ITfComposition, ITfCompositionSink, ITfContext, ITfContextComposition, ITfContextView,
    ITfEditSession,
    ITfEditSession_Impl, ITfInsertAtSelection, ITfRange, GUID_PROP_ATTRIBUTE,
    INSERT_TEXT_AT_SELECTION_FLAGS, TF_AE_NONE, TF_ANCHOR_END, TF_ANCHOR_START,
    TF_DEFAULT_SELECTION, TF_IAS_QUERYONLY, TF_SELECTION, TF_SELECTIONSTYLE,
};
use windows::Win32::UI::WindowsAndMessaging::{GetAncestor, GetWindowRect, GA_ROOT};
use windows_core::{implement, BOOL, Interface, Result};

use crate::debug_log::tsf_log;

fn get_anchor_range(
    context: &ITfContext,
    ec: u32,
    composition: Option<&ITfComposition>,
) -> Result<ITfRange> {
    if let Some(comp) = composition {
        return unsafe { comp.GetRange() };
    }
    let mut sel = [TF_SELECTION::default()];
    let mut fetched = 0u32;
    unsafe { context.GetSelection(ec, TF_DEFAULT_SELECTION, &mut sel, &mut fetched)? };
    let range_opt = unsafe { ManuallyDrop::take(&mut sel[0].range) };
    if fetched == 0 {
        return unsafe { context.GetStart(ec) };
    }
    match range_opt {
        Some(r) => Ok(r),
        None => unsafe { context.GetStart(ec) },
    }
}

fn set_selection_to_end(context: &ITfContext, ec: u32, range: &ITfRange) -> Result<()> {
    let end_range = unsafe { range.Clone()? };
    unsafe { end_range.Collapse(ec, TF_ANCHOR_END)? };
    let mut sel = [TF_SELECTION {
        range: ManuallyDrop::new(Some(end_range)),
        style: TF_SELECTIONSTYLE {
            ase: TF_AE_NONE,
            fInterimChar: BOOL(0),
        },
    }];
    let res = unsafe { context.SetSelection(ec, &sel) };
    unsafe { ManuallyDrop::drop(&mut sel[0].range) };
    res
}

fn near(a: i32, b: i32) -> bool {
    (a - b).abs() <= 1
}

/// Some hosts (Chromium/Electron, WPF, several Win32 frameworks) answer `GetTextExt` for a freshly
/// focused document or a just-inserted composition before laying it out, and report the screen
/// origin of the view window (client or frame top-left) instead of the real text position. Treat
/// such a rect as "no layout yet" so the candidate window never jumps to the window's corner.
fn is_window_origin_placeholder(view: &ITfContextView, rc: &RECT) -> bool {
    let hwnd = match unsafe { view.GetWnd() } {
        Ok(h) if !h.0.is_null() => h,
        _ => return false,
    };
    let root = unsafe { GetAncestor(hwnd, GA_ROOT) };
    let candidates: [Option<HWND>; 2] = [
        Some(hwnd),
        (!root.0.is_null() && root != hwnd).then_some(root),
    ];
    for w in candidates.into_iter().flatten() {
        let mut wr = RECT::default();
        if unsafe { GetWindowRect(w, &mut wr) }.is_ok() && near(rc.left, wr.left) && near(rc.top, wr.top)
        {
            return true;
        }
        let mut origin = POINT::default();
        if unsafe { ClientToScreen(w, &mut origin) }.as_bool()
            && near(rc.left, origin.x)
            && near(rc.top, origin.y)
        {
            return true;
        }
    }
    false
}

fn get_caret_rect(context: &ITfContext, ec: u32, range: &ITfRange) -> Option<RECT> {
    let view = unsafe { context.GetActiveView() }.ok()?;
    let mut rc = RECT::default();
    let mut clipped = BOOL(0);
    if let Err(e) = unsafe { view.GetTextExt(ec, range, &mut rc, &mut clipped) } {
        tsf_log!("caret", "GetTextExt failed hr=0x{:08x}", e.code().0 as u32);
        return None;
    }
    if rc == RECT::default() {
        tsf_log!("caret", "GetTextExt returned empty rect");
        return None;
    }
    if is_window_origin_placeholder(&view, &rc) {
        tsf_log!(
            "caret",
            "GetTextExt rejected window-origin placeholder rc=({},{},{},{})",
            rc.left,
            rc.top,
            rc.right,
            rc.bottom
        );
        return None;
    }
    tsf_log!(
        "caret",
        "GetTextExt rc=({},{},{},{}) clipped={}",
        rc.left,
        rc.top,
        rc.right,
        rc.bottom,
        clipped.as_bool()
    );
    Some(rc)
}

/// Read-only session that re-measures the live composition after the host reports a layout change.
#[implement(ITfEditSession)]
pub struct RefreshCaretSession {
    context: ITfContext,
    composition: ITfComposition,
    on_rect: Box<dyn Fn(RECT)>,
}

impl RefreshCaretSession {
    pub fn new(context: ITfContext, composition: ITfComposition, on_rect: Box<dyn Fn(RECT)>) -> Self {
        Self {
            context,
            composition,
            on_rect,
        }
    }
}

impl ITfEditSession_Impl for RefreshCaretSession_Impl {
    fn DoEditSession(&self, ec: u32) -> Result<()> {
        let range = unsafe { self.composition.GetRange()? };
        if let Some(rc) = get_caret_rect(&self.context, ec, &range) {
            (self.on_rect)(rc);
        }
        Ok(())
    }
}

fn apply_display_attr(context: &ITfContext, ec: u32, range: &ITfRange, atom: u32) {
    if atom == 0 {
        return;
    }
    if let Ok(prop) = unsafe { context.GetProperty(&GUID_PROP_ATTRIBUTE) } {
        let mut var = VARIANT {
            Anonymous: VARIANT_0 {
                Anonymous: ManuallyDrop::new(VARIANT_0_0 {
                    vt: VT_I4,
                    wReserved1: 0,
                    wReserved2: 0,
                    wReserved3: 0,
                    Anonymous: VARIANT_0_0_0 { lVal: atom as i32 },
                }),
            },
        };
        unsafe {
            let _ = prop.SetValue(ec, range, &var);
            let _ = VariantClear(&mut var);
        }
    }
}

fn clear_display_attr(context: &ITfContext, ec: u32, range: &ITfRange) {
    if let Ok(prop) = unsafe { context.GetProperty(&GUID_PROP_ATTRIBUTE) } {
        unsafe {
            let _ = prop.Clear(ec, range);
        }
    }
}

#[implement(ITfEditSession)]
pub struct ReadContextAndCaretSession {
    context: ITfContext,
    composition: Option<ITfComposition>,
    out: Rc<RefCell<(String, Option<RECT>)>>,
}

impl ReadContextAndCaretSession {
    pub fn new(
        context: ITfContext,
        composition: Option<ITfComposition>,
        out: Rc<RefCell<(String, Option<RECT>)>>,
    ) -> Self {
        Self {
            context,
            composition,
            out,
        }
    }
}

fn read_preceding_context(ec: u32, base_range: &ITfRange) -> String {
    if let Ok(pre_range) = unsafe { base_range.Clone() } {
        if unsafe { pre_range.Collapse(ec, TF_ANCHOR_START) }.is_ok() {
            let mut shifted = 0i32;
            let _ = unsafe { pre_range.ShiftStart(ec, -32, &mut shifted, std::ptr::null()) };
            let mut buf = [0u16; 64];
            let mut read_len = 0u32;
            if unsafe { pre_range.GetText(ec, 0, &mut buf, &mut read_len) }.is_ok() {
                let n = (read_len as usize).min(buf.len());
                return String::from_utf16_lossy(&buf[..n]);
            }
        }
    }
    String::new()
}

impl ITfEditSession_Impl for ReadContextAndCaretSession_Impl {
    fn DoEditSession(&self, ec: u32) -> Result<()> {
        let mut ctx_str = String::new();
        let mut caret_rect = None;

        if let Ok(base_range) = get_anchor_range(&self.context, ec, self.composition.as_ref()) {
            caret_rect = get_caret_rect(&self.context, ec, &base_range);
            ctx_str = read_preceding_context(ec, &base_range);
        }
        *self.out.borrow_mut() = (ctx_str, caret_rect);
        Ok(())
    }
}

fn start_new_composition(
    context: &ITfContext,
    ec: u32,
    comp_sink: &ITfCompositionSink,
) -> Result<ITfComposition> {
    let range = if let Ok(insert) = context.cast::<ITfInsertAtSelection>() {
        match unsafe { insert.InsertTextAtSelection(ec, TF_IAS_QUERYONLY, &[]) } {
            Ok(r) => r,
            Err(_) => get_anchor_range(context, ec, None)?,
        }
    } else {
        get_anchor_range(context, ec, None)?
    };
    let comp_ctx: ITfContextComposition = context.cast()?;
    unsafe { comp_ctx.StartComposition(ec, &range, comp_sink) }
}

#[implement(ITfEditSession)]
pub struct UpdateCompositionSession {
    context: ITfContext,
    composition_slot: Rc<RefCell<Option<ITfComposition>>>,
    comp_sink: ITfCompositionSink,
    preedit: String,
    display_attr_atom: u32,
    caret_out: Rc<RefCell<Option<RECT>>>,
    context_out: Rc<RefCell<Option<String>>>,
}

impl UpdateCompositionSession {
    pub fn new(
        context: ITfContext,
        composition_slot: Rc<RefCell<Option<ITfComposition>>>,
        comp_sink: ITfCompositionSink,
        preedit: String,
        display_attr_atom: u32,
        caret_out: Rc<RefCell<Option<RECT>>>,
        context_out: Rc<RefCell<Option<String>>>,
    ) -> Self {
        Self {
            context,
            composition_slot,
            comp_sink,
            preedit,
            display_attr_atom,
            caret_out,
            context_out,
        }
    }
}

impl ITfEditSession_Impl for UpdateCompositionSession_Impl {
    fn DoEditSession(&self, ec: u32) -> Result<()> {
        let wtext: Vec<u16> = self.preedit.encode_utf16().collect();
        let existing = self.composition_slot.borrow().clone();
        let mut comp_and_range = None;

        if let Some(c) = existing {
            if let Ok(range) = unsafe { c.GetRange() } {
                if unsafe { range.SetText(ec, 0, &wtext) }.is_ok() {
                    comp_and_range = Some((c, range));
                }
            }
        }

        let (comp, range) = match comp_and_range {
            Some(pair) => pair,
            None => {
                *self.composition_slot.borrow_mut() = None;
                let c = match start_new_composition(&self.context, ec, &self.comp_sink) {
                    Ok(c) => c,
                    Err(e) => {
                        crate::debug_log::tsf_log!(
                            "edit_session",
                            "UpdateComposition start_new_composition failed hr=0x{:08x}",
                            e.code().0 as u32
                        );
                        return Err(e);
                    }
                };
                let range = match unsafe { c.GetRange() } {
                    Ok(r) => r,
                    Err(e) => {
                        crate::debug_log::tsf_log!(
                            "edit_session",
                            "UpdateComposition GetRange failed hr=0x{:08x}",
                            e.code().0 as u32
                        );
                        return Err(e);
                    }
                };
                if let Err(e) = unsafe { range.SetText(ec, 0, &wtext) } {
                    crate::debug_log::tsf_log!(
                        "edit_session",
                        "UpdateComposition SetText failed hr=0x{:08x}",
                        e.code().0 as u32
                    );
                    return Err(e);
                }
                (c, range)
            }
        };

        apply_display_attr(&self.context, ec, &range, self.display_attr_atom);
        let _ = set_selection_to_end(&self.context, ec, &range);
        let caret = get_caret_rect(&self.context, ec, &range);
        let ctx_str = read_preceding_context(ec, &range);

        *self.composition_slot.borrow_mut() = Some(comp);
        *self.caret_out.borrow_mut() = caret;
        *self.context_out.borrow_mut() = Some(ctx_str);
        Ok(())
    }
}

#[implement(ITfEditSession)]
pub struct CommitCompositionSession {
    context: ITfContext,
    composition: Option<ITfComposition>,
    comp_sink: ITfCompositionSink,
    text: String,
}

impl CommitCompositionSession {
    pub fn new(
        context: ITfContext,
        composition: Option<ITfComposition>,
        comp_sink: ITfCompositionSink,
        text: String,
    ) -> Self {
        Self {
            context,
            composition,
            comp_sink,
            text,
        }
    }
}

impl ITfEditSession_Impl for CommitCompositionSession_Impl {
    fn DoEditSession(&self, ec: u32) -> Result<()> {
        let wtext: Vec<u16> = self.text.encode_utf16().collect();
        if let Some(ref comp) = self.composition {
            if let Ok(range) = unsafe { comp.GetRange() } {
                clear_display_attr(&self.context, ec, &range);
                if unsafe { range.SetText(ec, 0, &wtext) }.is_ok() {
                    let _ = set_selection_to_end(&self.context, ec, &range);
                    let _ = unsafe { comp.EndComposition(ec) };
                    return Ok(());
                }
            }
        }

        if let Ok(comp) = start_new_composition(&self.context, ec, &self.comp_sink) {
            if let Ok(range) = unsafe { comp.GetRange() } {
                if unsafe { range.SetText(ec, 0, &wtext) }.is_ok() {
                    let _ = set_selection_to_end(&self.context, ec, &range);
                    let _ = unsafe { comp.EndComposition(ec) };
                    return Ok(());
                }
            }
            let _ = unsafe { comp.EndComposition(ec) };
        }

        if let Ok(insert) = self.context.cast::<ITfInsertAtSelection>() {
            match unsafe {
                insert.InsertTextAtSelection(ec, INSERT_TEXT_AT_SELECTION_FLAGS(0), &wtext)
            } {
                Ok(range) => {
                    let _ = set_selection_to_end(&self.context, ec, &range);
                }
                Err(e) => {
                    crate::debug_log::tsf_log!(
                        "edit_session",
                        "CommitComposition InsertTextAtSelection failed hr=0x{:08x}",
                        e.code().0 as u32
                    );
                    return Err(e);
                }
            }
        }
        Ok(())
    }
}

#[implement(ITfEditSession)]
pub struct CancelCompositionSession {
    context: ITfContext,
    composition: Option<ITfComposition>,
}

impl CancelCompositionSession {
    pub fn new(context: ITfContext, composition: Option<ITfComposition>) -> Self {
        Self {
            context,
            composition,
        }
    }
}

impl ITfEditSession_Impl for CancelCompositionSession_Impl {
    fn DoEditSession(&self, ec: u32) -> Result<()> {
        if let Some(ref comp) = self.composition {
            let range = unsafe { comp.GetRange()? };
            clear_display_attr(&self.context, ec, &range);
            unsafe { range.SetText(ec, 0, &[])? };
            let _ = set_selection_to_end(&self.context, ec, &range);
            unsafe { comp.EndComposition(ec)? };
        }
        Ok(())
    }
}

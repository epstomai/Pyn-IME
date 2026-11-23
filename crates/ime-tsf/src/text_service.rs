//! Core Windows TSF COM Text Input Processor (`ITfTextInputProcessorEx` & sinks).

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use ime_ipc::config::ImeConfig;
use ime_ipc::pipe::{spawn_server_detached, NamedPipeClient};
use ime_ipc::protocol::{IpcRequest, IpcResponse, DEFAULT_PIPE_NAME};
use ime_ipc::session::{InputMode, KeyAction, KeyEvent, KeyModifiers, SessionController};
use windows::Win32::Foundation::{E_INVALIDARG, E_POINTER, RECT};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::System::Variant::{
    VariantClear, VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_I4,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, VK_ADD, VK_BACK, VK_CAPITAL, VK_CONTROL, VK_DECIMAL, VK_DIVIDE, VK_DOWN,
    VK_ESCAPE, VK_LBUTTON, VK_LCONTROL, VK_LEFT, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_MENU,
    VK_MULTIPLY, VK_NEXT, VK_NUMLOCK, VK_NUMPAD0, VK_NUMPAD9, VK_OEM_1, VK_OEM_2, VK_OEM_3,
    VK_OEM_4, VK_OEM_5, VK_OEM_6, VK_OEM_7, VK_OEM_COMMA, VK_OEM_MINUS, VK_OEM_PERIOD,
    VK_OEM_PLUS, VK_PRIOR, VK_RBUTTON, VK_RCONTROL, VK_RETURN, VK_RIGHT, VK_RMENU, VK_RSHIFT,
    VK_RWIN, VK_SCROLL, VK_SHIFT, VK_SPACE, VK_SUBTRACT, VK_TAB, VK_UP,
};
use windows::Win32::UI::TextServices::{
    CLSID_TF_CategoryMgr, GUID_COMPARTMENT_KEYBOARD_INPUTMODE_CONVERSION,
    GUID_COMPARTMENT_KEYBOARD_OPENCLOSE, IEnumTfDisplayAttributeInfo, ITfCategoryMgr,
    ITfCompartmentEventSink, ITfCompartmentEventSink_Impl, ITfCompartmentMgr, ITfComposition,
    ITfCompositionSink, ITfCompositionSink_Impl, ITfContext, ITfDisplayAttributeInfo,
    ITfDisplayAttributeProvider, ITfDisplayAttributeProvider_Impl, ITfDocumentMgr, ITfEditSession,
    ITfKeyEventSink, ITfKeyEventSink_Impl, ITfKeystrokeMgr, ITfSource, ITfTextInputProcessorEx,
    ITfTextInputProcessorEx_Impl, ITfTextInputProcessor_Impl, ITfThreadMgr,
    ITfThreadMgrEventSink, ITfThreadMgrEventSink_Impl, TF_CONVERSIONMODE_NATIVE,
    TF_CONVERSIONMODE_SYMBOL, TF_ES_ASYNCDONTCARE, TF_ES_READ, TF_ES_READWRITE, TF_ES_SYNC,
    TF_INVALID_COOKIE, TF_MOD_CONTROL, TF_MOD_ON_KEYUP, TF_PRESERVEDKEY,
};
use windows_core::{implement, BOOL, Error, GUID, IUnknownImpl, Interface, Ref, Result};

use crate::candidate_ui::{CandidateViewState, CandidateWindow};
use crate::display_attr::{EnumDisplayAttributeInfo, InputDisplayAttributeInfo};
use crate::edit_session::{
    CancelCompositionSession, CommitCompositionSession, ReadContextAndCaretSession,
    UpdateCompositionSession,
};
use crate::globals::{dll_add_ref, dll_module_path, dll_release, next_session_id};
use crate::guids::active_guids;

const PRESERVED_KEY_SHIFT: TF_PRESERVEDKEY = TF_PRESERVEDKEY {
    uVKey: VK_SHIFT.0 as u32,
    uModifiers: TF_MOD_ON_KEYUP,
};

const PRESERVED_KEY_PUNCT: TF_PRESERVEDKEY = TF_PRESERVEDKEY {
    uVKey: VK_OEM_PERIOD.0 as u32,
    uModifiers: TF_MOD_CONTROL,
};

fn request_edit_session_failed(res: &Result<windows_core::HRESULT>) -> bool {
    match res {
        Ok(hr) => hr.is_err(),
        Err(_) => true,
    }
}

fn is_pure_modifier_or_toggle_vk(vk: u16) -> bool {
    vk == VK_SHIFT.0
        || vk == VK_LSHIFT.0
        || vk == VK_RSHIFT.0
        || vk == VK_CONTROL.0
        || vk == VK_LCONTROL.0
        || vk == VK_RCONTROL.0
        || vk == VK_MENU.0
        || vk == VK_LMENU.0
        || vk == VK_RMENU.0
        || vk == VK_LWIN.0
        || vk == VK_RWIN.0
        || vk == VK_CAPITAL.0
        || vk == VK_NUMLOCK.0
        || vk == VK_SCROLL.0
}

struct TextServiceState {
    tid: u32,
    session_id: u64,
    thread_mgr: Option<ITfThreadMgr>,
    thread_mgr_cookie: u32,
    conv_comp_cookie: u32,
    syncing_compartment: bool,
    composition: Rc<RefCell<Option<ITfComposition>>>,
    display_attr_atom: u32,
    config: ImeConfig,
    config_revision: u64,
    last_config_poll: Option<Instant>,
    session: SessionController,
    last_test_key: Option<(usize, KeyEvent, KeyModifiers)>,
    pipe: Option<NamedPipeClient>,
    last_spawn_attempt: Option<Instant>,
    last_context_text: String,
    candidate_wnd: Option<CandidateWindow>,
    last_caret_rect: Option<RECT>,
}

impl TextServiceState {
    fn new() -> Self {
        let config = ImeConfig::load_from(&ImeConfig::default_config_path());
        let mut session = SessionController::new();
        session.apply_config(&config, None);
        Self {
            tid: 0,
            session_id: next_session_id(),
            thread_mgr: None,
            thread_mgr_cookie: TF_INVALID_COOKIE,
            conv_comp_cookie: TF_INVALID_COOKIE,
            syncing_compartment: false,
            composition: Rc::new(RefCell::new(None)),
            display_attr_atom: 0,
            config,
            config_revision: 0,
            last_config_poll: None,
            session,
            last_test_key: None,
            pipe: None,
            last_spawn_attempt: None,
            last_context_text: String::new(),
            candidate_wnd: None,
            last_caret_rect: None,
        }
    }

    fn ensure_candidate_window(&mut self) -> Option<&mut CandidateWindow> {
        if self.candidate_wnd.is_none() {
            if let Ok(mut wnd) = CandidateWindow::new() {
                wnd.set_config(self.config.clone());
                self.candidate_wnd = Some(wnd);
            }
        }
        self.candidate_wnd.as_mut()
    }

    fn apply_new_config(&mut self, cfg: ImeConfig, rev: u64) {
        let prev = self.config.clone();
        self.session.apply_config(&cfg, Some(&prev));
        self.config = cfg.clone();
        if rev != 0 {
            self.config_revision = rev;
        }
        if let Some(ref mut wnd) = self.candidate_wnd {
            wnd.set_config(cfg);
        }
    }

    fn sync_config_from_server(&mut self, force: bool) -> bool {
        if !force {
            if let Some(t) = self.last_config_poll {
                if t.elapsed() < Duration::from_secs(2) {
                    return false;
                }
            }
        }
        self.last_config_poll = Some(Instant::now());
        if let Some(IpcResponse::Config { config, revision }) =
            self.send_ipc(&IpcRequest::GetConfig)
        {
            if revision != self.config_revision || config != self.config {
                self.apply_new_config(config, revision);
                return true;
            }
        }
        false
    }

    fn hide_candidate_window(&self) {
        if let Some(ref wnd) = self.candidate_wnd {
            wnd.hide();
        }
    }

    fn ensure_pipe(&mut self) -> Option<&mut NamedPipeClient> {
        if self.pipe.is_some() {
            return self.pipe.as_mut();
        }
        if let Ok(client) = NamedPipeClient::connect(DEFAULT_PIPE_NAME, 50) {
            self.pipe = Some(client);
            return self.pipe.as_mut();
        }

        let should_spawn = match self.last_spawn_attempt {
            None => true,
            Some(t) => t.elapsed() >= Duration::from_secs(5),
        };
        if !should_spawn {
            return None;
        }
        self.last_spawn_attempt = Some(Instant::now());

        if let Some(dll_path) = dll_module_path() {
            if let Some(dir) = dll_path.parent() {
                let server_exe = dir.join("ime-server.exe");
                if server_exe.exists() && spawn_server_detached(&server_exe).is_ok() {
                    for _ in 0..20 {
                        std::thread::sleep(Duration::from_millis(25));
                        if let Ok(client) = NamedPipeClient::connect(DEFAULT_PIPE_NAME, 50) {
                            self.pipe = Some(client);
                            return self.pipe.as_mut();
                        }
                    }
                }
            }
        }
        None
    }

    fn send_ipc(&mut self, req: &IpcRequest) -> Option<IpcResponse> {
        if let Some(client) = self.ensure_pipe() {
            if let Ok(resp) = client.request(req) {
                return Some(resp);
            }
        }
        // Reconnect once if existing pipe broke (e.g. server restarted).
        self.pipe = None;
        if let Ok(mut client) = NamedPipeClient::connect(DEFAULT_PIPE_NAME, 50) {
            if let Ok(resp) = client.request(req) {
                self.pipe = Some(client);
                return Some(resp);
            }
        }
        None
    }

    fn render_candidates(&mut self) {
        if !self.session.is_composing() {
            self.hide_candidate_window();
            return;
        }
        let mode_label = if self.session.is_english_composing() {
            "英"
        } else if self.session.chinese_punct() {
            "中"
        } else {
            "中."
        }
        .to_owned();
        let (page_idx, total_pages, slice, selected_in_page) = self.session.current_page();
        let page_text = if total_pages > 1 {
            format!("{}/{}", page_idx + 1, total_pages)
        } else {
            String::new()
        };
        let view = CandidateViewState {
            preedit: self.session.keys().to_owned(),
            mode_label,
            page_text,
            candidates: slice.to_vec(),
            selected_in_page,
            is_toast: false,
        };
        let caret = self.last_caret_rect;
        if let Some(wnd) = self.ensure_candidate_window() {
            wnd.update_and_show(view, caret);
        }
    }
}

#[implement(
    ITfTextInputProcessorEx,
    ITfKeyEventSink,
    ITfThreadMgrEventSink,
    ITfCompositionSink,
    ITfDisplayAttributeProvider,
    ITfCompartmentEventSink
)]
pub struct TextService {
    inner: RefCell<TextServiceState>,
}

impl TextService {
    pub fn new() -> Self {
        dll_add_ref();
        Self {
            inner: RefCell::new(TextServiceState::new()),
        }
    }

    fn deactivate_internal(&self) {
        let (tm_opt, tid, tm_cookie, comp_cookie, session_id) = {
            let mut st = self.inner.borrow_mut();
            st.session.on_focus_change();
            st.last_test_key = None;
            *st.composition.borrow_mut() = None;
            st.hide_candidate_window();
            st.candidate_wnd = None;
            let res = (
                st.thread_mgr.take(),
                st.tid,
                std::mem::replace(&mut st.thread_mgr_cookie, TF_INVALID_COOKIE),
                std::mem::replace(&mut st.conv_comp_cookie, TF_INVALID_COOKIE),
                st.session_id,
            );
            st.pipe = None;
            res
        };
        let _ = session_id;

        if let Some(tm) = tm_opt {
            let g = active_guids();
            if let Ok(km) = tm.cast::<ITfKeystrokeMgr>() {
                unsafe {
                    let _ = km.UnpreserveKey(&g.preserved_key_shift, &PRESERVED_KEY_SHIFT);
                    let _ = km.UnpreserveKey(&g.preserved_key_punct, &PRESERVED_KEY_PUNCT);
                    let _ = km.UnadviseKeyEventSink(tid);
                }
            }
            if comp_cookie != TF_INVALID_COOKIE {
                if let Ok(cm) = tm.cast::<ITfCompartmentMgr>() {
                    if let Ok(comp) =
                        unsafe { cm.GetCompartment(&GUID_COMPARTMENT_KEYBOARD_INPUTMODE_CONVERSION) }
                    {
                        if let Ok(src) = comp.cast::<ITfSource>() {
                            unsafe {
                                let _ = src.UnadviseSink(comp_cookie);
                            }
                        }
                    }
                }
            }
            if tm_cookie != TF_INVALID_COOKIE {
                if let Ok(src) = tm.cast::<ITfSource>() {
                    unsafe {
                        let _ = src.UnadviseSink(tm_cookie);
                    }
                }
            }
        }
    }

    fn sync_mode_compartment(&self) {
        let (tm_opt, tid, mode, chinese_punct) = {
            let mut st = self.inner.borrow_mut();
            if st.syncing_compartment {
                return;
            }
            st.syncing_compartment = true;
            (
                st.thread_mgr.clone(),
                st.tid,
                st.session.mode(),
                st.session.chinese_punct(),
            )
        };
        if let Some(tm) = tm_opt {
            set_compartment_i32(&tm, tid, &GUID_COMPARTMENT_KEYBOARD_OPENCLOSE, 1);
            let conv_val = match (mode, chinese_punct) {
                (InputMode::Chinese, true) => {
                    (TF_CONVERSIONMODE_NATIVE | TF_CONVERSIONMODE_SYMBOL) as i32
                }
                (InputMode::Chinese, false) => TF_CONVERSIONMODE_NATIVE as i32,
                (InputMode::English, _) => 0,
            };
            set_compartment_i32(
                &tm,
                tid,
                &GUID_COMPARTMENT_KEYBOARD_INPUTMODE_CONVERSION,
                conv_val,
            );
        }
        self.inner.borrow_mut().syncing_compartment = false;
    }

    fn show_toast_label(&self, pic: Option<&ITfContext>, label: &str) {
        self.inner.borrow_mut().sync_config_from_server(false);
        if !self.inner.borrow().config.show_mode_toast {
            return;
        }
        if self.inner.borrow().last_caret_rect.is_none() {
            if let Some(ctx) = pic {
                let (_, caret) = self.read_context_and_caret(ctx);
                if let Some(rc) = caret {
                    self.inner.borrow_mut().last_caret_rect = Some(rc);
                }
            }
        }
        let mut st = self.inner.borrow_mut();
        let caret = st.last_caret_rect;
        if let Some(wnd) = st.ensure_candidate_window() {
            wnd.show_mode_toast(label, caret);
        }
    }

    fn show_mode_toast_now(&self, pic: Option<&ITfContext>) {
        let label = match self.inner.borrow().session.mode() {
            InputMode::Chinese => "中",
            InputMode::English => "英",
        };
        self.show_toast_label(pic, label);
    }

    fn toggle_punctuation_mode(&self, pic: Option<&ITfContext>) {
        let (chinese_punct, is_composing) = {
            let mut st = self.inner.borrow_mut();
            let punct = st.session.toggle_chinese_punct();
            (punct, st.session.is_composing())
        };
        self.sync_mode_compartment();
        if is_composing {
            self.inner.borrow_mut().render_candidates();
        } else {
            self.show_toast_label(pic, if chinese_punct { "，。" } else { ",." });
        }
    }

    fn read_context_and_caret(&self, context: &ITfContext) -> (String, Option<RECT>) {
        let (tid, comp) = {
            let st = self.inner.borrow();
            let comp = st.composition.borrow().clone();
            (st.tid, comp)
        };
        let out = Rc::new(RefCell::new((String::new(), None)));
        let sess: ITfEditSession =
            ReadContextAndCaretSession::new(context.clone(), comp, Rc::clone(&out)).into();
        let _ = unsafe { context.RequestEditSession(tid, &sess, TF_ES_SYNC | TF_ES_READ) };
        let res = out.borrow().clone();
        res
    }

    fn run_cancel_composition(&self, context: &ITfContext, sync: bool) {
        let (tid, comp) = {
            let st = self.inner.borrow();
            let comp = st.composition.borrow_mut().take();
            (st.tid, comp)
        };
        if comp.is_none() {
            return;
        }
        let sess: ITfEditSession = CancelCompositionSession::new(context.clone(), comp).into();
        let flags = if sync {
            TF_ES_SYNC | TF_ES_READWRITE
        } else {
            TF_ES_ASYNCDONTCARE | TF_ES_READWRITE
        };
        let res = unsafe { context.RequestEditSession(tid, &sess, flags) };
        if sync && request_edit_session_failed(&res) {
            let _ = unsafe {
                context.RequestEditSession(tid, &sess, TF_ES_ASYNCDONTCARE | TF_ES_READWRITE)
            };
        }
    }
}

impl TextService_Impl {
    fn activate_impl(&self, ptim: Ref<ITfThreadMgr>, tid: u32) -> Result<()> {
        let tm = ptim.ok()?.clone();
        self.deactivate_internal();

        {
            let mut st = self.inner.borrow_mut();
            st.tid = tid;
            st.thread_mgr = Some(tm.clone());
            if let Ok(cat_mgr) =
                unsafe { CoCreateInstance::<_, ITfCategoryMgr>(&CLSID_TF_CategoryMgr, None, CLSCTX_INPROC_SERVER) }
            {
                if let Ok(atom) = unsafe { cat_mgr.RegisterGUID(&active_guids().display_attr_input) } {
                    st.display_attr_atom = atom;
                }
            }
        }

        let key_sink: ITfKeyEventSink = self.to_interface();
        let keystroke_mgr: ITfKeystrokeMgr = tm.cast()?;
        unsafe { keystroke_mgr.AdviseKeyEventSink(tid, &key_sink, true)? };

        let g = active_guids();
        let shift_desc: Vec<u16> = "Toggle Chinese/English".encode_utf16().collect();
        let punct_desc: Vec<u16> = "Toggle Chinese/English Punctuation".encode_utf16().collect();
        let _ = unsafe {
            keystroke_mgr.PreserveKey(
                tid,
                &g.preserved_key_shift,
                &PRESERVED_KEY_SHIFT,
                &shift_desc,
            )
        };
        let _ = unsafe {
            keystroke_mgr.PreserveKey(
                tid,
                &g.preserved_key_punct,
                &PRESERVED_KEY_PUNCT,
                &punct_desc,
            )
        };

        let tm_sink: ITfThreadMgrEventSink = self.to_interface();
        let source: ITfSource = tm.cast()?;
        let cookie = unsafe { source.AdviseSink(&ITfThreadMgrEventSink::IID, &tm_sink)? };
        self.inner.borrow_mut().thread_mgr_cookie = cookie;

        self.sync_mode_compartment();
        if let Ok(cm) = tm.cast::<ITfCompartmentMgr>() {
            if let Ok(comp) =
                unsafe { cm.GetCompartment(&GUID_COMPARTMENT_KEYBOARD_INPUTMODE_CONVERSION) }
            {
                if let Ok(comp_src) = comp.cast::<ITfSource>() {
                    let comp_sink: ITfCompartmentEventSink = self.to_interface();
                    if let Ok(ck) = unsafe {
                        comp_src.AdviseSink(&ITfCompartmentEventSink::IID, &comp_sink)
                    } {
                        self.inner.borrow_mut().conv_comp_cookie = ck;
                    }
                }
            }
        }

        Ok(())
    }

    fn run_commit_text(&self, context: &ITfContext, text: String) {
        self.run_commit_text_with_sync(context, text, true);
    }

    fn run_commit_text_with_sync(&self, context: &ITfContext, text: String, sync: bool) {
        let (tid, comp) = {
            let st = self.inner.borrow();
            let comp = st.composition.borrow_mut().take();
            (st.tid, comp)
        };
        let comp_sink: ITfCompositionSink = self.to_interface();
        let sess: ITfEditSession =
            CommitCompositionSession::new(context.clone(), comp, comp_sink, text).into();
        let flags = if sync {
            TF_ES_SYNC | TF_ES_READWRITE
        } else {
            TF_ES_ASYNCDONTCARE | TF_ES_READWRITE
        };
        let res = unsafe { context.RequestEditSession(tid, &sess, flags) };
        if sync && request_edit_session_failed(&res) {
            let _ = unsafe {
                context.RequestEditSession(tid, &sess, TF_ES_ASYNCDONTCARE | TF_ES_READWRITE)
            };
        }
    }

    fn run_update_composition(&self, context: &ITfContext, preedit: String) {
        self.run_update_composition_with_sync(context, preedit, true);
    }

    fn run_update_composition_with_sync(&self, context: &ITfContext, preedit: String, sync: bool) {
        let (tid, comp_slot, atom) = {
            let st = self.inner.borrow();
            (st.tid, Rc::clone(&st.composition), st.display_attr_atom)
        };
        let comp_sink: ITfCompositionSink = self.to_interface();
        let caret_out = Rc::new(RefCell::new(None));
        let sess: ITfEditSession = UpdateCompositionSession::new(
            context.clone(),
            comp_slot,
            comp_sink,
            preedit,
            atom,
            Rc::clone(&caret_out),
        )
        .into();
        let flags = if sync {
            TF_ES_SYNC | TF_ES_READWRITE
        } else {
            TF_ES_ASYNCDONTCARE | TF_ES_READWRITE
        };
        let res = unsafe { context.RequestEditSession(tid, &sess, flags) };
        if sync && request_edit_session_failed(&res) {
            let _ = unsafe {
                context.RequestEditSession(tid, &sess, TF_ES_ASYNCDONTCARE | TF_ES_READWRITE)
            };
        }
        let new_caret = *caret_out.borrow();
        if new_caret.is_some() {
            self.inner.borrow_mut().last_caret_rect = new_caret;
        }
    }

    fn apply_shift_toggle_action(&self, pic: Option<&ITfContext>, action: KeyAction) {
        self.sync_mode_compartment();
        match action {
            KeyAction::UpdateComposition { need_convert } => {
                if need_convert {
                    let (preedit, sid, ctx_text) = {
                        let st = self.inner.borrow();
                        (
                            st.session.keys().to_owned(),
                            st.session_id,
                            st.last_context_text.clone(),
                        )
                    };
                    if let Some(ctx) = pic {
                        self.run_update_composition_with_sync(ctx, preedit.clone(), false);
                    }
                    let resp = self.inner.borrow_mut().send_ipc(&IpcRequest::Convert {
                        session_id: sid,
                        context: ctx_text,
                        keys: preedit,
                        limit: 50,
                    });
                    if let Some(IpcResponse::Candidates {
                        candidates,
                        config_revision,
                        ..
                    }) = resp
                    {
                        let mut st = self.inner.borrow_mut();
                        st.session.set_candidates(candidates);
                        if config_revision != 0 && config_revision != st.config_revision {
                            st.sync_config_from_server(true);
                        }
                    }
                }
                self.inner.borrow_mut().render_candidates();
            }
            KeyAction::Commit { text, .. } => {
                if let Some(ctx) = pic {
                    self.run_commit_text_with_sync(ctx, text, false);
                }
                let mut st = self.inner.borrow_mut();
                st.hide_candidate_window();
                let sid = st.session_id;
                let _ = st.send_ipc(&IpcRequest::ResetSession { session_id: sid });
                drop(st);
                self.show_mode_toast_now(pic);
            }
            KeyAction::ConsumedNoChange => {
                self.show_mode_toast_now(pic);
            }
            _ => {}
        }
    }

    fn handle_shift_release_event(&self, pic: Option<&ITfContext>, wparam: usize) -> bool {
        let (ev, mods) = map_win_key(wparam, true);
        if matches!(ev, KeyEvent::ShiftUp) {
            let pending = self.inner.borrow().session.shift_pending_tap();
            if pending {
                let action = self.inner.borrow_mut().session.handle_key(ev, mods);
                self.apply_shift_toggle_action(pic, action);
                return true;
            }
        }
        false
    }

    fn execute_action(&self, context: &ITfContext, action: KeyAction) {
        match action {
            KeyAction::PassThrough | KeyAction::ConsumedNoChange => {}
            KeyAction::PassThroughAndUndo => {
                let mut st = self.inner.borrow_mut();
                let sid = st.session_id;
                let _ = st.send_ipc(&IpcRequest::Undo { session_id: sid });
            }
            KeyAction::UpdateComposition { need_convert } => {
                let (preedit, sid) = {
                    let st = self.inner.borrow();
                    (st.session.keys().to_owned(), st.session_id)
                };
                self.run_update_composition(context, preedit.clone());
                if need_convert {
                    let (ctx_text, caret) = self.read_context_and_caret(context);
                    let resp = {
                        let mut st = self.inner.borrow_mut();
                        st.last_context_text = ctx_text.clone();
                        if let Some(rc) = caret {
                            st.last_caret_rect = Some(rc);
                        }
                        st.send_ipc(&IpcRequest::Convert {
                            session_id: sid,
                            context: ctx_text,
                            keys: preedit,
                            limit: 50,
                        })
                    };
                    if let Some(IpcResponse::Candidates {
                        candidates,
                        config_revision,
                        ..
                    }) = resp
                    {
                        let mut st = self.inner.borrow_mut();
                        st.session.set_candidates(candidates);
                        if config_revision != 0 && config_revision != st.config_revision {
                            st.sync_config_from_server(true);
                        }
                    }
                } else {
                    if self.inner.borrow().last_caret_rect.is_none() {
                        let (ctx_text, caret) = self.read_context_and_caret(context);
                        let mut st = self.inner.borrow_mut();
                        st.last_context_text = ctx_text;
                        if let Some(rc) = caret {
                            st.last_caret_rect = Some(rc);
                        }
                    }
                    self.inner.borrow_mut().sync_config_from_server(false);
                }
                self.inner.borrow_mut().render_candidates();
            }
            KeyAction::Commit {
                text,
                learn_keys,
                learn_text,
            } => {
                self.run_commit_text(context, text);
                let mut st = self.inner.borrow_mut();
                st.hide_candidate_window();
                let sid = st.session_id;
                let ctx_text = st.last_context_text.clone();
                if let (Some(lk), Some(lt)) = (learn_keys, learn_text) {
                    let _ = st.send_ipc(&IpcRequest::Commit {
                        session_id: sid,
                        context: ctx_text,
                        keys: lk,
                        text: lt,
                    });
                } else {
                    let _ = st.send_ipc(&IpcRequest::ResetSession { session_id: sid });
                }
            }
            KeyAction::CancelComposition => {
                self.run_cancel_composition(context, true);
                let mut st = self.inner.borrow_mut();
                st.hide_candidate_window();
                let sid = st.session_id;
                let _ = st.send_ipc(&IpcRequest::ResetSession { session_id: sid });
            }
        }
    }
}

impl Drop for TextService {
    fn drop(&mut self) {
        self.deactivate_internal();
        dll_release();
    }
}

impl ITfTextInputProcessor_Impl for TextService_Impl {
    fn Activate(&self, ptim: Ref<ITfThreadMgr>, tid: u32) -> Result<()> {
        self.activate_impl(ptim, tid)
    }

    fn Deactivate(&self) -> Result<()> {
        self.deactivate_internal();
        Ok(())
    }
}

impl ITfTextInputProcessorEx_Impl for TextService_Impl {
    fn ActivateEx(&self, ptim: Ref<ITfThreadMgr>, tid: u32, _dwflags: u32) -> Result<()> {
        self.activate_impl(ptim, tid)
    }
}

impl ITfKeyEventSink_Impl for TextService_Impl {
    fn OnSetFocus(&self, _fforeground: BOOL) -> Result<()> {
        Ok(())
    }

    fn OnTestKeyDown(
        &self,
        pic: Ref<ITfContext>,
        wparam: windows::Win32::Foundation::WPARAM,
        lparam: windows::Win32::Foundation::LPARAM,
    ) -> Result<BOOL> {
        let vk = (wparam.0 & 0xFF) as u16;
        let is_repeat = ((lparam.0 as u32) & 0x4000_0000) != 0;
        let (ev, mods) = map_win_key(wparam.0, false);

        if vk == VK_OEM_PERIOD.0 && mods.ctrl && !mods.alt && !mods.shift && !mods.win {
            let mut st = self.inner.borrow_mut();
            st.last_test_key = Some((wparam.0, ev, mods));
            st.session.note_test_key_with_repeat(ev, mods, is_repeat);
            return Ok(BOOL(1));
        }

        let dropped_action = self
            .inner
            .borrow_mut()
            .session
            .consume_dropped_shift_tap(ev, mods);
        if let Some(action) = dropped_action {
            self.apply_shift_toggle_action(pic.as_ref(), action);
        }

        let (eaten, fire_undo, fallback_commit, sid) = {
            let mut st = self.inner.borrow_mut();
            let eaten = st.session.should_consume(ev, mods);
            st.last_test_key = if eaten {
                Some((wparam.0, ev, mods))
            } else {
                None
            };
            st.session.note_test_key_with_repeat(ev, mods, is_repeat);
            let fallback_commit = if !eaten
                && !mods.ctrl
                && !mods.alt
                && !mods.win
                && !is_pure_modifier_or_toggle_vk(vk)
                && st.session.is_composing()
            {
                st.session.commit_raw_for_passthrough()
            } else {
                None
            };
            let fire_undo = if !eaten && fallback_commit.is_none() {
                st.session.should_undo_on_passthrough_key(ev, mods)
            } else {
                false
            };
            (eaten, fire_undo, fallback_commit, st.session_id)
        };
        if let Some(raw) = fallback_commit {
            if let Some(ctx) = pic.as_ref() {
                self.run_commit_text_with_sync(ctx, raw, false);
            }
            let mut st = self.inner.borrow_mut();
            st.hide_candidate_window();
            let _ = st.send_ipc(&IpcRequest::ResetSession { session_id: sid });
        } else if fire_undo {
            let mut st = self.inner.borrow_mut();
            let _ = st.send_ipc(&IpcRequest::Undo { session_id: sid });
        }
        Ok(BOOL::from(eaten))
    }

    fn OnTestKeyUp(
        &self,
        pic: Ref<ITfContext>,
        wparam: windows::Win32::Foundation::WPARAM,
        _lparam: windows::Win32::Foundation::LPARAM,
    ) -> Result<BOOL> {
        let _ = self.handle_shift_release_event(pic.as_ref(), wparam.0);
        Ok(BOOL(0))
    }

    fn OnKeyDown(
        &self,
        pic: Ref<ITfContext>,
        wparam: windows::Win32::Foundation::WPARAM,
        _lparam: windows::Win32::Foundation::LPARAM,
    ) -> Result<BOOL> {
        let context = pic.ok()?;
        let vk = (wparam.0 & 0xFF) as u16;
        let (ev, mods) = {
            let mut st = self.inner.borrow_mut();
            match st.last_test_key.take() {
                Some((wp, cached_ev, cached_mods)) if wp == wparam.0 => (cached_ev, cached_mods),
                _ => map_win_key(wparam.0, false),
            }
        };
        if vk == VK_OEM_PERIOD.0 && mods.ctrl && !mods.alt && !mods.shift && !mods.win {
            self.toggle_punctuation_mode(Some(context));
            return Ok(BOOL(1));
        }
        let action = self.inner.borrow_mut().session.handle_key(ev, mods);
        let eaten = !matches!(action, KeyAction::PassThrough | KeyAction::PassThroughAndUndo);
        self.execute_action(context, action);
        Ok(BOOL::from(eaten))
    }

    fn OnKeyUp(
        &self,
        pic: Ref<ITfContext>,
        wparam: windows::Win32::Foundation::WPARAM,
        _lparam: windows::Win32::Foundation::LPARAM,
    ) -> Result<BOOL> {
        let _ = self.handle_shift_release_event(pic.as_ref(), wparam.0);
        Ok(BOOL(0))
    }

    fn OnPreservedKey(&self, pic: Ref<ITfContext>, rguid: *const GUID) -> Result<BOOL> {
        if rguid.is_null() {
            return Ok(BOOL(0));
        }
        let guid = unsafe { *rguid };
        let g = active_guids();
        if guid == g.preserved_key_shift {
            if key_down(VK_CONTROL.0)
                || key_down(VK_MENU.0)
                || key_down(VK_LWIN.0)
                || key_down(VK_RWIN.0)
                || key_down(VK_LBUTTON.0)
                || key_down(VK_RBUTTON.0)
            {
                self.inner.borrow_mut().session.note_test_key(
                    KeyEvent::Other,
                    KeyModifiers {
                        ctrl: true,
                        ..Default::default()
                    },
                );
                return Ok(BOOL(0));
            }
            let action = self.inner.borrow_mut().session.take_preserved_shift_tap();
            if let Some(act) = action {
                self.apply_shift_toggle_action(pic.as_ref(), act);
                Ok(BOOL(1))
            } else {
                Ok(BOOL(0))
            }
        } else if guid == g.preserved_key_punct {
            self.toggle_punctuation_mode(pic.as_ref());
            Ok(BOOL(1))
        } else {
            Ok(BOOL(0))
        }
    }
}

impl ITfThreadMgrEventSink_Impl for TextService_Impl {
    fn OnInitDocumentMgr(&self, _pdim: Ref<ITfDocumentMgr>) -> Result<()> {
        Ok(())
    }

    fn OnUninitDocumentMgr(&self, _pdim: Ref<ITfDocumentMgr>) -> Result<()> {
        Ok(())
    }

    fn OnSetFocus(
        &self,
        _pdimfocus: Ref<ITfDocumentMgr>,
        pdimprevfocus: Ref<ITfDocumentMgr>,
    ) -> Result<()> {
        if let Some(prev_dm) = pdimprevfocus.as_ref() {
            if let Ok(prev_ctx) = unsafe { prev_dm.GetTop() } {
                self.run_cancel_composition(&prev_ctx, false);
            }
        }
        let mut st = self.inner.borrow_mut();
        st.session.on_focus_change();
        st.last_test_key = None;
        *st.composition.borrow_mut() = None;
        st.last_caret_rect = None;
        st.hide_candidate_window();
        let sid = st.session_id;
        let _ = st.send_ipc(&IpcRequest::ResetSession { session_id: sid });
        st.sync_config_from_server(false);
        Ok(())
    }

    fn OnPushContext(&self, _pic: Ref<ITfContext>) -> Result<()> {
        Ok(())
    }

    fn OnPopContext(&self, _pic: Ref<ITfContext>) -> Result<()> {
        Ok(())
    }
}

impl ITfCompositionSink_Impl for TextService_Impl {
    fn OnCompositionTerminated(
        &self,
        _ecwrite: u32,
        _pcomposition: Ref<ITfComposition>,
    ) -> Result<()> {
        let mut st = self.inner.borrow_mut();
        *st.composition.borrow_mut() = None;
        st.session.reset_composing();
        st.hide_candidate_window();
        Ok(())
    }
}

impl ITfDisplayAttributeProvider_Impl for TextService_Impl {
    fn EnumDisplayAttributeInfo(&self) -> Result<IEnumTfDisplayAttributeInfo> {
        Ok(EnumDisplayAttributeInfo::new().into())
    }

    fn GetDisplayAttributeInfo(&self, guid: *const GUID) -> Result<ITfDisplayAttributeInfo> {
        if guid.is_null() {
            return Err(Error::from(E_POINTER));
        }
        if unsafe { *guid } == active_guids().display_attr_input {
            Ok(InputDisplayAttributeInfo.into())
        } else {
            Err(Error::from(E_INVALIDARG))
        }
    }
}

impl ITfCompartmentEventSink_Impl for TextService_Impl {
    fn OnChange(&self, rguid: *const GUID) -> Result<()> {
        if rguid.is_null() {
            return Ok(());
        }
        if self.inner.borrow().syncing_compartment {
            return Ok(());
        }
        if unsafe { *rguid } == GUID_COMPARTMENT_KEYBOARD_INPUTMODE_CONVERSION {
            let tm_opt = self.inner.borrow().thread_mgr.clone();
            if let Some(tm) = tm_opt {
                if let Some(val) =
                    get_compartment_i32(&tm, &GUID_COMPARTMENT_KEYBOARD_INPUTMODE_CONVERSION)
                {
                    let new_mode = if (val & (TF_CONVERSIONMODE_NATIVE as i32)) != 0 {
                        InputMode::Chinese
                    } else {
                        InputMode::English
                    };
                    let mut st = self.inner.borrow_mut();
                    let old_mode = st.session.mode();
                    if old_mode == InputMode::Chinese && new_mode == InputMode::Chinese {
                        st.session
                            .set_chinese_punct((val & (TF_CONVERSIONMODE_SYMBOL as i32)) != 0);
                    }
                    st.session.set_mode(new_mode);
                }
            }
        }
        Ok(())
    }
}

fn set_compartment_i32(tm: &ITfThreadMgr, tid: u32, guid: &GUID, val: i32) {
    let Ok(cm) = tm.cast::<ITfCompartmentMgr>() else {
        return;
    };
    let Ok(comp) = (unsafe { cm.GetCompartment(guid) }) else {
        return;
    };
    let mut var = VARIANT {
        Anonymous: VARIANT_0 {
            Anonymous: std::mem::ManuallyDrop::new(VARIANT_0_0 {
                vt: VT_I4,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: VARIANT_0_0_0 { lVal: val },
            }),
        },
    };
    unsafe {
        let _ = comp.SetValue(tid, &var);
        let _ = VariantClear(&mut var);
    }
}

fn get_compartment_i32(tm: &ITfThreadMgr, guid: &GUID) -> Option<i32> {
    let cm = tm.cast::<ITfCompartmentMgr>().ok()?;
    let comp = unsafe { cm.GetCompartment(guid) }.ok()?;
    let mut var = unsafe { comp.GetValue() }.ok()?;
    let res = unsafe {
        if var.Anonymous.Anonymous.vt == VT_I4 {
            Some(var.Anonymous.Anonymous.Anonymous.lVal)
        } else {
            None
        }
    };
    unsafe {
        let _ = VariantClear(&mut var);
    }
    res
}

fn key_down(vk: u16) -> bool {
    unsafe { GetKeyState(vk as i32) < 0 }
}

fn caps_toggled() -> bool {
    unsafe { (GetKeyState(VK_CAPITAL.0 as i32) & 1) != 0 }
}

fn map_win_key(wparam: usize, is_up: bool) -> (KeyEvent, KeyModifiers) {
    let vk = (wparam & 0xFF) as u16;
    let mods = KeyModifiers {
        ctrl: key_down(VK_CONTROL.0)
            || (!is_up && (vk == VK_CONTROL.0 || vk == VK_LCONTROL.0 || vk == VK_RCONTROL.0)),
        alt: key_down(VK_MENU.0)
            || (!is_up && (vk == VK_MENU.0 || vk == VK_LMENU.0 || vk == VK_RMENU.0)),
        shift: key_down(VK_SHIFT.0)
            || key_down(VK_LSHIFT.0)
            || key_down(VK_RSHIFT.0)
            || (!is_up && (vk == VK_SHIFT.0 || vk == VK_LSHIFT.0 || vk == VK_RSHIFT.0)),
        win: key_down(VK_LWIN.0)
            || key_down(VK_RWIN.0)
            || (!is_up && (vk == VK_LWIN.0 || vk == VK_RWIN.0)),
    };
    let ev = if vk == VK_SHIFT.0 || vk == VK_LSHIFT.0 || vk == VK_RSHIFT.0 {
        if is_up {
            KeyEvent::ShiftUp
        } else {
            KeyEvent::ShiftDown
        }
    } else if vk == VK_BACK.0 {
        KeyEvent::Backspace
    } else if vk == VK_SPACE.0 {
        KeyEvent::Space
    } else if vk == VK_RETURN.0 {
        KeyEvent::Enter
    } else if vk == VK_ESCAPE.0 {
        KeyEvent::Escape
    } else if vk == VK_LEFT.0 {
        KeyEvent::Left
    } else if vk == VK_RIGHT.0 {
        KeyEvent::Right
    } else if vk == VK_UP.0 {
        KeyEvent::Up
    } else if vk == VK_DOWN.0 {
        KeyEvent::Down
    } else if vk == VK_PRIOR.0 {
        KeyEvent::PageUp
    } else if vk == VK_NEXT.0 {
        KeyEvent::PageDown
    } else if vk == VK_TAB.0 {
        KeyEvent::Tab
    } else if (b'A' as u16..=b'Z' as u16).contains(&vk) {
        if !caps_toggled() {
            if !mods.shift {
                KeyEvent::Char((vk as u8 + 32) as char)
            } else if !mods.ctrl && !mods.alt && !mods.win {
                KeyEvent::UpperChar(vk as u8 as char)
            } else {
                KeyEvent::Other
            }
        } else if !mods.ctrl && !mods.alt && !mods.win {
            KeyEvent::RawChar(if mods.shift {
                (vk as u8 + 32) as char
            } else {
                vk as u8 as char
            })
        } else {
            KeyEvent::Other
        }
    } else if (b'0' as u16..=b'9' as u16).contains(&vk) {
        if !mods.shift {
            KeyEvent::Digit((vk as u8) - b'0')
        } else {
            match vk as u8 {
                b'1' => KeyEvent::Char('!'),
                b'2' => KeyEvent::RawChar('@'),
                b'3' => KeyEvent::RawChar('#'),
                b'4' => KeyEvent::RawChar('$'),
                b'5' => KeyEvent::RawChar('%'),
                b'6' => KeyEvent::Char('^'),
                b'7' => KeyEvent::RawChar('&'),
                b'8' => KeyEvent::RawChar('*'),
                b'9' => KeyEvent::Char('('),
                b'0' => KeyEvent::Char(')'),
                _ => KeyEvent::Other,
            }
        }
    } else if vk == VK_OEM_COMMA.0 {
        KeyEvent::Char(if mods.shift { '<' } else { ',' })
    } else if vk == VK_OEM_PERIOD.0 {
        KeyEvent::Char(if mods.shift { '>' } else { '.' })
    } else if vk == VK_OEM_2.0 {
        KeyEvent::Char(if mods.shift { '?' } else { '/' })
    } else if vk == VK_OEM_1.0 {
        KeyEvent::Char(if mods.shift { ':' } else { ';' })
    } else if vk == VK_OEM_7.0 {
        KeyEvent::Char(if mods.shift { '"' } else { '\'' })
    } else if vk == VK_OEM_MINUS.0 {
        KeyEvent::Char(if mods.shift { '_' } else { '-' })
    } else if vk == VK_OEM_PLUS.0 {
        if mods.shift {
            KeyEvent::RawChar('+')
        } else {
            KeyEvent::Char('=')
        }
    } else if vk == VK_OEM_5.0 {
        if !mods.shift {
            KeyEvent::Char('\\')
        } else {
            KeyEvent::RawChar('|')
        }
    } else if vk == VK_OEM_3.0 {
        KeyEvent::RawChar(if mods.shift { '~' } else { '`' })
    } else if vk == VK_OEM_4.0 {
        KeyEvent::RawChar(if mods.shift { '{' } else { '[' })
    } else if vk == VK_OEM_6.0 {
        KeyEvent::RawChar(if mods.shift { '}' } else { ']' })
    } else if (VK_NUMPAD0.0..=VK_NUMPAD9.0).contains(&vk) {
        KeyEvent::RawChar((b'0' + (vk - VK_NUMPAD0.0) as u8) as char)
    } else if vk == VK_MULTIPLY.0 {
        KeyEvent::RawChar('*')
    } else if vk == VK_ADD.0 {
        KeyEvent::RawChar('+')
    } else if vk == VK_SUBTRACT.0 {
        KeyEvent::RawChar('-')
    } else if vk == VK_DECIMAL.0 {
        KeyEvent::RawChar('.')
    } else if vk == VK_DIVIDE.0 {
        KeyEvent::RawChar('/')
    } else {
        KeyEvent::Other
    };
    (ev, mods)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::globals::dll_ref_count;
    use windows::Win32::UI::TextServices::{ITfKeyEventSink, ITfTextInputProcessorEx};

    #[test]
    fn preserved_keys_toggle_and_dedup_callbacks() {
        let tip: ITfTextInputProcessorEx = TextService::new().into();
        let imp: &TextService = unsafe { windows_core::AsImpl::as_impl(&tip) };
        let key_sink: ITfKeyEventSink = tip.cast().expect("cast ITfKeyEventSink");

        let wp_shift = windows::Win32::Foundation::WPARAM(VK_SHIFT.0 as usize);
        let lp0 = windows::Win32::Foundation::LPARAM(0);

        let g = active_guids();
        unsafe {
            // 1. OnPreservedKey(VK_SHIFT) toggles Chinese -> English even without prior ShiftDown.
            assert_eq!(imp.inner.borrow().session.mode(), InputMode::Chinese);
            assert!(key_sink.OnPreservedKey(None, &g.preserved_key_shift).unwrap().as_bool());
            assert_eq!(imp.inner.borrow().session.mode(), InputMode::English);

            // Immediate duplicate OnTestKeyUp / OnKeyUp must NOT double-toggle.
            let _ = key_sink.OnTestKeyUp(None, wp_shift, lp0);
            let _ = key_sink.OnKeyUp(None, wp_shift, lp0);
            assert_eq!(imp.inner.borrow().session.mode(), InputMode::English);

            // Second OnPreservedKey toggles English -> Chinese.
            assert!(key_sink.OnPreservedKey(None, &g.preserved_key_shift).unwrap().as_bool());
            assert_eq!(imp.inner.borrow().session.mode(), InputMode::Chinese);

            // 2. Shift combo (ShiftDown + Shift+'N') rejects preserved Shift release.
            let _ = key_sink.OnTestKeyDown(None, wp_shift, lp0);
            imp.inner.borrow_mut().session.note_test_key_with_repeat(
                KeyEvent::UpperChar('N'),
                KeyModifiers {
                    shift: true,
                    ..Default::default()
                },
                false,
            );
            assert!(!key_sink.OnPreservedKey(None, &g.preserved_key_shift).unwrap().as_bool());
            assert_eq!(imp.inner.borrow().session.mode(), InputMode::Chinese);

            // 3. OnPreservedKey(Ctrl+.) toggles Chinese <-> English punctuation cleanly.
            assert!(imp.inner.borrow().session.chinese_punct());
            assert!(key_sink.OnPreservedKey(None, &g.preserved_key_punct).unwrap().as_bool());
            assert!(!imp.inner.borrow().session.chinese_punct());
            assert!(key_sink.OnPreservedKey(None, &g.preserved_key_punct).unwrap().as_bool());
            assert!(imp.inner.borrow().session.chinese_punct());

            // 4. Fallback: OnTestKeyDown(VK_SHIFT) -> OnKeyUp(VK_SHIFT) without OnPreservedKey/OnTestKeyUp.
            let _ = key_sink.OnTestKeyDown(None, wp_shift, lp0);
            let _ = key_sink.OnKeyUp(None, wp_shift, lp0);
            assert_eq!(imp.inner.borrow().session.mode(), InputMode::English);
            // Repeat OnKeyUp must not double-toggle.
            let _ = key_sink.OnKeyUp(None, wp_shift, lp0);
            assert_eq!(imp.inner.borrow().session.mode(), InputMode::English);
        }
        drop(key_sink);
        drop(tip);
        // Release the process-global DLL ref check cleanly only up to live objects.
        let _ = dll_ref_count();
    }
}

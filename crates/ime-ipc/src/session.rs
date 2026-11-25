//! Pure-Rust IME composing and key-dispatch state machine shared by the TSF shell and tests.

use crate::config::ImeConfig;
use crate::english::suggest_english_candidates;

const MAX_KEYS_LEN: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    Chinese,
    English,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KeyModifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyEvent {
    Char(char),
    UpperChar(char),
    RawChar(char),
    Backspace,
    Space,
    Enter,
    Escape,
    Digit(u8),
    Left,
    Right,
    Up,
    Down,
    PageUp,
    PageDown,
    Tab,
    ShiftDown,
    ShiftUp,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyAction {
    PassThrough,
    PassThroughAndUndo,
    ConsumedNoChange,
    UpdateComposition { need_convert: bool },
    Commit {
        text: String,
        learn_keys: Option<String>,
        learn_text: Option<String>,
    },
    CancelComposition,
}

#[derive(Debug, Clone)]
pub struct SessionController {
    mode: InputMode,
    chinese_punct: bool,
    shift_toggle_enabled: bool,
    initial_upper_english: bool,
    page_size: usize,
    keys: String,
    candidates: Vec<String>,
    selected: usize,
    just_committed_chinese: bool,
    shift_pending_tap: bool,
    shift_combo_used: bool,
    open_double_quote: bool,
    open_single_quote: bool,
}

impl Default for SessionController {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionController {
    pub fn new() -> Self {
        Self {
            mode: InputMode::Chinese,
            chinese_punct: true,
            shift_toggle_enabled: true,
            initial_upper_english: true,
            page_size: 5,
            keys: String::new(),
            candidates: Vec::new(),
            selected: 0,
            just_committed_chinese: false,
            shift_pending_tap: false,
            shift_combo_used: false,
            open_double_quote: true,
            open_single_quote: true,
        }
    }

    pub fn set_page_size(&mut self, size: usize) {
        self.page_size = size.clamp(3, 9);
    }

    pub fn set_shift_toggle_enabled(&mut self, enabled: bool) {
        self.shift_toggle_enabled = enabled;
        if !enabled {
            self.shift_pending_tap = false;
            self.shift_combo_used = false;
        }
    }

    pub fn shift_toggle_enabled(&self) -> bool {
        self.shift_toggle_enabled
    }

    pub fn set_initial_upper_english(&mut self, enabled: bool) {
        self.initial_upper_english = enabled;
    }

    pub fn initial_upper_english(&self) -> bool {
        self.initial_upper_english
    }

    pub fn apply_config(&mut self, cfg: &ImeConfig, prev: Option<&ImeConfig>) {
        self.set_page_size(cfg.page_size);
        self.set_shift_toggle_enabled(cfg.shift_toggle);
        self.set_initial_upper_english(cfg.initial_upper_english);
        if prev.is_none()
            || prev.is_some_and(|p| p.default_chinese_punct != cfg.default_chinese_punct)
        {
            self.set_chinese_punct(cfg.default_chinese_punct);
        }
    }

    pub fn mode(&self) -> InputMode {
        self.mode
    }

    pub fn set_mode(&mut self, mode: InputMode) {
        self.mode = mode;
        self.shift_pending_tap = false;
        self.shift_combo_used = false;
        if self.is_composing() && mode == InputMode::English {
            self.refresh_english_candidates();
        }
    }

    pub fn chinese_punct(&self) -> bool {
        self.chinese_punct
    }

    pub fn set_chinese_punct(&mut self, enabled: bool) {
        self.chinese_punct = enabled;
    }

    pub fn toggle_chinese_punct(&mut self) -> bool {
        self.chinese_punct = !self.chinese_punct;
        self.chinese_punct
    }

    pub fn is_composing(&self) -> bool {
        !self.keys.is_empty()
    }

    pub fn is_english_composing(&self) -> bool {
        self.is_composing()
            && (self.mode == InputMode::English
                || self.keys.bytes().any(|b| b.is_ascii_uppercase()))
    }

    pub fn keys(&self) -> &str {
        &self.keys
    }

    pub fn page_size(&self) -> usize {
        self.page_size
    }

    pub fn shift_pending_tap(&self) -> bool {
        self.shift_pending_tap
    }

    /// Recovers a clean solo Shift tap when the host app (e.g. Chromium/Electron)
    /// drops `OnTestKeyUp(VK_SHIFT)` before dispatching the next keydown.
    pub fn consume_dropped_shift_tap(
        &mut self,
        event: KeyEvent,
        mods: KeyModifiers,
    ) -> Option<KeyAction> {
        if mods.ctrl || mods.alt || mods.win {
            self.shift_pending_tap = false;
            return None;
        }
        if self.shift_pending_tap && !matches!(event, KeyEvent::ShiftDown | KeyEvent::ShiftUp) {
            if !mods.shift {
                return Some(self.handle_key(KeyEvent::ShiftUp, KeyModifiers::default()));
            }
            self.shift_pending_tap = false;
        }
        None
    }

    pub fn should_undo_on_passthrough_key(&mut self, event: KeyEvent, mods: KeyModifiers) -> bool {
        if mods.ctrl || mods.alt || mods.win {
            self.just_committed_chinese = false;
            return false;
        }
        if matches!(event, KeyEvent::Backspace) && !self.is_composing() && self.just_committed_chinese
        {
            self.just_committed_chinese = false;
            return true;
        }
        if !matches!(event, KeyEvent::ShiftDown | KeyEvent::ShiftUp) {
            self.just_committed_chinese = false;
        }
        false
    }

    /// Updates pending Shift-tap tracking during `OnTestKeyDown` so combos like `Shift+A`
    /// or key-repeat `Shift` holds never toggle Chinese/English.
    pub fn note_test_key(&mut self, event: KeyEvent, mods: KeyModifiers) {
        self.note_test_key_with_repeat(event, mods, false);
    }

    pub fn note_test_key_with_repeat(&mut self, event: KeyEvent, mods: KeyModifiers, is_repeat: bool) {
        if mods.ctrl || mods.alt || mods.win {
            self.shift_pending_tap = false;
            self.shift_combo_used = mods.shift;
            return;
        }
        match event {
            KeyEvent::ShiftDown => {
                if !is_repeat {
                    self.shift_pending_tap = true;
                    self.shift_combo_used = false;
                }
            }
            KeyEvent::ShiftUp => {}
            _ => {
                self.shift_pending_tap = false;
                self.shift_combo_used = mods.shift;
            }
        }
    }

    /// Handles a TSF `OnPreservedKey(VK_SHIFT, TF_MOD_ON_KEYUP)` release callback.
    /// Fires even if the host app skipped `OnTestKeyDown(VK_SHIFT)`, while rejecting any
    /// Shift release that participated in a key combo (`Shift+A`, `Shift+(`, `Ctrl+Shift`, etc.).
    pub fn take_preserved_shift_tap(&mut self) -> Option<KeyAction> {
        let combo = self.shift_combo_used;
        self.shift_combo_used = false;
        if combo || !self.shift_toggle_enabled {
            self.shift_pending_tap = false;
            return None;
        }
        self.shift_pending_tap = true;
        Some(self.handle_key(KeyEvent::ShiftUp, KeyModifiers::default()))
    }

    /// Non-mutating predicate for `ITfKeyEventSink::OnTestKeyDown`.
    pub fn should_consume(&self, event: KeyEvent, mods: KeyModifiers) -> bool {
        if mods.ctrl || mods.alt || mods.win {
            return false;
        }
        if matches!(event, KeyEvent::ShiftDown | KeyEvent::ShiftUp) {
            return false;
        }
        if self.is_composing() {
            return match event {
                KeyEvent::Char('a'..='z') if !mods.shift => true,
                KeyEvent::UpperChar('A'..='Z') => true,
                KeyEvent::RawChar(_) => true,
                KeyEvent::Char('\'' | '-' | '=' | '/') if !mods.shift => true,
                KeyEvent::Char(ch) if is_mapped_punct(ch) => true,
                KeyEvent::Backspace
                | KeyEvent::Space
                | KeyEvent::Enter
                | KeyEvent::Escape
                | KeyEvent::Left
                | KeyEvent::Right
                | KeyEvent::Up
                | KeyEvent::Down
                | KeyEvent::PageUp
                | KeyEvent::PageDown
                | KeyEvent::Tab => true,
                KeyEvent::Digit(_) => true,
                _ => false,
            };
        }
        if self.mode == InputMode::Chinese {
            match event {
                KeyEvent::Char('a'..='z') if !mods.shift => true,
                KeyEvent::UpperChar('A'..='Z') if self.initial_upper_english => true,
                KeyEvent::Char(ch) if self.chinese_punct && is_mapped_punct(ch) => true,
                _ => false,
            }
        } else {
            false
        }
    }

    pub fn handle_key(&mut self, event: KeyEvent, mods: KeyModifiers) -> KeyAction {
        if mods.ctrl || mods.alt || mods.win {
            self.shift_pending_tap = false;
            self.shift_combo_used = mods.shift;
            self.just_committed_chinese = false;
            return KeyAction::PassThrough;
        }

        match event {
            KeyEvent::ShiftDown => {
                self.shift_pending_tap = true;
                self.shift_combo_used = false;
                return KeyAction::PassThrough;
            }
            KeyEvent::ShiftUp => {
                self.shift_combo_used = false;
                if !self.shift_pending_tap || !self.shift_toggle_enabled {
                    self.shift_pending_tap = false;
                    return KeyAction::PassThrough;
                }
                self.shift_pending_tap = false;
                if self.is_composing() {
                    if self.mode == InputMode::Chinese {
                        let raw = self.take_composing_reset(false);
                        self.mode = InputMode::English;
                        return KeyAction::Commit {
                            text: raw,
                            learn_keys: None,
                            learn_text: None,
                        };
                    } else {
                        self.mode = InputMode::Chinese;
                        self.keys.make_ascii_lowercase();
                        self.candidates.clear();
                        self.selected = 0;
                        return KeyAction::UpdateComposition { need_convert: true };
                    }
                }
                self.mode = match self.mode {
                    InputMode::Chinese => InputMode::English,
                    InputMode::English => InputMode::Chinese,
                };
                return KeyAction::ConsumedNoChange;
            }
            _ => {
                self.shift_pending_tap = false;
                self.shift_combo_used = mods.shift;
            }
        }

        if !self.is_composing() && matches!(event, KeyEvent::Backspace) && self.just_committed_chinese
        {
            self.just_committed_chinese = false;
            return KeyAction::PassThroughAndUndo;
        }
        if !matches!(event, KeyEvent::ShiftDown | KeyEvent::ShiftUp) {
            self.just_committed_chinese = false;
        }

        if !self.is_composing() {
            if self.mode == InputMode::English {
                return KeyAction::PassThrough;
            }
            return match event {
                KeyEvent::Char(ch @ 'a'..='z') if !mods.shift => {
                    self.keys.push(ch);
                    self.selected = 0;
                    KeyAction::UpdateComposition { need_convert: true }
                }
                KeyEvent::UpperChar(ch @ 'A'..='Z') if self.initial_upper_english => {
                    self.keys.push(ch);
                    self.refresh_english_candidates();
                    KeyAction::UpdateComposition {
                        need_convert: false,
                    }
                }
                KeyEvent::Char(ch) if self.chinese_punct && is_mapped_punct(ch) => {
                    if let Some(mapped) = self.map_punct(ch) {
                        KeyAction::Commit {
                            text: mapped.to_owned(),
                            learn_keys: None,
                            learn_text: None,
                        }
                    } else {
                        KeyAction::PassThrough
                    }
                }
                _ => KeyAction::PassThrough,
            };
        }

        // Composing state
        match event {
            KeyEvent::Char(ch @ 'a'..='z') if !mods.shift => {
                if self.keys.len() < MAX_KEYS_LEN {
                    self.keys.push(ch);
                    self.selected = 0;
                    if self.is_english_composing() {
                        self.refresh_english_candidates();
                        KeyAction::UpdateComposition {
                            need_convert: false,
                        }
                    } else {
                        KeyAction::UpdateComposition { need_convert: true }
                    }
                } else {
                    KeyAction::ConsumedNoChange
                }
            }
            KeyEvent::UpperChar(ch) => {
                if self.keys.len() < MAX_KEYS_LEN {
                    self.keys.push(ch);
                    self.refresh_english_candidates();
                    KeyAction::UpdateComposition {
                        need_convert: false,
                    }
                } else {
                    KeyAction::ConsumedNoChange
                }
            }
            KeyEvent::RawChar(ch) => {
                let mut text = self.take_composing_reset(false);
                text.push(ch);
                KeyAction::Commit {
                    text,
                    learn_keys: None,
                    learn_text: None,
                }
            }
            KeyEvent::Digit(0) => {
                let mut text = self.take_composing_reset(false);
                text.push('0');
                KeyAction::Commit {
                    text,
                    learn_keys: None,
                    learn_text: None,
                }
            }
            KeyEvent::Char('/') if !mods.shift => {
                let mut text = self.take_composing_reset(false);
                text.push('/');
                KeyAction::Commit {
                    text,
                    learn_keys: None,
                    learn_text: None,
                }
            }
            KeyEvent::Char('\'') if !mods.shift => {
                if self.is_english_composing() {
                    if self.keys.len() < MAX_KEYS_LEN {
                        self.keys.push('\'');
                        self.refresh_english_candidates();
                        KeyAction::UpdateComposition {
                            need_convert: false,
                        }
                    } else {
                        KeyAction::ConsumedNoChange
                    }
                } else if self.keys.len() < MAX_KEYS_LEN && !self.keys.ends_with('\'') {
                    self.keys.push('\'');
                    self.selected = 0;
                    KeyAction::UpdateComposition { need_convert: true }
                } else {
                    KeyAction::ConsumedNoChange
                }
            }
            KeyEvent::Backspace => {
                self.keys.pop();
                if self.keys.is_empty() {
                    self.candidates.clear();
                    self.selected = 0;
                    KeyAction::CancelComposition
                } else if self.is_english_composing() {
                    self.refresh_english_candidates();
                    KeyAction::UpdateComposition {
                        need_convert: false,
                    }
                } else {
                    self.selected = 0;
                    KeyAction::UpdateComposition { need_convert: true }
                }
            }
            KeyEvent::Escape => {
                self.reset_composing();
                KeyAction::CancelComposition
            }
            KeyEvent::Enter => {
                let raw = self.take_composing_reset(false);
                KeyAction::Commit {
                    text: raw,
                    learn_keys: None,
                    learn_text: None,
                }
            }
            KeyEvent::Space => {
                let picked = self.candidates.get(self.selected).cloned();
                let is_chinese_pick = picked.as_ref().is_some_and(|c| !c.is_ascii());
                let raw = self.take_composing_reset(is_chinese_pick);
                match picked {
                    Some(cand) => {
                        let (learn_keys, learn_text) = if cand.is_ascii() {
                            (None, None)
                        } else {
                            (Some(raw), Some(cand.clone()))
                        };
                        KeyAction::Commit {
                            text: cand,
                            learn_keys,
                            learn_text,
                        }
                    }
                    None => KeyAction::Commit {
                        text: raw,
                        learn_keys: None,
                        learn_text: None,
                    },
                }
            }
            KeyEvent::Digit(d) if (1..=9).contains(&d) => {
                let d_usize = d as usize;
                if d_usize > self.page_size {
                    return KeyAction::ConsumedNoChange;
                }
                let page_start = (self.selected / self.page_size) * self.page_size;
                let idx = page_start + (d_usize - 1);
                if let Some(cand) = self.candidates.get(idx).cloned() {
                    let is_chinese_pick = !cand.is_ascii();
                    let raw = self.take_composing_reset(is_chinese_pick);
                    let (learn_keys, learn_text) = if cand.is_ascii() {
                        (None, None)
                    } else {
                        (Some(raw), Some(cand.clone()))
                    };
                    KeyAction::Commit {
                        text: cand,
                        learn_keys,
                        learn_text,
                    }
                } else {
                    KeyAction::ConsumedNoChange
                }
            }
            KeyEvent::Up | KeyEvent::Left => {
                if !self.candidates.is_empty() {
                    self.selected = if self.selected == 0 {
                        self.candidates.len() - 1
                    } else {
                        self.selected - 1
                    };
                }
                KeyAction::UpdateComposition {
                    need_convert: false,
                }
            }
            KeyEvent::Down | KeyEvent::Right | KeyEvent::Tab => {
                if !self.candidates.is_empty() {
                    self.selected = (self.selected + 1) % self.candidates.len();
                }
                KeyAction::UpdateComposition {
                    need_convert: false,
                }
            }
            KeyEvent::PageUp | KeyEvent::Char('-') => {
                if !self.candidates.is_empty() {
                    let page_idx = self.selected / self.page_size;
                    if page_idx > 0 {
                        self.selected = (page_idx - 1) * self.page_size;
                    } else {
                        self.selected = 0;
                    }
                }
                KeyAction::UpdateComposition {
                    need_convert: false,
                }
            }
            KeyEvent::PageDown | KeyEvent::Char('=') => {
                if !self.candidates.is_empty() {
                    let page_idx = self.selected / self.page_size;
                    let total_pages = self.candidates.len().div_ceil(self.page_size);
                    if page_idx + 1 < total_pages {
                        self.selected = (page_idx + 1) * self.page_size;
                    }
                }
                KeyAction::UpdateComposition {
                    need_convert: false,
                }
            }
            KeyEvent::Char(ch) if is_mapped_punct(ch) => {
                if self.is_english_composing() {
                    let picked = self
                        .candidates
                        .get(self.selected)
                        .cloned()
                        .unwrap_or_else(|| self.keys.clone());
                    let _ = self.take_composing_reset(false);
                    return KeyAction::Commit {
                        text: format!("{picked}{ch}"),
                        learn_keys: None,
                        learn_text: None,
                    };
                }
                let punct_owned = if self.chinese_punct {
                    self.map_punct(ch).unwrap_or("").to_owned()
                } else {
                    ch.to_string()
                };
                let picked = self.candidates.get(self.selected).cloned();
                let raw = self.take_composing_reset(false);
                match picked {
                    Some(cand) => {
                        let (learn_keys, learn_text) = if cand.is_ascii() {
                            (None, None)
                        } else {
                            (Some(raw), Some(cand.clone()))
                        };
                        KeyAction::Commit {
                            text: format!("{cand}{punct_owned}"),
                            learn_keys,
                            learn_text,
                        }
                    }
                    None => KeyAction::Commit {
                        text: format!("{raw}{punct_owned}"),
                        learn_keys: None,
                        learn_text: None,
                    },
                }
            }
            _ => KeyAction::PassThrough,
        }
    }

    pub fn set_candidates(&mut self, cands: Vec<String>) {
        self.candidates = cands;
        if self.selected >= self.candidates.len() {
            self.selected = 0;
        }
    }

    pub fn current_page(&self) -> (usize, usize, &[String], usize) {
        if self.candidates.is_empty() {
            return (0, 0, &[], 0);
        }
        let total_pages = self.candidates.len().div_ceil(self.page_size);
        let page_idx = (self.selected / self.page_size).min(total_pages - 1);
        let start = page_idx * self.page_size;
        let end = (start + self.page_size).min(self.candidates.len());
        (
            page_idx,
            total_pages,
            &self.candidates[start..end],
            self.selected - start,
        )
    }

    pub fn reset_composing(&mut self) {
        self.keys.clear();
        self.candidates.clear();
        self.selected = 0;
    }

    pub fn on_focus_change(&mut self) {
        self.reset_composing();
        self.just_committed_chinese = false;
        self.shift_pending_tap = false;
        self.shift_combo_used = false;
    }

    pub fn commit_raw_for_passthrough(&mut self) -> Option<String> {
        if self.is_composing() {
            Some(self.take_composing_reset(false))
        } else {
            None
        }
    }

    fn refresh_english_candidates(&mut self) {
        self.candidates = suggest_english_candidates(&self.keys, 15);
        self.selected = 0;
    }

    fn take_composing_reset(&mut self, committed_chinese: bool) -> String {
        let raw = std::mem::take(&mut self.keys);
        self.candidates.clear();
        self.selected = 0;
        self.just_committed_chinese = committed_chinese;
        raw
    }

    fn map_punct(&mut self, ch: char) -> Option<&'static str> {
        match ch {
            ',' => Some("，"),
            '.' => Some("。"),
            '?' => Some("？"),
            '!' => Some("！"),
            ';' => Some("；"),
            ':' => Some("："),
            '(' => Some("（"),
            ')' => Some("）"),
            '<' => Some("《"),
            '>' => Some("》"),
            '\\' => Some("、"),
            '^' => Some("……"),
            '_' => Some("——"),
            '"' => {
                let out = if self.open_double_quote { "“" } else { "”" };
                self.open_double_quote = !self.open_double_quote;
                Some(out)
            }
            '\'' => {
                let out = if self.open_single_quote { "‘" } else { "’" };
                self.open_single_quote = !self.open_single_quote;
                Some(out)
            }
            _ => None,
        }
    }
}

#[inline]
fn is_mapped_punct(ch: char) -> bool {
    matches!(
        ch,
        ',' | '.' | '?' | '!' | ';' | ':' | '(' | ')' | '<' | '>' | '\\' | '"' | '\'' | '^' | '_'
    )
}

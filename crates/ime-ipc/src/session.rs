//! Pure-Rust IME composing and key-dispatch state machine shared by the TSF shell and tests.

use crate::config::ImeConfig;
use crate::english::suggest_english_candidates;
use crate::protocol::{WireCandidate, WireCandidateKind};

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
struct LockedSegment {
    text: String,
    raw_keys: String,
}

#[derive(Debug, Clone)]
pub struct SessionController {
    mode: InputMode,
    chinese_punct: bool,
    shift_toggle_enabled: bool,
    initial_upper_english: bool,
    page_size: usize,
    locked_segments: Vec<LockedSegment>,
    keys: String,
    candidates: Vec<String>,
    choices: Vec<WireCandidate>,
    segmented_preedit: String,
    first_char_offset: Option<usize>,
    first_syllable_len: usize,
    single_char_mode: bool,
    selected_before_tab: Option<usize>,
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
            locked_segments: Vec::new(),
            keys: String::new(),
            candidates: Vec::new(),
            choices: Vec::new(),
            segmented_preedit: String::new(),
            first_char_offset: None,
            first_syllable_len: 0,
            single_char_mode: false,
            selected_before_tab: None,
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
        !self.keys.is_empty() || !self.locked_segments.is_empty()
    }

    pub fn is_english_composing(&self) -> bool {
        self.locked_segments.is_empty()
            && self.is_composing()
            && (self.mode == InputMode::English
                || self.keys.bytes().any(|b| b.is_ascii_uppercase()))
    }

    pub fn has_locked(&self) -> bool {
        !self.locked_segments.is_empty()
    }

    pub fn locked_text(&self) -> String {
        let mut out = String::new();
        for seg in &self.locked_segments {
            out.push_str(&seg.text);
        }
        out
    }

    fn locked_raw_keys(&self) -> String {
        let mut out = String::new();
        for seg in &self.locked_segments {
            out.push_str(&seg.raw_keys);
        }
        out
    }

    fn locked_raw_len(&self) -> usize {
        self.locked_segments
            .iter()
            .map(|seg| seg.raw_keys.len())
            .sum()
    }

    pub fn inline_preedit(&self) -> String {
        let mut out = self.locked_text();
        out.push_str(&self.keys);
        out
    }

    pub fn panel_preedit(&self) -> String {
        if self.is_english_composing() {
            return self.keys.clone();
        }
        let rem = if !self.segmented_preedit.is_empty() {
            self.segmented_preedit.as_str()
        } else {
            self.keys.as_str()
        };
        if self.locked_segments.is_empty() {
            rem.to_owned()
        } else if rem.is_empty() {
            self.locked_text()
        } else {
            format!("{} {}", self.locked_text(), rem)
        }
    }

    pub fn is_single_char_mode(&self) -> bool {
        self.single_char_mode && self.active_view_start() > 0
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
                        self.clear_candidates();
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
                if self.locked_raw_len() + self.keys.len() < MAX_KEYS_LEN {
                    self.keys.push(ch);
                    self.exit_single_char_mode();
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
                if !self.locked_segments.is_empty() {
                    let mut text = self.take_composing_reset(false);
                    text.push(ch);
                    return KeyAction::Commit {
                        text,
                        learn_keys: None,
                        learn_text: None,
                    };
                }
                if self.keys.len() < MAX_KEYS_LEN {
                    self.keys.push(ch);
                    self.exit_single_char_mode();
                    self.refresh_english_candidates();
                    KeyAction::UpdateComposition {
                        need_convert: false,
                    }
                } else {
                    KeyAction::ConsumedNoChange
                }
            }
            KeyEvent::RawChar(ch @ ('[' | ']')) if !self.is_english_composing() => {
                if let Some(choice) = self.active_choice_at(self.selected) {
                    if !choice.text.is_ascii() {
                        let picked_char = if ch == '[' {
                            choice.text.chars().next()
                        } else {
                            choice.text.chars().last()
                        };
                        if let Some(c) = picked_char {
                            let consume = if ch == '[' {
                                if self.first_syllable_len > 0 {
                                    self.first_syllable_len.min(choice.consumed_len)
                                } else {
                                    choice.consumed_len
                                }
                            } else {
                                choice.consumed_len
                            };
                            let synthetic = WireCandidate {
                                text: c.to_string(),
                                consumed_len: consume,
                                kind: WireCandidateKind::PrefixChar,
                            };
                            return self.apply_choice(synthetic);
                        }
                    }
                }
                let mut text = self.take_composing_reset(false);
                text.push(ch);
                KeyAction::Commit {
                    text,
                    learn_keys: None,
                    learn_text: None,
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
                } else if self.locked_raw_len() + self.keys.len() < MAX_KEYS_LEN
                    && !self.keys.ends_with('\'')
                    && !self.keys.is_empty()
                {
                    self.keys.push('\'');
                    self.exit_single_char_mode();
                    self.selected = 0;
                    KeyAction::UpdateComposition { need_convert: true }
                } else {
                    KeyAction::ConsumedNoChange
                }
            }
            KeyEvent::Backspace => {
                self.exit_single_char_mode();
                if let Some(last) = self.locked_segments.pop() {
                    self.keys = format!("{}{}", last.raw_keys, self.keys);
                    self.clear_candidates();
                    return KeyAction::UpdateComposition { need_convert: true };
                }
                self.keys.pop();
                if self.keys.is_empty() {
                    self.clear_candidates();
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
                if let Some(choice) = self.active_choice_at(self.selected) {
                    self.apply_choice(choice)
                } else {
                    let raw = self.take_composing_reset(false);
                    KeyAction::Commit {
                        text: raw,
                        learn_keys: None,
                        learn_text: None,
                    }
                }
            }
            KeyEvent::Digit(d) if (1..=9).contains(&d) => {
                let d_usize = d as usize;
                let view_len = self.active_view_len();
                let page_start = (self.selected / self.page_size) * self.page_size;
                let idx = page_start + (d_usize - 1);
                let ascii_only =
                    self.locked_segments.is_empty() && self.candidates.iter().all(|c| c.is_ascii());
                let can_select = d_usize <= self.page_size
                    && idx < view_len
                    && !(ascii_only && self.candidates.len() <= 1);
                if can_select {
                    if let Some(choice) = self.active_choice_at(idx) {
                        return self.apply_choice(choice);
                    }
                }
                if ascii_only {
                    let mut text = self.take_composing_reset(false);
                    text.push((b'0' + d) as char);
                    KeyAction::Commit {
                        text,
                        learn_keys: None,
                        learn_text: None,
                    }
                } else {
                    KeyAction::ConsumedNoChange
                }
            }
            KeyEvent::Up | KeyEvent::Left => {
                let view_len = self.active_view_len();
                if view_len > 0 {
                    self.selected = if self.selected == 0 {
                        view_len - 1
                    } else {
                        self.selected - 1
                    };
                }
                KeyAction::UpdateComposition {
                    need_convert: false,
                }
            }
            KeyEvent::Down | KeyEvent::Right => {
                let view_len = self.active_view_len();
                if view_len > 0 {
                    self.selected = (self.selected + 1) % view_len;
                }
                KeyAction::UpdateComposition {
                    need_convert: false,
                }
            }
            KeyEvent::Tab => {
                if !self.is_english_composing() {
                    if let Some(off) = self.first_char_offset {
                        if off > 0 && off < self.candidates.len() {
                            if !self.single_char_mode {
                                self.selected_before_tab = Some(self.selected);
                                self.single_char_mode = true;
                                self.selected = 0;
                            } else {
                                self.single_char_mode = false;
                                let prev = self.selected_before_tab.take().unwrap_or(0);
                                self.selected = prev.min(self.candidates.len().saturating_sub(1));
                            }
                            return KeyAction::UpdateComposition {
                                need_convert: false,
                            };
                        }
                    }
                }
                let view_len = self.active_view_len();
                if view_len > 0 {
                    self.selected = (self.selected + 1) % view_len;
                }
                KeyAction::UpdateComposition {
                    need_convert: false,
                }
            }
            KeyEvent::PageUp | KeyEvent::Char('-') => {
                let view_len = self.active_view_len();
                if view_len > 0 {
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
                let view_len = self.active_view_len();
                if view_len > 0 {
                    let page_idx = self.selected / self.page_size;
                    let total_pages = view_len.div_ceil(self.page_size);
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
                // Never drop trailing unconsumed pinyin on punctuation commit: if the active choice
                // is only a prefix, promote the full-span candidate (or remaining raw keys).
                let full_choice = self
                    .active_choice_at(self.selected)
                    .filter(|c| c.consumed_len >= self.keys.len())
                    .or_else(|| {
                        self.choices
                            .iter()
                            .find(|c| c.consumed_len >= self.keys.len())
                            .cloned()
                    });
                let picked_text = full_choice.map(|c| c.text);
                let locked_prefix = self.locked_text();
                let locked_raw = self.locked_raw_keys();
                let rem_keys = self.keys.clone();
                let _ = self.take_composing_reset(false);
                match picked_text {
                    Some(cand) => {
                        let full_text = format!("{locked_prefix}{cand}");
                        let full_keys = format!("{locked_raw}{rem_keys}");
                        let (learn_keys, learn_text) = if cand.is_ascii() {
                            (None, None)
                        } else {
                            (Some(full_keys), Some(full_text.clone()))
                        };
                        KeyAction::Commit {
                            text: format!("{full_text}{punct_owned}"),
                            learn_keys,
                            learn_text,
                        }
                    }
                    None => KeyAction::Commit {
                        text: format!("{locked_prefix}{rem_keys}{punct_owned}"),
                        learn_keys: None,
                        learn_text: None,
                    },
                }
            }
            _ => KeyAction::PassThrough,
        }
    }

    pub fn set_candidates(&mut self, cands: Vec<String>) {
        let len = self.keys.len();
        let choices = cands
            .iter()
            .map(|text| WireCandidate {
                text: text.clone(),
                consumed_len: len,
                kind: WireCandidateKind::Full,
            })
            .collect();
        self.candidates = cands;
        self.choices = choices;
        self.segmented_preedit.clear();
        self.first_char_offset = None;
        self.first_syllable_len = 0;
        self.exit_single_char_mode();
        if self.selected >= self.candidates.len() {
            self.selected = 0;
        }
    }

    pub fn set_rich_candidates(
        &mut self,
        items: Vec<WireCandidate>,
        segmented_preedit: Option<String>,
        first_char_offset: Option<usize>,
        first_syllable_len: usize,
    ) {
        self.candidates = items.iter().map(|it| it.text.clone()).collect();
        self.choices = items;
        self.segmented_preedit = segmented_preedit.unwrap_or_default();
        self.first_char_offset = first_char_offset.filter(|&off| off < self.candidates.len());
        self.first_syllable_len = first_syllable_len;
        self.exit_single_char_mode();
        if self.selected >= self.candidates.len() {
            self.selected = 0;
        }
    }

    pub fn current_page(&self) -> (usize, usize, &[String], usize) {
        let view_start = self.active_view_start();
        let view = &self.candidates[view_start..];
        if view.is_empty() {
            return (0, 0, &[], 0);
        }
        let total_pages = view.len().div_ceil(self.page_size);
        let sel = self.selected.min(view.len() - 1);
        let page_idx = (sel / self.page_size).min(total_pages - 1);
        let start = page_idx * self.page_size;
        let end = (start + self.page_size).min(view.len());
        (page_idx, total_pages, &view[start..end], sel - start)
    }

    pub fn reset_composing(&mut self) {
        self.locked_segments.clear();
        self.keys.clear();
        self.clear_candidates();
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

    fn clear_candidates(&mut self) {
        self.candidates.clear();
        self.choices.clear();
        self.segmented_preedit.clear();
        self.first_char_offset = None;
        self.first_syllable_len = 0;
        self.exit_single_char_mode();
        self.selected = 0;
    }

    fn exit_single_char_mode(&mut self) {
        self.single_char_mode = false;
        self.selected_before_tab = None;
    }

    fn active_view_start(&self) -> usize {
        if self.single_char_mode {
            if let Some(off) = self.first_char_offset {
                if off < self.candidates.len() {
                    return off;
                }
            }
        }
        0
    }

    fn active_view_len(&self) -> usize {
        self.candidates.len().saturating_sub(self.active_view_start())
    }

    fn active_choice_at(&self, rel_idx: usize) -> Option<WireCandidate> {
        let abs = self.active_view_start().checked_add(rel_idx)?;
        self.choices.get(abs).cloned()
    }

    fn apply_choice(&mut self, choice: WireCandidate) -> KeyAction {
        if !self.is_english_composing()
            && choice.consumed_len > 0
            && choice.consumed_len < self.keys.len()
            && self.keys.is_char_boundary(choice.consumed_len)
        {
            let raw_part = self.keys[..choice.consumed_len].to_owned();
            let rest = self.keys[choice.consumed_len..].to_owned();
            self.locked_segments.push(LockedSegment {
                text: choice.text,
                raw_keys: raw_part,
            });
            self.keys = rest;
            self.clear_candidates();
            return KeyAction::UpdateComposition { need_convert: true };
        }

        let locked_prefix = self.locked_text();
        let locked_raw = self.locked_raw_keys();
        let rem_keys = self.keys.clone();
        let is_ascii_choice = choice.text.is_ascii();
        let full_text = format!("{}{}", locked_prefix, choice.text);
        let is_chinese_pick = !full_text.is_ascii();
        let _ = self.take_composing_reset(is_chinese_pick);
        let full_raw = format!("{locked_raw}{rem_keys}");
        let (learn_keys, learn_text) = if is_ascii_choice {
            (None, None)
        } else {
            (Some(full_raw), Some(full_text.clone()))
        };
        KeyAction::Commit {
            text: full_text,
            learn_keys,
            learn_text,
        }
    }

    fn refresh_english_candidates(&mut self) {
        let cands = suggest_english_candidates(&self.keys, 15);
        self.set_candidates(cands);
        self.selected = 0;
    }

    fn take_composing_reset(&mut self, committed_chinese: bool) -> String {
        let mut raw = self.locked_text();
        raw.push_str(&self.keys);
        self.locked_segments.clear();
        self.keys.clear();
        self.clear_candidates();
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

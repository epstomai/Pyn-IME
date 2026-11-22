//! Zero-dependency guarded incremental user learning and anti-pollution memory.
//!
//! Design principles:
//! - Strict gating (G1-G5): single characters, abbreviations, pronunciation mismatches,
//!   protected function-word slot substitutions, and already-top-1 base predictions are never inflated.
//! - Confirm-before-promote: new preferences stay in staging until confirmed cleanly
//!   (`min_confirm_count >= 2.0`, raised dynamically by recent `undo()` strikes).
//! - Self-healing & time decay: selecting the base prediction decays conflicting user overrides,
//!   and unused boosts decay smoothly with a configurable step half-life.
//! - Exact rollback + negative feedback: `LearnDelta` restores every mutation from `commit()`
//!   while subtracting `undo_demote_step` and adding an undo strike.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::Path;

use crate::model::{is_hanzi, Model, FIRST_REAL_WORD_ID, NONE_ID};
use crate::pinyin::{build_dag, pack_exact, syllable_id, syllable_str, DagEdgeKind, SyllableId};

pub const USER_DICT_MAGIC: &str = "# IMEUSR01";

pub(crate) const PROTECTED_FUNCTION_WORDS: &[&str] = &[
    "的", "得", "地", "了", "着", "过", "吗", "呢", "吧", "啊", "呀", "啦", "嘛", "哦", "么",
];

const PROTECTED_FUNCTION_SYLLABLES: &[&str] = &[
    "de", "le", "zhe", "guo", "ma", "ne", "ba", "a", "ya", "la", "o", "me",
];

const COMPOSE_STOP_CHARS: &[char] = &[
    '我', '你', '他', '她', '它', '这', '那', '是', '不', '在', '有', '就', '也', '都', '和', '与',
    '把', '被', '让', '给',
];

#[inline]
pub(crate) fn is_protected_function_char(ch: char) -> bool {
    let mut buf = [0u8; 4];
    let s: &str = ch.encode_utf8(&mut buf);
    PROTECTED_FUNCTION_WORDS.contains(&s)
}

#[inline]
fn is_protected_function_syllable(syl: SyllableId) -> bool {
    PROTECTED_FUNCTION_SYLLABLES.contains(&syllable_str(syl))
}

#[derive(Debug, Clone, Copy)]
pub struct LearnConfig {
    pub enabled: bool,
    pub min_confirm_count: f32,
    pub min_bigram_confirm: f32,
    pub vocab_base_boost: f32,
    pub vocab_log_slope: f32,
    pub max_vocab_boost: f32,
    pub oov_base_boost: f32,
    pub oov_log_slope: f32,
    pub max_oov_boost: f32,
    pub bigram_base_boost: f32,
    pub bigram_log_slope: f32,
    pub max_bigram_boost: f32,
    pub decay_half_life: f32,
    pub undo_demote_step: f32,
    pub undo_strike_penalty: f32,
    pub max_words: usize,
    pub max_bigrams: usize,
}

impl Default for LearnConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_confirm_count: 1.95,
            min_bigram_confirm: 1.95,
            vocab_base_boost: 2.5,
            vocab_log_slope: 0.7,
            max_vocab_boost: 3.8,
            oov_base_boost: 3.0,
            oov_log_slope: 0.8,
            max_oov_boost: 5.0,
            bigram_base_boost: 1.0,
            bigram_log_slope: 0.5,
            max_bigram_boost: 2.0,
            decay_half_life: 2000.0,
            undo_demote_step: 1.5,
            undo_strike_penalty: 1.0,
            max_words: 8192,
            max_bigrams: 16384,
        }
    }
}

#[derive(Debug, Clone)]
pub struct UserWordEntry {
    pub word: String,
    pub packed_key: u64,
    pub syllables: Vec<SyllableId>,
    pub is_oov: bool,
    pub model_word_id: Option<u32>,
    pub count: f32,
    pub last_step: u64,
    pub undo_strikes: u32,
}

#[derive(Debug, Clone)]
pub struct UserBigramEntry {
    pub prev_word: String,
    pub word: String,
    pub prev_word_id: Option<u32>,
    pub count: f32,
    pub last_step: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UserStats {
    pub total_steps: u64,
    pub active_vocab_words: usize,
    pub active_oov_words: usize,
    pub staged_words: usize,
    pub active_bigrams: usize,
}

#[derive(Debug, Clone)]
pub struct ActiveKeyMatch {
    pub word: String,
    pub model_word_id: Option<u32>,
    pub boost: f32,
    pub is_oov: bool,
}

#[derive(Debug, Clone)]
pub struct WordChange {
    pub key: (u64, String),
    pub prev_entry: Option<UserWordEntry>,
    pub syllables: Vec<SyllableId>,
    pub is_oov: bool,
    pub model_word_id: Option<u32>,
    pub eligible_non_base: bool,
}

#[derive(Debug, Clone, Default)]
pub struct LearnDelta {
    pub prev_step: u64,
    pub word_change: Option<WordChange>,
    pub self_heal_change: Option<((u64, String), Option<UserWordEntry>)>,
    pub bigram_change: Option<((u32, String), Option<UserBigramEntry>)>,
    pub flushed_compose_change: Option<((u64, String), Option<UserWordEntry>)>,
    pub prev_compose_run: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct UserMemory {
    config: LearnConfig,
    step: u64,
    words: HashMap<(u64, String), UserWordEntry>,
    by_key: HashMap<u64, Vec<String>>,
    bigrams: HashMap<u32, Vec<UserBigramEntry>>,
    bigram_total: usize,
    compose_run: Vec<(String, String)>,
}

impl Default for UserMemory {
    fn default() -> Self {
        Self::new(LearnConfig::default())
    }
}

impl UserMemory {
    pub fn new(config: LearnConfig) -> Self {
        Self {
            config,
            step: 0,
            words: HashMap::new(),
            by_key: HashMap::new(),
            bigrams: HashMap::new(),
            bigram_total: 0,
            compose_run: Vec::new(),
        }
    }

    pub fn config(&self) -> LearnConfig {
        self.config
    }

    pub fn set_config(&mut self, config: LearnConfig) {
        self.config = config;
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.config.enabled = enabled;
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty() && self.bigrams.is_empty()
    }

    pub fn clear(&mut self) {
        self.step = 0;
        self.words.clear();
        self.by_key.clear();
        self.bigrams.clear();
        self.bigram_total = 0;
        self.compose_run.clear();
    }

    #[inline]
    fn decayed_count(&self, count: f32, last_step: u64) -> f32 {
        decayed_count(count, last_step, self.step, self.config.decay_half_life)
    }

    #[inline]
    fn effective_threshold(&self, entry: &UserWordEntry) -> f32 {
        self.config.min_confirm_count
            + (entry.undo_strikes as f32 * self.config.undo_strike_penalty).min(3.0)
    }

    fn word_boost(&self, entry: &UserWordEntry) -> f32 {
        let eff = self.decayed_count(entry.count, entry.last_step);
        let thresh = self.effective_threshold(entry);
        if eff < thresh {
            return 0.0;
        }
        if entry.is_oov {
            (self.config.oov_base_boost + self.config.oov_log_slope * (eff - thresh + 1.0).ln())
                .min(self.config.max_oov_boost)
        } else {
            (self.config.vocab_base_boost
                + self.config.vocab_log_slope * (eff - thresh + 1.0).ln())
            .min(self.config.max_vocab_boost)
        }
    }

    fn bigram_boost(&self, entry: &UserBigramEntry) -> f32 {
        let eff = self.decayed_count(entry.count, entry.last_step);
        if eff < self.config.min_bigram_confirm {
            return 0.0;
        }
        (self.config.bigram_base_boost
            + self.config.bigram_log_slope * (eff - self.config.min_bigram_confirm + 1.0).ln())
        .min(self.config.max_bigram_boost)
    }

    pub fn active_entries_for_key(&self, packed_key: u64) -> Vec<ActiveKeyMatch> {
        if !self.config.enabled || self.words.is_empty() {
            return Vec::new();
        }
        let Some(word_list) = self.by_key.get(&packed_key) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for w in word_list {
            if let Some(entry) = self.words.get(&(packed_key, w.clone())) {
                let boost = self.word_boost(entry);
                if boost > 0.0 {
                    out.push(ActiveKeyMatch {
                        word: entry.word.clone(),
                        model_word_id: entry.model_word_id,
                        boost,
                        is_oov: entry.is_oov,
                    });
                }
            }
        }
        out
    }

    pub fn active_bigram_boost(&self, prev_h2: u32, word: &str) -> f32 {
        if !self.config.enabled
            || self.bigrams.is_empty()
            || prev_h2 == NONE_ID
            || prev_h2 < FIRST_REAL_WORD_ID
        {
            return 0.0;
        }
        let Some(entries) = self.bigrams.get(&prev_h2) else {
            return 0.0;
        };
        for e in entries {
            if e.word == word {
                return self.bigram_boost(e);
            }
        }
        0.0
    }

    pub fn stats(&self) -> UserStats {
        let mut stats = UserStats {
            total_steps: self.step,
            ..UserStats::default()
        };
        for entry in self.words.values() {
            let boost = self.word_boost(entry);
            if boost > 0.0 {
                if entry.is_oov {
                    stats.active_oov_words += 1;
                } else {
                    stats.active_vocab_words += 1;
                }
            } else if entry.count > 0.0 {
                stats.staged_words += 1;
            }
        }
        for bucket in self.bigrams.values() {
            for e in bucket {
                if self.bigram_boost(e) > 0.0 {
                    stats.active_bigrams += 1;
                }
            }
        }
        stats
    }

    pub fn active_words_summary(&self) -> Vec<(String, f32, bool)> {
        if !self.config.enabled {
            return Vec::new();
        }
        let mut rows: Vec<(String, f32, bool)> = self
            .words
            .values()
            .filter_map(|e| {
                let b = self.word_boost(e);
                if b > 0.0 {
                    Some((e.word.clone(), b, e.is_oov))
                } else {
                    None
                }
            })
            .collect();
        rows.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        rows
    }

    pub fn flush_compose_run(&mut self, model: &Model) {
        if !self.config.enabled || self.compose_run.len() < 2 {
            self.compose_run.clear();
            return;
        }
        let _ = self.try_stage_compose_run(model);
        self.compose_run.clear();
    }

    pub fn record_commit(
        &mut self,
        model: &Model,
        keys: &str,
        text: &str,
        context_before: &str,
        base_top1: Option<&str>,
        current_top1: Option<&str>,
    ) -> LearnDelta {
        if !self.config.enabled {
            return LearnDelta::default();
        }

        let prev_step = self.step;
        let prev_compose_run = self.compose_run.clone();
        self.step = self.step.saturating_add(1);

        let mut delta = LearnDelta {
            prev_step,
            prev_compose_run,
            ..LearnDelta::default()
        };

        // G5: track short 1..=2 char exact-pinyin commits for cross-commit OOV assembly.
        let text_chars: Vec<char> = text.chars().collect();
        let is_compose_piece = (1..=2).contains(&text_chars.len())
            && text_chars.iter().all(|&c| is_hanzi(c))
            && !keys.is_empty()
            && build_dag(keys).exact_only;

        if is_compose_piece {
            let cur_len: usize = self
                .compose_run
                .iter()
                .map(|(_, t)| t.chars().count())
                .sum();
            if cur_len + text_chars.len() <= 4 {
                self.compose_run.push((keys.to_owned(), text.to_owned()));
            } else {
                if self.compose_run.len() >= 2 {
                    delta.flushed_compose_change = self.try_stage_compose_run(model);
                }
                self.compose_run.clear();
                self.compose_run.push((keys.to_owned(), text.to_owned()));
            }
        } else {
            if self.compose_run.len() >= 2 {
                delta.flushed_compose_change = self.try_stage_compose_run(model);
            }
            self.compose_run.clear();
        }

        // G1: only learn multi-char (2..=6) pure-Hanzi phrases; single chars are frozen.
        if !(2..=6).contains(&text_chars.len()) || !text_chars.iter().all(|&c| is_hanzi(c)) {
            return delta;
        }

        // G2: strict exact-syllable phonetic alignment (rejects abbrev and non-matching text).
        let Some(syls) = align_exact_syllables(model, keys, text) else {
            return delta;
        };
        let Some(packed_key) = pack_exact(&syls) else {
            return delta;
        };

        // G3a: never override or substitute a protected function-word slot relative to base top-1.
        if let Some(base) = base_top1 {
            if has_protected_function_mismatch(base, &text_chars) {
                return delta;
            }
        }

        // G3b: OOV phrases containing protected function words are rejected unconditionally.
        let model_word_id = model.word_id(text);
        let is_oov = model_word_id.is_none();
        if is_oov && text_chars.iter().any(|&c| is_protected_function_char(c)) {
            return delta;
        }

        // G4: mistake-driven update — if base top-1 already won cleanly, do not inflate boosts;
        // if an earlier learned entry wrongly beat base top-1, softly self-heal (decay) it.
        if base_top1 == Some(text) {
            if let Some(cur) = current_top1 {
                if cur != text {
                    let heal_key = (packed_key, cur.to_owned());
                    if self.words.contains_key(&heal_key) {
                        let prev = self.words.get(&heal_key).cloned();
                        delta.self_heal_change = Some((heal_key.clone(), prev));
                        let now_step = self.step;
                        let half_life = self.config.decay_half_life;
                        let remove_it = if let Some(entry) = self.words.get_mut(&heal_key) {
                            let cur_cnt = decayed_count(entry.count, entry.last_step, now_step, half_life);
                            entry.count = (cur_cnt - 0.75).max(0.0);
                            entry.last_step = now_step;
                            entry.count <= 0.0 && entry.undo_strikes == 0
                        } else {
                            false
                        };
                        if remove_it {
                            self.remove_word_entry(&heal_key);
                        }
                    }
                }
            }
            return delta;
        }

        let word_key = (packed_key, text.to_owned());
        let prev_entry = self.words.get(&word_key).cloned();
        delta.word_change = Some(WordChange {
            key: word_key.clone(),
            prev_entry,
            syllables: syls.clone(),
            is_oov,
            model_word_id,
            eligible_non_base: true,
        });

        self.increment_word_entry(word_key, syls, is_oov, model_word_id, 1.0);
        self.evict_words_if_needed();

        // Context-conditioned bigram for clean 2+ char non-function preceding words.
        let prev_ctx = model.context_state(context_before);
        if prev_ctx.has_context && prev_ctx.h2 >= FIRST_REAL_WORD_ID && prev_ctx.h2 != NONE_ID {
            let prev_str = model.word_str(prev_ctx.h2);
            if prev_str.chars().count() >= 2 && !prev_str.chars().any(is_protected_function_char) {
                let bg_key = (prev_ctx.h2, text.to_owned());
                let prev_bg = self.find_bigram_entry(prev_ctx.h2, text).cloned();
                delta.bigram_change = Some((bg_key, prev_bg));
                self.increment_bigram_entry(prev_ctx.h2, prev_str.to_owned(), text.to_owned(), 1.0);
                self.evict_bigrams_if_needed();
            }
        }

        delta
    }

    pub fn undo_commit(&mut self, delta: LearnDelta) {
        if !self.config.enabled {
            return;
        }

        self.step = delta.prev_step;
        self.compose_run = delta.prev_compose_run;

        if let Some((key, prev_opt)) = delta.flushed_compose_change {
            self.restore_word_entry(key, prev_opt);
        }
        if let Some((bg_key, prev_opt)) = delta.bigram_change {
            self.restore_bigram_entry(bg_key, prev_opt);
        }
        if let Some((key, prev_opt)) = delta.self_heal_change {
            self.restore_word_entry(key, prev_opt);
        }
        if let Some(wc) = delta.word_change {
            let key = wc.key;
            self.restore_word_entry(key.clone(), wc.prev_entry);
            if wc.eligible_non_base {
                let now_step = self.step;
                let half_life = self.config.decay_half_life;
                let demote = self.config.undo_demote_step;
                if let Some(entry) = self.words.get_mut(&key) {
                    let cur = decayed_count(entry.count, entry.last_step, now_step, half_life);
                    entry.count = (cur - demote).max(0.0);
                    entry.last_step = now_step;
                    entry.undo_strikes = entry.undo_strikes.saturating_add(1);
                } else {
                    let strike_entry = UserWordEntry {
                        word: key.1.clone(),
                        packed_key: key.0,
                        syllables: wc.syllables,
                        is_oov: wc.is_oov,
                        model_word_id: wc.model_word_id,
                        count: 0.0,
                        last_step: now_step,
                        undo_strikes: 1,
                    };
                    self.insert_word_entry(key, strike_entry);
                }
            }
        }
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let file = File::create(path)?;
        let mut w = BufWriter::new(file);
        writeln!(w, "{USER_DICT_MAGIC}\tstep={}", self.step)?;

        let mut words: Vec<&UserWordEntry> = self.words.values().collect();
        words.sort_by(|a, b| {
            b.count
                .total_cmp(&a.count)
                .then_with(|| a.word.cmp(&b.word))
                .then_with(|| a.packed_key.cmp(&b.packed_key))
        });
        for entry in words {
            if entry.count <= 0.0 && entry.undo_strikes == 0 {
                continue;
            }
            let syls: Vec<&str> = entry.syllables.iter().map(|&id| syllable_str(id)).collect();
            writeln!(
                w,
                "W\t{}\t{}\t{:.6}\t{}\t{}",
                entry.word,
                syls.join(","),
                entry.count,
                entry.last_step,
                entry.undo_strikes
            )?;
        }

        let mut bigrams: Vec<&UserBigramEntry> = self.bigrams.values().flatten().collect();
        bigrams.sort_by(|a, b| {
            b.count
                .total_cmp(&a.count)
                .then_with(|| a.prev_word.cmp(&b.prev_word))
                .then_with(|| a.word.cmp(&b.word))
        });
        for bg in bigrams {
            if bg.count <= 0.0 {
                continue;
            }
            writeln!(
                w,
                "B\t{}\t{}\t{:.6}\t{}",
                bg.prev_word, bg.word, bg.count, bg.last_step
            )?;
        }
        w.flush()
    }

    pub fn load(path: &Path, model: &Model, config: LearnConfig) -> io::Result<Self> {
        let file = File::open(path)?;
        let mut lines = BufReader::new(file).lines();
        let Some(header_res) = lines.next() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "empty user dict file",
            ));
        };
        let header = header_res?;
        if !header.starts_with(USER_DICT_MAGIC) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid user dict magic header",
            ));
        }

        let mut mem = Self::new(config);
        for part in header.split('\t').skip(1) {
            if let Some(rest) = part.strip_prefix("step=") {
                if let Ok(step) = rest.parse::<u64>() {
                    mem.step = step;
                }
            }
        }

        for line_res in lines {
            let line = line_res?;
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let cols: Vec<&str> = line.split('\t').collect();
            match cols.first().copied() {
                Some("W") if cols.len() >= 6 => {
                    let word = cols[1].trim();
                    let syl_names: Vec<&str> = cols[2].split(',').filter(|s| !s.is_empty()).collect();
                    let Ok(count) = cols[3].parse::<f32>() else {
                        continue;
                    };
                    let Ok(last_step) = cols[4].parse::<u64>() else {
                        continue;
                    };
                    let Ok(undo_strikes) = cols[5].parse::<u32>() else {
                        continue;
                    };
                    let chars: Vec<char> = word.chars().collect();
                    if !(2..=6).contains(&chars.len())
                        || chars.len() != syl_names.len()
                        || !chars.iter().all(|&c| is_hanzi(c))
                    {
                        continue;
                    }
                    let mut syls = Vec::with_capacity(syl_names.len());
                    let mut valid = true;
                    for (&ch, &sn) in chars.iter().zip(syl_names.iter()) {
                        let Some(id) = syllable_id(sn) else {
                            valid = false;
                            break;
                        };
                        if model.char_reading_logp(ch, id).is_none() {
                            valid = false;
                            break;
                        }
                        syls.push(id);
                    }
                    if !valid {
                        continue;
                    }
                    let Some(packed_key) = pack_exact(&syls) else {
                        continue;
                    };
                    let model_word_id = model.word_id(word);
                    let is_oov = model_word_id.is_none();
                    if is_oov && chars.iter().any(|&c| is_protected_function_char(c)) {
                        continue;
                    }
                    let key = (packed_key, word.to_owned());
                    let entry = UserWordEntry {
                        word: word.to_owned(),
                        packed_key,
                        syllables: syls,
                        is_oov,
                        model_word_id,
                        count: count.max(0.0),
                        last_step,
                        undo_strikes,
                    };
                    mem.insert_word_entry(key, entry);
                }
                Some("B") if cols.len() >= 5 => {
                    let prev_word = cols[1].trim();
                    let word = cols[2].trim();
                    let Ok(count) = cols[3].parse::<f32>() else {
                        continue;
                    };
                    let Ok(last_step) = cols[4].parse::<u64>() else {
                        continue;
                    };
                    if let Some(prev_id) = model.word_id(prev_word) {
                        if prev_id >= FIRST_REAL_WORD_ID && count > 0.0 {
                            mem.insert_bigram_entry(
                                prev_id,
                                UserBigramEntry {
                                    prev_word: prev_word.to_owned(),
                                    word: word.to_owned(),
                                    prev_word_id: Some(prev_id),
                                    count,
                                    last_step,
                                },
                            );
                        }
                    }
                }
                _ => {}
            }
        }
        mem.evict_words_if_needed();
        mem.evict_bigrams_if_needed();
        Ok(mem)
    }

    fn try_stage_compose_run(&mut self, model: &Model) -> Option<((u64, String), Option<UserWordEntry>)> {
        if self.compose_run.len() < 2 {
            return None;
        }
        let mut joined_keys = String::new();
        let mut joined_text = String::new();
        for (k, t) in &self.compose_run {
            joined_keys.push_str(k);
            joined_text.push_str(t);
        }
        let chars: Vec<char> = joined_text.chars().collect();
        if !(2..=4).contains(&chars.len()) {
            return None;
        }
        if model.word_id(&joined_text).is_some() {
            return None;
        }
        if chars
            .iter()
            .any(|&c| is_protected_function_char(c) || COMPOSE_STOP_CHARS.contains(&c))
        {
            return None;
        }
        let syls = align_exact_syllables(model, &joined_keys, &joined_text)?;
        if is_protected_function_syllable(*syls.last()?) {
            return None;
        }
        let packed_key = pack_exact(&syls)?;
        let key = (packed_key, joined_text);
        let prev = self.words.get(&key).cloned();
        self.increment_word_entry(key.clone(), syls, true, None, 1.0);
        self.evict_words_if_needed();
        Some((key, prev))
    }

    fn increment_word_entry(
        &mut self,
        key: (u64, String),
        syllables: Vec<SyllableId>,
        is_oov: bool,
        model_word_id: Option<u32>,
        delta_count: f32,
    ) {
        let now_step = self.step;
        let half_life = self.config.decay_half_life;
        if let Some(entry) = self.words.get_mut(&key) {
            let cur = decayed_count(entry.count, entry.last_step, now_step, half_life);
            entry.count = cur + delta_count;
            entry.last_step = now_step;
            return;
        }
        let entry = UserWordEntry {
            word: key.1.clone(),
            packed_key: key.0,
            syllables,
            is_oov,
            model_word_id,
            count: delta_count,
            last_step: now_step,
            undo_strikes: 0,
        };
        self.insert_word_entry(key, entry);
    }

    fn insert_word_entry(&mut self, key: (u64, String), entry: UserWordEntry) {
        let packed_key = key.0;
        let word = key.1.clone();
        self.words.insert(key, entry);
        let bucket = self.by_key.entry(packed_key).or_default();
        if !bucket.iter().any(|w| w == &word) {
            bucket.push(word);
        }
    }

    fn remove_word_entry(&mut self, key: &(u64, String)) {
        if self.words.remove(key).is_some() {
            if let Some(bucket) = self.by_key.get_mut(&key.0) {
                bucket.retain(|w| w != &key.1);
                if bucket.is_empty() {
                    self.by_key.remove(&key.0);
                }
            }
        }
    }

    fn restore_word_entry(&mut self, key: (u64, String), prev_opt: Option<UserWordEntry>) {
        match prev_opt {
            Some(prev) => self.insert_word_entry(key, prev),
            None => self.remove_word_entry(&key),
        }
    }

    fn evict_words_if_needed(&mut self) {
        let max_words = self.config.max_words;
        if max_words == 0 {
            self.words.clear();
            self.by_key.clear();
            return;
        }
        while self.words.len() > max_words {
            let now_step = self.step;
            let half_life = self.config.decay_half_life;
            let victim = self
                .words
                .iter()
                .min_by(|a, b| {
                    let ca = decayed_count(a.1.count, a.1.last_step, now_step, half_life);
                    let cb = decayed_count(b.1.count, b.1.last_step, now_step, half_life);
                    ca.total_cmp(&cb)
                        .then_with(|| a.1.last_step.cmp(&b.1.last_step))
                })
                .map(|(k, _)| k.clone());
            if let Some(k) = victim {
                self.remove_word_entry(&k);
            } else {
                break;
            }
        }
    }

    fn find_bigram_entry(&self, prev_id: u32, word: &str) -> Option<&UserBigramEntry> {
        self.bigrams
            .get(&prev_id)
            .and_then(|v| v.iter().find(|e| e.word == word))
    }

    fn increment_bigram_entry(
        &mut self,
        prev_id: u32,
        prev_word: String,
        word: String,
        delta_count: f32,
    ) {
        let now_step = self.step;
        let half_life = self.config.decay_half_life;
        let bucket = self.bigrams.entry(prev_id).or_default();
        if let Some(entry) = bucket.iter_mut().find(|e| e.word == word) {
            let cur = decayed_count(entry.count, entry.last_step, now_step, half_life);
            entry.count = cur + delta_count;
            entry.last_step = now_step;
            return;
        }
        bucket.push(UserBigramEntry {
            prev_word,
            word,
            prev_word_id: Some(prev_id),
            count: delta_count,
            last_step: now_step,
        });
        self.bigram_total += 1;
    }

    fn insert_bigram_entry(&mut self, prev_id: u32, entry: UserBigramEntry) {
        let bucket = self.bigrams.entry(prev_id).or_default();
        if let Some(existing) = bucket.iter_mut().find(|e| e.word == entry.word) {
            *existing = entry;
        } else {
            bucket.push(entry);
            self.bigram_total += 1;
        }
    }

    fn remove_bigram_entry(&mut self, prev_id: u32, word: &str) {
        if let Some(bucket) = self.bigrams.get_mut(&prev_id) {
            let before = bucket.len();
            bucket.retain(|e| e.word != word);
            if bucket.len() < before {
                self.bigram_total = self.bigram_total.saturating_sub(before - bucket.len());
            }
            if bucket.is_empty() {
                self.bigrams.remove(&prev_id);
            }
        }
    }

    fn restore_bigram_entry(
        &mut self,
        key: (u32, String),
        prev_opt: Option<UserBigramEntry>,
    ) {
        match prev_opt {
            Some(prev) => self.insert_bigram_entry(key.0, prev),
            None => self.remove_bigram_entry(key.0, &key.1),
        }
    }

    fn evict_bigrams_if_needed(&mut self) {
        let max_bigrams = self.config.max_bigrams;
        if max_bigrams == 0 {
            self.bigrams.clear();
            self.bigram_total = 0;
            return;
        }
        while self.bigram_total > max_bigrams {
            let now_step = self.step;
            let half_life = self.config.decay_half_life;
            let mut best: Option<(u32, String, f32, u64)> = None;
            for (&prev_id, bucket) in &self.bigrams {
                for e in bucket {
                    let c = decayed_count(e.count, e.last_step, now_step, half_life);
                    let replace = match best {
                        None => true,
                        Some((_, _, bc, bs)) => {
                            c < bc || ((c - bc).abs() <= f32::EPSILON && e.last_step < bs)
                        }
                    };
                    if replace {
                        best = Some((prev_id, e.word.clone(), c, e.last_step));
                    }
                }
            }
            if let Some((prev_id, word, _, _)) = best {
                self.remove_bigram_entry(prev_id, &word);
            } else {
                break;
            }
        }
    }
}

#[inline]
pub(crate) fn decayed_count(count: f32, last_step: u64, now_step: u64, half_life: f32) -> f32 {
    if count <= 0.0 {
        return 0.0;
    }
    let dt = now_step.saturating_sub(last_step) as f32;
    count * 0.5_f32.powf(dt / half_life.max(1.0))
}

fn has_protected_function_mismatch(base_top1: &str, text_chars: &[char]) -> bool {
    let base_chars: Vec<char> = base_top1.chars().collect();
    if base_chars.len() != text_chars.len() {
        return false;
    }
    base_chars.iter().zip(text_chars.iter()).any(|(&bc, &tc)| {
        bc != tc && (is_protected_function_char(bc) || is_protected_function_char(tc))
    })
}

/// Finds the highest-probability exact syllable sequence spelling `text` along `keys`.
/// Returns `None` if `keys` uses abbreviations or any character cannot be read as its aligned syllable.
pub fn align_exact_syllables(model: &Model, keys: &str, text: &str) -> Option<Vec<SyllableId>> {
    let dag = build_dag(keys);
    if dag.len == 0 || !dag.exact_only {
        return None;
    }
    let chars: Vec<char> = text.chars().collect();
    if !(2..=6).contains(&chars.len()) || !chars.iter().all(|&c| is_hanzi(c)) {
        return None;
    }

    struct Search<'a> {
        model: &'a Model,
        dag: &'a crate::pinyin::PinyinDag,
        chars: &'a [char],
        cur_syls: Vec<SyllableId>,
        best_score: f32,
        best_syls: Option<Vec<SyllableId>>,
    }

    impl<'a> Search<'a> {
        fn dfs(&mut self, pos: usize, char_idx: usize, score: f32) {
            let rem_chars = self.chars.len() - char_idx;
            let rem_keys = self.dag.len - pos;
            if rem_chars == 0 {
                if rem_keys == 0 && score > self.best_score {
                    self.best_score = score;
                    self.best_syls = Some(self.cur_syls.clone());
                }
                return;
            }
            if rem_keys < rem_chars || rem_keys > rem_chars * 6 {
                return;
            }
            let ch = self.chars[char_idx];
            for edge in &self.dag.edges_from[pos] {
                if let DagEdgeKind::Exact(syl) = edge.kind {
                    if let Some(rp) = self.model.char_reading_logp(ch, syl) {
                        self.cur_syls.push(syl);
                        self.dfs(edge.end, char_idx + 1, score + rp);
                        self.cur_syls.pop();
                    }
                }
            }
        }
    }

    let mut search = Search {
        model,
        dag: &dag,
        chars: &chars,
        cur_syls: Vec::with_capacity(chars.len()),
        best_score: f32::NEG_INFINITY,
        best_syls: None,
    };
    search.dfs(0, 0, 0.0);
    search.best_syls
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{LexEntry, LexTable, EOS_ID, PAUSE_ID};

    fn make_user_test_model() -> Model {
        let words = [
            "<bos>", "<eos>", "<pause>", "的", "得", "地", "了", "乐", "我", "说", "林", "子",
            "轩", "权", "力", "利", "权力", "权利", "快", "快乐", "快了", "我说了", "好", "好的",
        ];
        let mut word_offsets = Vec::with_capacity(words.len() + 1);
        let mut word_bytes = Vec::new();
        for w in &words {
            word_offsets.push(word_bytes.len() as u32);
            word_bytes.extend_from_slice(w.as_bytes());
        }
        word_offsets.push(word_bytes.len() as u32);

        let n = words.len();
        let mut unigram_logp = vec![-6.0_f32; n];
        unigram_logp[EOS_ID as usize] = -2.0;
        unigram_logp[PAUSE_ID as usize] = -2.5;
        unigram_logp[16] = -3.0; // 权力
        unigram_logp[17] = -4.0; // 权利
        unigram_logp[19] = -2.5; // 快乐
        unigram_logp[20] = -4.0; // 快了
        unigram_logp[21] = -2.5; // 我说了
        let unigram_bow = vec![-0.5_f32; n];

        let mut exact_pairs = vec![
            (pack_exact(&[syllable_id("de").unwrap()]).unwrap(), LexEntry { word_id: 3, reading_logp: -0.01 }),
            (pack_exact(&[syllable_id("de").unwrap()]).unwrap(), LexEntry { word_id: 4, reading_logp: -0.05 }),
            (pack_exact(&[syllable_id("de").unwrap()]).unwrap(), LexEntry { word_id: 5, reading_logp: -0.2 }),
            (pack_exact(&[syllable_id("le").unwrap()]).unwrap(), LexEntry { word_id: 6, reading_logp: -0.01 }),
            (pack_exact(&[syllable_id("le").unwrap()]).unwrap(), LexEntry { word_id: 7, reading_logp: -0.05 }),
            (pack_exact(&[syllable_id("wo").unwrap()]).unwrap(), LexEntry { word_id: 8, reading_logp: 0.0 }),
            (pack_exact(&[syllable_id("shuo").unwrap()]).unwrap(), LexEntry { word_id: 9, reading_logp: 0.0 }),
            (pack_exact(&[syllable_id("lin").unwrap()]).unwrap(), LexEntry { word_id: 10, reading_logp: 0.0 }),
            (pack_exact(&[syllable_id("zi").unwrap()]).unwrap(), LexEntry { word_id: 11, reading_logp: 0.0 }),
            (pack_exact(&[syllable_id("xuan").unwrap()]).unwrap(), LexEntry { word_id: 12, reading_logp: 0.0 }),
            (pack_exact(&[syllable_id("quan").unwrap()]).unwrap(), LexEntry { word_id: 13, reading_logp: 0.0 }),
            (pack_exact(&[syllable_id("li").unwrap()]).unwrap(), LexEntry { word_id: 14, reading_logp: -0.02 }),
            (pack_exact(&[syllable_id("li").unwrap()]).unwrap(), LexEntry { word_id: 15, reading_logp: -0.05 }),
            (
                pack_exact(&[syllable_id("quan").unwrap(), syllable_id("li").unwrap()]).unwrap(),
                LexEntry { word_id: 16, reading_logp: 0.0 },
            ),
            (
                pack_exact(&[syllable_id("quan").unwrap(), syllable_id("li").unwrap()]).unwrap(),
                LexEntry { word_id: 17, reading_logp: 0.0 },
            ),
            (pack_exact(&[syllable_id("kuai").unwrap()]).unwrap(), LexEntry { word_id: 18, reading_logp: 0.0 }),
            (
                pack_exact(&[syllable_id("kuai").unwrap(), syllable_id("le").unwrap()]).unwrap(),
                LexEntry { word_id: 19, reading_logp: 0.0 },
            ),
            (
                pack_exact(&[syllable_id("kuai").unwrap(), syllable_id("le").unwrap()]).unwrap(),
                LexEntry { word_id: 20, reading_logp: -0.1 },
            ),
            (
                pack_exact(&[
                    syllable_id("wo").unwrap(),
                    syllable_id("shuo").unwrap(),
                    syllable_id("le").unwrap(),
                ])
                .unwrap(),
                LexEntry { word_id: 21, reading_logp: 0.0 },
            ),
            (pack_exact(&[syllable_id("hao").unwrap()]).unwrap(), LexEntry { word_id: 22, reading_logp: 0.0 }),
            (
                pack_exact(&[syllable_id("hao").unwrap(), syllable_id("de").unwrap()]).unwrap(),
                LexEntry { word_id: 23, reading_logp: 0.0 },
            ),
        ];
        exact_pairs.sort_by_key(|x| x.0);
        let mut keys = Vec::new();
        let mut ranges = vec![0u32];
        let mut entries = Vec::new();
        for (k, e) in exact_pairs {
            if keys.last().copied() != Some(k) {
                if !keys.is_empty() {
                    ranges.push(entries.len() as u32);
                }
                keys.push(k);
            }
            entries.push(e);
        }
        ranges.push(entries.len() as u32);
        let exact = LexTable {
            keys,
            ranges,
            entries,
        };
        let initial_chars: [Vec<LexEntry>; 26] = std::array::from_fn(|_| Vec::new());

        Model::from_parts(
            word_offsets,
            word_bytes,
            unigram_logp,
            unigram_bow,
            exact,
            initial_chars,
            LexTable::default(),
            LexTable::default(),
            vec![0u32; n + 1],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![0u32],
            Vec::new(),
            Vec::new(),
        )
    }

    #[test]
    fn guardrails_block_single_char_abbrev_and_function_word_mispicks() {
        let model = make_user_test_model();
        let mut mem = UserMemory::default();

        // G1: single chars are ignored completely.
        mem.record_commit(&model, "le", "乐", "", Some("了"), Some("了"));
        mem.record_commit(&model, "le", "乐", "", Some("了"), Some("了"));
        assert!(mem.is_empty());

        // G2: abbreviations are ignored completely.
        mem.record_commit(&model, "hd", "好得", "", Some("好的"), Some("好的"));
        mem.record_commit(&model, "hd", "好得", "", Some("好的"), Some("好的"));
        assert!(mem.is_empty());

        // G3a & G3b: function-word slot mismatch / OOV with protected function word is rejected.
        mem.record_commit(&model, "woshuole", "我说乐", "", Some("我说了"), Some("我说了"));
        mem.record_commit(&model, "woshuole", "我说乐", "", Some("我说了"), Some("我说了"));
        mem.record_commit(&model, "haode", "好得", "", Some("好的"), Some("好的"));
        mem.record_commit(&model, "haode", "好得", "", Some("好的"), Some("好的"));
        assert!(mem.is_empty());

        // G4: committing base_top1 when current_top1 already equals base_top1 never inflates counts.
        mem.record_commit(&model, "kuaile", "快乐", "", Some("快乐"), Some("快乐"));
        mem.record_commit(&model, "kuaile", "快乐", "", Some("快乐"), Some("快乐"));
        assert!(mem.is_empty());
    }

    #[test]
    fn staging_promotion_self_heal_and_undo_strikes() {
        let model = make_user_test_model();
        let mut mem = UserMemory::default();
        let quanli_key = pack_exact(&[
            syllable_id("quan").unwrap(),
            syllable_id("li").unwrap(),
        ])
        .unwrap();

        // Commit 1: staged only (boost == 0.0).
        let d1 = mem.record_commit(&model, "quanli", "权利", "", Some("权力"), Some("权力"));
        assert!(mem.active_entries_for_key(quanli_key).is_empty());
        assert_eq!(mem.stats().staged_words, 1);

        // Undo staged commit: reverts staging AND adds strike (threshold becomes 3.0).
        mem.undo_commit(d1);
        assert!(mem.active_entries_for_key(quanli_key).is_empty());

        // Takes 3 confirmations after strike before promotion.
        mem.record_commit(&model, "quanli", "权利", "", Some("权力"), Some("权力"));
        mem.record_commit(&model, "quanli", "权利", "", Some("权力"), Some("权力"));
        assert!(mem.active_entries_for_key(quanli_key).is_empty());
        let d3 = mem.record_commit(&model, "quanli", "权利", "", Some("权力"), Some("权力"));
        assert_eq!(mem.active_entries_for_key(quanli_key).len(), 1);
        assert!(mem.active_entries_for_key(quanli_key)[0].boost >= 2.5);

        // Undo the promoted commit: immediately drops below threshold!
        mem.undo_commit(d3);
        assert!(mem.active_entries_for_key(quanli_key).is_empty());

        // Re-promote, then pick base_top1 "权力" to self-heal down.
        for _ in 0..4 {
            mem.record_commit(&model, "quanli", "权利", "", Some("权力"), Some("权力"));
        }
        assert!(!mem.active_entries_for_key(quanli_key).is_empty());
        let d_heal = mem.record_commit(&model, "quanli", "权力", "", Some("权力"), Some("权利"));
        let after_heal = mem.active_entries_for_key(quanli_key);
        assert!(after_heal.is_empty() || after_heal[0].boost < 3.0);
        mem.undo_commit(d_heal);
    }

    #[test]
    fn cross_commit_oov_and_persistence_roundtrip() {
        let model = make_user_test_model();
        let mut mem = UserMemory::default();
        let lzx_key = pack_exact(&[
            syllable_id("lin").unwrap(),
            syllable_id("zi").unwrap(),
            syllable_id("xuan").unwrap(),
        ])
        .unwrap();

        for _ in 0..2 {
            mem.record_commit(&model, "lin", "林", "", Some("林"), Some("林"));
            mem.record_commit(&model, "zi", "子", "", Some("子"), Some("子"));
            mem.record_commit(&model, "xuan", "轩", "", Some("轩"), Some("轩"));
            mem.flush_compose_run(&model);
        }

        let active = mem.active_entries_for_key(lzx_key);
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].word, "林子轩");
        assert!(active[0].is_oov);
        assert!(active[0].boost >= 3.0);

        let tmp = std::env::temp_dir().join("ime_user_dict_roundtrip_test.tsv");
        mem.save(&tmp).unwrap();
        let reloaded = UserMemory::load(&tmp, &model, LearnConfig::default()).unwrap();
        let _ = std::fs::remove_file(&tmp);

        let reloaded_active = reloaded.active_entries_for_key(lzx_key);
        assert_eq!(reloaded_active.len(), 1);
        assert_eq!(reloaded_active[0].word, "林子轩");
        assert!((reloaded_active[0].boost - active[0].boost).abs() < 1e-4);
    }
}

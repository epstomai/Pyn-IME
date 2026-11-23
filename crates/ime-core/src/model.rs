//! Compact binary lexicon and Kneser-Ney trigram language model (`IMEMOD01`).

use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;

use crate::pinyin::{
    pack_exact, pack_initials, syllable_first_letter, syllable_matches_digraph,
    syllable_matches_unit, unit_first_letter, DagEdgeKind, SpanPron, SyllableId,
};
use crate::user_dict::PROTECTED_FUNCTION_WORDS;

pub const MAGIC: &[u8; 8] = b"IMEMOD01";
pub const BOS_ID: u32 = 0;
pub const EOS_ID: u32 = 1;
pub const PAUSE_ID: u32 = 2;
pub const FIRST_REAL_WORD_ID: u32 = 3;
pub const NONE_ID: u32 = u32::MAX;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LexEntry {
    pub word_id: u32,
    pub reading_logp: f32,
}

#[derive(Debug, Clone, Default)]
pub struct LexTable {
    pub keys: Vec<u64>,
    pub ranges: Vec<u32>,
    pub entries: Vec<LexEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MixedLexEntry {
    pub exact_key: u64,
    pub word_id: u32,
    pub reading_logp: f32,
}

#[derive(Debug, Clone, Default)]
pub struct MixedLexTable {
    pub keys: Vec<u64>,
    pub ranges: Vec<u32>,
    pub entries: Vec<MixedLexEntry>,
}

#[inline]
fn binary_search_u32(slice: &[u32], target: u32) -> Option<usize> {
    let mut size = slice.len();
    if size == 0 {
        return None;
    }
    let mut base = 0usize;
    while size > 1 {
        let half = size >> 1;
        let mid = base + half;
        // SAFETY: `mid < slice.len()` throughout the loop because `half < size` and `base + size <= slice.len()`.
        let v = unsafe { *slice.get_unchecked(mid) };
        base = if v <= target { mid } else { base };
        size -= half;
    }
    // SAFETY: `base < slice.len()` when non-empty.
    if unsafe { *slice.get_unchecked(base) } == target {
        Some(base)
    } else {
        None
    }
}

#[inline]
fn binary_search_u64(slice: &[u64], target: u64) -> Option<usize> {
    let mut size = slice.len();
    if size == 0 {
        return None;
    }
    let mut base = 0usize;
    while size > 1 {
        let half = size >> 1;
        let mid = base + half;
        // SAFETY: `mid < slice.len()` throughout the loop because `half < size` and `base + size <= slice.len()`.
        let v = unsafe { *slice.get_unchecked(mid) };
        base = if v <= target { mid } else { base };
        size -= half;
    }
    // SAFETY: `base < slice.len()` when non-empty.
    if unsafe { *slice.get_unchecked(base) } == target {
        Some(base)
    } else {
        None
    }
}

impl LexTable {
    pub fn lookup(&self, key: u64) -> &[LexEntry] {
        match binary_search_u64(&self.keys, key) {
            Some(idx) => {
                let start = self.ranges[idx] as usize;
                let end = self.ranges[idx + 1] as usize;
                &self.entries[start..end]
            }
            None => &[],
        }
    }

    fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        write_u32(w, self.keys.len() as u32)?;
        write_u32(w, self.entries.len() as u32)?;
        write_u64_slice(w, &self.keys)?;
        write_u32_slice(w, &self.ranges)?;
        for e in &self.entries {
            write_u32(w, e.word_id)?;
            write_f32(w, e.reading_logp)?;
        }
        Ok(())
    }

    fn read_from<R: Read>(r: &mut R) -> io::Result<Self> {
        let num_keys = read_u32(r)? as usize;
        let num_entries = read_u32(r)? as usize;
        let keys = read_u64_vec(r, num_keys)?;
        let ranges = read_u32_vec(r, num_keys + 1)?;
        if ranges.first().copied() != Some(0) || ranges.last().copied() != Some(num_entries as u32) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid LexTable ranges"));
        }
        let entries = read_lex_entries(r, num_entries)?;
        Ok(Self { keys, ranges, entries })
    }
}

/// Fast zero-allocation lookup index from UTF-8 word bytes to word ID.
#[derive(Debug, Clone)]
struct WordIndex {
    buckets: Vec<u32>,
    mask: usize,
}

impl WordIndex {
    fn build(word_offsets: &[u32], word_bytes: &[u8]) -> Self {
        let num_words = word_offsets.len().saturating_sub(1);
        let cap = (num_words.max(8) * 2).next_power_of_two();
        let mask = cap - 1;
        let mut buckets = vec![NONE_ID; cap];
        for id in FIRST_REAL_WORD_ID as usize..num_words {
            let start = word_offsets[id] as usize;
            let end = word_offsets[id + 1] as usize;
            let word = &word_bytes[start..end];
            if word.is_empty() {
                continue;
            }
            let mut slot = (fnv1a64(word) as usize) & mask;
            while buckets[slot] != NONE_ID {
                slot = (slot + 1) & mask;
            }
            buckets[slot] = id as u32;
        }
        Self { buckets, mask }
    }

    fn get(&self, key: &str, word_offsets: &[u32], word_bytes: &[u8]) -> Option<u32> {
        let kb = key.as_bytes();
        let mut slot = (fnv1a64(kb) as usize) & self.mask;
        loop {
            let id = self.buckets[slot];
            if id == NONE_ID {
                return None;
            }
            let idx = id as usize;
            let start = word_offsets[idx] as usize;
            let end = word_offsets[idx + 1] as usize;
            if &word_bytes[start..end] == kb {
                return Some(id);
            }
            slot = (slot + 1) & self.mask;
        }
    }
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

#[derive(Debug, Clone)]
pub struct BoundaryTail {
    pub tail_text: String,
    pub tail_chars: usize,
    pub pre_h1: u32,
    pub pre_h2: u32,
    pub tail_lm_score: f32,
}

#[derive(Debug, Clone)]
pub struct ContextState {
    pub h1: u32,
    pub h2: u32,
    pub has_context: bool,
    pub tail_joins: Vec<BoundaryTail>,
}

#[derive(Debug)]
pub struct Model {
    pub word_offsets: Vec<u32>,
    pub word_bytes: Vec<u8>,
    pub unigram_logp: Vec<f32>,
    pub unigram_bow: Vec<f32>,

    pub exact: LexTable,
    pub initial_chars: [Vec<LexEntry>; 26],
    pub abbrev_tail: LexTable,
    pub abbrev_init: LexTable,

    pub bigram_offsets: Vec<u32>,
    pub bigram_w2: Vec<u32>,
    pub bigram_logp: Vec<f32>,
    pub bigram_bow: Vec<f32>,

    pub trigram_offsets: Vec<u32>,
    pub trigram_w3: Vec<u32>,
    pub trigram_logp: Vec<f32>,

    word_index: WordIndex,
    /// Sorted by `word_id`: `(word_id, packed_exact_key, reading_logp)` for multi-char words (`len >= 2`).
    multi_word_readings: Vec<(u32, u64, f32)>,
    mixed_lex: MixedLexTable,
    protected_fn_mask: Vec<u8>,
    user_oov_unigram_logp: f32,
}

impl Model {
    #[allow(clippy::too_many_arguments)]
    pub fn from_parts(
        word_offsets: Vec<u32>,
        word_bytes: Vec<u8>,
        unigram_logp: Vec<f32>,
        unigram_bow: Vec<f32>,
        exact: LexTable,
        initial_chars: [Vec<LexEntry>; 26],
        abbrev_tail: LexTable,
        abbrev_init: LexTable,
        bigram_offsets: Vec<u32>,
        bigram_w2: Vec<u32>,
        bigram_logp: Vec<f32>,
        bigram_bow: Vec<f32>,
        trigram_offsets: Vec<u32>,
        trigram_w3: Vec<u32>,
        trigram_logp: Vec<f32>,
    ) -> Self {
        let word_index = WordIndex::build(&word_offsets, &word_bytes);
        let mut multi_word_readings = Vec::new();
        let mut mixed_items: Vec<(u64, MixedLexEntry)> = Vec::new();
        for (key_idx, &key) in exact.keys.iter().enumerate() {
            let syl_len = ((key >> 54) & 0x7) as usize;
            if syl_len >= 2 {
                let start = exact.ranges[key_idx] as usize;
                let end = exact.ranges[key_idx + 1] as usize;
                let init_key = if syl_len <= 4 {
                    let mut letters = [0u8; 4];
                    for (i, slot) in letters.iter_mut().enumerate().take(syl_len) {
                        let sid = ((key >> (i * 9)) & 0x1ff) as SyllableId;
                        *slot = syllable_first_letter(sid);
                    }
                    pack_initials(&letters[..syl_len])
                } else {
                    None
                };
                for entry in &exact.entries[start..end] {
                    multi_word_readings.push((entry.word_id, key, entry.reading_logp));
                    if let Some(ik) = init_key {
                        mixed_items.push((
                            ik,
                            MixedLexEntry {
                                exact_key: key,
                                word_id: entry.word_id,
                                reading_logp: entry.reading_logp,
                            },
                        ));
                    }
                }
            }
        }
        multi_word_readings.sort_unstable_by_key(|x| x.0);

        mixed_items.sort_unstable_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| {
                    let sa = unigram_logp.get(a.1.word_id as usize).copied().unwrap_or(-20.0)
                        + a.1.reading_logp;
                    let sb = unigram_logp.get(b.1.word_id as usize).copied().unwrap_or(-20.0)
                        + b.1.reading_logp;
                    sb.total_cmp(&sa)
                })
                .then_with(|| a.1.word_id.cmp(&b.1.word_id))
        });

        let mut mixed_keys = Vec::new();
        let mut mixed_ranges = vec![0u32];
        let mut mixed_entries = Vec::with_capacity(mixed_items.len());
        let mut cur_init_key: Option<u64> = None;
        for (init_key, entry) in mixed_items {
            if cur_init_key != Some(init_key) {
                if cur_init_key.is_some() {
                    mixed_ranges.push(mixed_entries.len() as u32);
                }
                cur_init_key = Some(init_key);
                mixed_keys.push(init_key);
            }
            mixed_entries.push(entry);
        }
        if cur_init_key.is_some() {
            mixed_ranges.push(mixed_entries.len() as u32);
        }
        let mixed_lex = MixedLexTable {
            keys: mixed_keys,
            ranges: mixed_ranges,
            entries: mixed_entries,
        };

        let mut real_unis: Vec<f32> = unigram_logp
            .iter()
            .skip(FIRST_REAL_WORD_ID as usize)
            .copied()
            .filter(|v| v.is_finite())
            .collect();
        let user_oov_unigram_logp = if real_unis.is_empty() {
            -12.0
        } else {
            real_unis.sort_by(|a, b| a.total_cmp(b));
            let idx = ((real_unis.len() - 1) * 90) / 100;
            real_unis[idx]
        };

        let num_words = word_offsets.len().saturating_sub(1);
        let mut protected_fn_mask = vec![0u8; num_words];
        for id in FIRST_REAL_WORD_ID as usize..num_words {
            let start = word_offsets[id] as usize;
            let end = word_offsets[id + 1] as usize;
            if let Ok(s) = std::str::from_utf8(&word_bytes[start..end]) {
                if PROTECTED_FUNCTION_WORDS.contains(&s) {
                    protected_fn_mask[id] = 1;
                }
            }
        }

        Self {
            word_offsets,
            word_bytes,
            unigram_logp,
            unigram_bow,
            exact,
            initial_chars,
            abbrev_tail,
            abbrev_init,
            bigram_offsets,
            bigram_w2,
            bigram_logp,
            bigram_bow,
            trigram_offsets,
            trigram_w3,
            trigram_logp,
            word_index,
            multi_word_readings,
            mixed_lex,
            protected_fn_mask,
            user_oov_unigram_logp,
        }
    }

    pub fn user_oov_unigram_logp(&self) -> f32 {
        self.user_oov_unigram_logp
    }

    pub fn oov_transition_logp(&self, prev_h2: u32) -> f32 {
        if prev_h2 == BOS_ID || prev_h2 == NONE_ID || (prev_h2 as usize) >= self.unigram_bow.len() {
            self.user_oov_unigram_logp
        } else {
            0.5 * self.unigram_bow[prev_h2 as usize] + self.user_oov_unigram_logp
        }
    }

    pub fn char_reading_logp(&self, ch: char, syl: SyllableId) -> Option<f32> {
        let mut buf = [0u8; 4];
        let s = ch.encode_utf8(&mut buf);
        let key = pack_exact(&[syl])?;
        for entry in self.exact.lookup(key) {
            if self.word_str(entry.word_id) == s {
                return Some(entry.reading_logp);
            }
        }
        None
    }

    pub fn num_words(&self) -> usize {
        self.unigram_logp.len()
    }

    #[inline]
    pub fn is_protected_function_word(&self, word_id: u32) -> bool {
        self.protected_fn_mask
            .get(word_id as usize)
            .copied()
            .unwrap_or(0)
            != 0
    }

    pub fn word_str(&self, word_id: u32) -> &str {
        let idx = word_id as usize;
        if idx + 1 >= self.word_offsets.len() {
            return "";
        }
        let start = self.word_offsets[idx] as usize;
        let end = self.word_offsets[idx + 1] as usize;
        std::str::from_utf8(&self.word_bytes[start..end]).unwrap_or("")
    }

    pub fn word_id(&self, word: &str) -> Option<u32> {
        self.word_index.get(word, &self.word_offsets, &self.word_bytes)
    }

    pub fn initial_entries(&self, letter_idx: u8) -> &[LexEntry] {
        self.initial_chars
            .get(letter_idx as usize)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn lookup_mixed(&self, units: &[DagEdgeKind], max_keep: usize, out: &mut Vec<LexEntry>) {
        out.clear();
        let len = units.len();
        if !(2..=4).contains(&len) || max_keep == 0 {
            return;
        }
        let mut letters = [0u8; 4];
        let mut exact_mask = 0u64;
        let mut exact_target = 0u64;
        let mut has_digraph = false;
        for (i, &unit) in units.iter().enumerate() {
            letters[i] = unit_first_letter(unit);
            match unit {
                DagEdgeKind::Exact(sid) => {
                    let shift = i * 9;
                    exact_mask |= 0x1ff_u64 << shift;
                    exact_target |= (sid as u64) << shift;
                }
                DagEdgeKind::Abbrev(_) => {}
                DagEdgeKind::Abbrev2(_) => {
                    has_digraph = true;
                }
            }
        }
        let Some(init_key) = pack_initials(&letters[..len]) else {
            return;
        };
        let Some(idx) = binary_search_u64(&self.mixed_lex.keys, init_key) else {
            return;
        };
        let start = self.mixed_lex.ranges[idx] as usize;
        let end = self.mixed_lex.ranges[idx + 1] as usize;
        for e in &self.mixed_lex.entries[start..end] {
            if (e.exact_key & exact_mask) != exact_target {
                continue;
            }
            if has_digraph {
                let mut ok = true;
                for (i, &unit) in units.iter().enumerate() {
                    if let DagEdgeKind::Abbrev2(di) = unit {
                        let sid = ((e.exact_key >> (i * 9)) & 0x1ff) as SyllableId;
                        if !syllable_matches_digraph(sid, di) {
                            ok = false;
                            break;
                        }
                    }
                }
                if !ok {
                    continue;
                }
            }
            if out.iter().any(|x| x.word_id == e.word_id) {
                continue;
            }
            out.push(LexEntry {
                word_id: e.word_id,
                reading_logp: e.reading_logp,
            });
            if out.len() >= max_keep {
                break;
            }
        }
    }

    #[inline]
    pub fn bigram_index(&self, w1: u32, w2: u32) -> Option<usize> {
        let idx = w1 as usize;
        if idx + 1 >= self.bigram_offsets.len() {
            return None;
        }
        let start = self.bigram_offsets[idx] as usize;
        let end = self.bigram_offsets[idx + 1] as usize;
        binary_search_u32(&self.bigram_w2[start..end], w2).map(|off| start + off)
    }

    #[inline]
    pub fn bigram_logp_known_uni(&self, w1: u32, w2: u32, uni_w2: f32) -> f32 {
        if w1 == NONE_ID || (w1 as usize) >= self.unigram_bow.len() {
            return uni_w2;
        }
        if let Some(bg) = self.bigram_index(w1, w2) {
            self.bigram_logp[bg]
        } else {
            self.unigram_bow[w1 as usize] + uni_w2
        }
    }

    pub fn bigram_logp(&self, w1: u32, w2: u32) -> f32 {
        let uni = self.unigram_logp.get(w2 as usize).copied().unwrap_or(-20.0);
        self.bigram_logp_known_uni(w1, w2, uni)
    }

    #[inline]
    pub fn trigram_meta(&self, w1: u32, w2: u32) -> (u32, u32, f32) {
        if w1 == NONE_ID || w2 == NONE_ID {
            return (0, 0, 0.0);
        }
        if let Some(bg) = self.bigram_index(w1, w2) {
            (
                self.trigram_offsets[bg],
                self.trigram_offsets[bg + 1],
                self.bigram_bow[bg],
            )
        } else {
            (0, 0, 0.0)
        }
    }

    #[inline]
    pub fn trigram_lookup_range(&self, tri_start: u32, tri_end: u32, w3: u32) -> Option<f32> {
        if tri_start >= tri_end {
            return None;
        }
        let s = tri_start as usize;
        let e = tri_end as usize;
        binary_search_u32(&self.trigram_w3[s..e], w3).map(|off| self.trigram_logp[s + off])
    }

    #[inline]
    pub fn trigram_logp_with_bg(&self, bg_opt: Option<usize>, w2: u32, w3: u32) -> f32 {
        if let Some(bg) = bg_opt {
            let start = self.trigram_offsets[bg] as usize;
            let end = self.trigram_offsets[bg + 1] as usize;
            if let Some(off) = binary_search_u32(&self.trigram_w3[start..end], w3) {
                return self.trigram_logp[start + off];
            }
            return self.bigram_bow[bg] + self.bigram_logp(w2, w3);
        }
        self.bigram_logp(w2, w3)
    }

    pub fn trigram_logp(&self, w1: u32, w2: u32, w3: u32) -> f32 {
        if w1 == NONE_ID {
            return self.bigram_logp(w2, w3);
        }
        if let Some(bg) = self.bigram_index(w1, w2) {
            let start = self.trigram_offsets[bg] as usize;
            let end = self.trigram_offsets[bg + 1] as usize;
            if let Some(off) = binary_search_u32(&self.trigram_w3[start..end], w3) {
                return self.trigram_logp[start + off];
            }
            return self.bigram_bow[bg] + self.bigram_logp(w2, w3);
        }
        self.bigram_logp(w2, w3)
    }

    /// Segment a pure-Hanzi string into word IDs using bigram maximum-probability DP.
    pub fn segment_hanzi_ids(&self, hanzi: &str, init_prev: u32) -> Vec<u32> {
        if hanzi.is_empty() {
            return Vec::new();
        }
        let mut byte_pos = Vec::with_capacity(hanzi.chars().count() + 1);
        for (idx, _) in hanzi.char_indices() {
            byte_pos.push(idx);
        }
        byte_pos.push(hanzi.len());
        let n = byte_pos.len() - 1;

        #[derive(Clone, Copy)]
        struct DpNode {
            score: f32,
            prev_pos: usize,
            word_id: u32,
        }
        let mut dp = vec![
            DpNode {
                score: f32::NEG_INFINITY,
                prev_pos: 0,
                word_id: NONE_ID,
            };
            n + 1
        ];
        dp[0].score = 0.0;
        dp[0].word_id = init_prev;

        for i in 0..n {
            let base = dp[i].score;
            if !base.is_finite() {
                continue;
            }
            let prev_w = dp[i].word_id;
            let max_len = 6.min(n - i);
            for len in 1..=max_len {
                let slice = &hanzi[byte_pos[i]..byte_pos[i + len]];
                if let Some(w) = self.word_id(slice) {
                    let cand = base + self.bigram_logp(prev_w, w);
                    if cand > dp[i + len].score {
                        dp[i + len] = DpNode {
                            score: cand,
                            prev_pos: i,
                            word_id: w,
                        };
                    }
                } else if len == 1 {
                    let cand = base - 35.0;
                    if cand > dp[i + 1].score {
                        dp[i + 1] = DpNode {
                            score: cand,
                            prev_pos: i,
                            word_id: NONE_ID,
                        };
                    }
                }
            }
        }

        let mut rev = Vec::new();
        let mut cur = n;
        while cur > 0 {
            rev.push(dp[cur].word_id);
            cur = dp[cur].prev_pos;
        }
        rev.reverse();
        rev
    }

    pub fn match_joined_reading(
        &self,
        word_id: u32,
        tail_chars: usize,
        pron: &SpanPron,
    ) -> Option<f32> {
        let start = self.multi_word_readings.partition_point(|x| x.0 < word_id);
        let mut best: Option<f32> = None;
        for &(id, key, reading_logp) in &self.multi_word_readings[start..] {
            if id != word_id {
                break;
            }
            let total_len = ((key >> 54) & 0x7) as usize;
            let mut word_syls = [0u16; 6];
            for (i, slot) in word_syls.iter_mut().enumerate().take(total_len.min(6)) {
                *slot = ((key >> (i * 9)) & 0x1ff) as u16;
            }
            let matches = match *pron {
                SpanPron::Exact { len, syls } => {
                    let slen = len as usize;
                    total_len == tail_chars + slen
                        && (0..slen).all(|j| word_syls[tail_chars + j] == syls[j])
                }
                SpanPron::AbbrevTail {
                    prefix_len,
                    prefix,
                    last_letter,
                } => {
                    let plen = prefix_len as usize;
                    total_len == tail_chars + plen + 1
                        && (0..plen).all(|j| word_syls[tail_chars + j] == prefix[j])
                        && syllable_first_letter(word_syls[total_len - 1]) == last_letter
                }
                SpanPron::AbbrevInit { len, letters } => {
                    let slen = len as usize;
                    total_len == tail_chars + slen
                        && (0..slen)
                            .all(|j| syllable_first_letter(word_syls[tail_chars + j]) == letters[j])
                }
                SpanPron::Mixed { len, units } => {
                    let slen = len as usize;
                    total_len == tail_chars + slen
                        && (0..slen)
                            .all(|j| syllable_matches_unit(word_syls[tail_chars + j], units[j]))
                }
            };
            if matches {
                best = Some(best.map_or(reading_logp, |b| b.max(reading_logp)));
            }
        }
        best
    }

    fn score_token_seq(&self, ids: &[u32], boundary_prev: u32) -> f32 {
        let mut score = 0.0_f32;
        let mut h1 = NONE_ID;
        let mut h2 = boundary_prev;
        for &w in ids {
            if w == NONE_ID {
                score -= 35.0;
            } else {
                score += self.trigram_logp(h1, h2, w);
            }
            h1 = h2;
            h2 = w;
        }
        score
    }

    /// Extract rich context state (including inline wrappers and pre-caret boundary join tails).
    pub fn context_state(&self, context_text: &str) -> ContextState {
        let trimmed = context_text.trim_end();
        let ended_with_ws = trimmed.len() != context_text.len();
        if trimmed.is_empty() {
            return ContextState {
                h1: NONE_ID,
                h2: BOS_ID,
                has_context: false,
                tail_joins: Vec::new(),
            };
        }

        // Split off any trailing non-Hanzi characters.
        let mut split_byte = trimmed.len();
        for (idx, ch) in trimmed.char_indices().rev() {
            if is_hanzi(ch) {
                break;
            }
            split_byte = idx;
        }
        let trailing = &trimmed[split_byte..];
        let before = &trimmed[..split_byte];

        if !trailing.is_empty() {
            if trailing.chars().any(is_sentence_end) {
                return ContextState {
                    h1: NONE_ID,
                    h2: BOS_ID,
                    has_context: false,
                    tail_joins: Vec::new(),
                };
            }
            if trailing.chars().any(is_pause_punct) {
                let (clause, boundary_prev) = trailing_hanzi_clause(before, 16);
                if clause.is_empty() {
                    return ContextState {
                        h1: NONE_ID,
                        h2: PAUSE_ID,
                        has_context: true,
                        tail_joins: Vec::new(),
                    };
                }
                let ids = self.segment_hanzi_ids(clause, boundary_prev);
                let h1 = match ids.last().copied() {
                    Some(w) if w != NONE_ID => w,
                    _ => NONE_ID,
                };
                return ContextState {
                    h1,
                    h2: PAUSE_ID,
                    has_context: true,
                    tail_joins: Vec::new(),
                };
            }
            if trailing.chars().all(is_inline_wrapper) {
                let (clause, boundary_prev) = trailing_hanzi_clause(before, 16);
                if clause.is_empty() {
                    return ContextState {
                        h1: NONE_ID,
                        h2: BOS_ID,
                        has_context: false,
                        tail_joins: Vec::new(),
                    };
                }
                let ids = self.segment_hanzi_ids(clause, boundary_prev);
                let (h1, h2) = history_from_ids(&ids, boundary_prev);
                return ContextState {
                    h1,
                    h2,
                    has_context: true,
                    tail_joins: Vec::new(),
                };
            }
            return ContextState {
                h1: NONE_ID,
                h2: BOS_ID,
                has_context: false,
                tail_joins: Vec::new(),
            };
        }

        let (clause, boundary_prev) = trailing_hanzi_clause(before, 16);
        if clause.is_empty() {
            return ContextState {
                h1: NONE_ID,
                h2: BOS_ID,
                has_context: false,
                tail_joins: Vec::new(),
            };
        }
        let full_ids = self.segment_hanzi_ids(clause, boundary_prev);
        let (h1, h2) = history_from_ids(&full_ids, boundary_prev);

        let mut tail_joins = Vec::new();
        if !ended_with_ws {
            let mut char_bytes = Vec::with_capacity(clause.chars().count() + 1);
            for (idx, _) in clause.char_indices() {
                char_bytes.push(idx);
            }
            char_bytes.push(clause.len());
            let n = char_bytes.len() - 1;
            let full_score = self.score_token_seq(&full_ids, boundary_prev);
            for k in 1..=3.min(n) {
                let split = char_bytes[n - k];
                let prefix = &clause[..split];
                let tail_text = clause[split..].to_owned();
                let prefix_ids = self.segment_hanzi_ids(prefix, boundary_prev);
                let prefix_score = self.score_token_seq(&prefix_ids, boundary_prev);
                let (pre_h1, pre_h2) = history_from_ids(&prefix_ids, boundary_prev);
                tail_joins.push(BoundaryTail {
                    tail_text,
                    tail_chars: k,
                    pre_h1,
                    pre_h2,
                    tail_lm_score: full_score - prefix_score,
                });
            }
        }

        ContextState {
            h1,
            h2,
            has_context: true,
            tail_joins,
        }
    }

    /// Extract the trailing `(h1, h2)` word-ID history from preceding text.
    pub fn context_history(&self, context_text: &str) -> (u32, u32) {
        let state = self.context_state(context_text);
        (state.h1, state.h2)
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        let file = File::create(path)?;
        let mut w = BufWriter::new(file);
        w.write_all(MAGIC)?;
        write_u32(&mut w, self.num_words() as u32)?;
        write_u32(&mut w, self.word_bytes.len() as u32)?;
        write_u32(&mut w, self.bigram_w2.len() as u32)?;
        write_u32(&mut w, self.trigram_w3.len() as u32)?;

        write_u32_slice(&mut w, &self.word_offsets)?;
        w.write_all(&self.word_bytes)?;
        write_f32_slice(&mut w, &self.unigram_logp)?;
        write_f32_slice(&mut w, &self.unigram_bow)?;

        self.exact.write_to(&mut w)?;
        for bucket in &self.initial_chars {
            write_u32(&mut w, bucket.len() as u32)?;
            for e in bucket {
                write_u32(&mut w, e.word_id)?;
                write_f32(&mut w, e.reading_logp)?;
            }
        }
        self.abbrev_tail.write_to(&mut w)?;
        self.abbrev_init.write_to(&mut w)?;

        write_u32_slice(&mut w, &self.bigram_offsets)?;
        write_u32_slice(&mut w, &self.bigram_w2)?;
        write_f32_slice(&mut w, &self.bigram_logp)?;
        write_f32_slice(&mut w, &self.bigram_bow)?;

        write_u32_slice(&mut w, &self.trigram_offsets)?;
        write_u32_slice(&mut w, &self.trigram_w3)?;
        write_f32_slice(&mut w, &self.trigram_logp)?;
        w.flush()
    }

    pub fn load(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let mut r = BufReader::with_capacity(1 << 20, file);
        let mut magic = [0u8; 8];
        r.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid model magic header",
            ));
        }

        let num_words = read_u32(&mut r)? as usize;
        let word_bytes_len = read_u32(&mut r)? as usize;
        let num_bigrams = read_u32(&mut r)? as usize;
        let num_trigrams = read_u32(&mut r)? as usize;

        let word_offsets = read_u32_vec(&mut r, num_words + 1)?;
        let mut word_bytes = vec![0u8; word_bytes_len];
        r.read_exact(&mut word_bytes)?;
        let unigram_logp = read_f32_vec(&mut r, num_words)?;
        let unigram_bow = read_f32_vec(&mut r, num_words)?;

        let exact = LexTable::read_from(&mut r)?;
        let mut initial_chars: [Vec<LexEntry>; 26] = std::array::from_fn(|_| Vec::new());
        for bucket in &mut initial_chars {
            let len = read_u32(&mut r)? as usize;
            *bucket = read_lex_entries(&mut r, len)?;
        }
        let abbrev_tail = LexTable::read_from(&mut r)?;
        let abbrev_init = LexTable::read_from(&mut r)?;

        let bigram_offsets = read_u32_vec(&mut r, num_words + 1)?;
        let bigram_w2 = read_u32_vec(&mut r, num_bigrams)?;
        let bigram_logp = read_f32_vec(&mut r, num_bigrams)?;
        let bigram_bow = read_f32_vec(&mut r, num_bigrams)?;

        let trigram_offsets = read_u32_vec(&mut r, num_bigrams + 1)?;
        let trigram_w3 = read_u32_vec(&mut r, num_trigrams)?;
        let trigram_logp = read_f32_vec(&mut r, num_trigrams)?;

        Ok(Self::from_parts(
            word_offsets,
            word_bytes,
            unigram_logp,
            unigram_bow,
            exact,
            initial_chars,
            abbrev_tail,
            abbrev_init,
            bigram_offsets,
            bigram_w2,
            bigram_logp,
            bigram_bow,
            trigram_offsets,
            trigram_w3,
            trigram_logp,
        ))
    }
}

pub(crate) fn is_hanzi(ch: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&ch)
}

fn is_sentence_end(ch: char) -> bool {
    matches!(ch, '。' | '？' | '！' | '?' | '!')
}

fn is_pause_punct(ch: char) -> bool {
    matches!(ch, '，' | '、' | '；' | '：' | ',' | ';' | '—' | '…')
}

fn is_inline_wrapper(ch: char) -> bool {
    matches!(
        ch,
        '“' | '”'
            | '‘'
            | '’'
            | '「'
            | '」'
            | '『'
            | '』'
            | '《'
            | '》'
            | '（'
            | '）'
            | '【'
            | '】'
            | '('
            | ')'
            | '['
            | ']'
            | '"'
            | '\''
    )
}

fn history_from_ids(ids: &[u32], boundary_prev: u32) -> (u32, u32) {
    match ids {
        [] => (NONE_ID, boundary_prev),
        [w] => (boundary_prev, *w),
        [.., w1, w2] => (*w1, *w2),
    }
}

fn wrapper_boundary_prev(before_wrapper: &str) -> u32 {
    for ch in before_wrapper.chars().rev() {
        if is_sentence_end(ch) {
            return BOS_ID;
        }
        if is_hanzi(ch) {
            return NONE_ID;
        }
    }
    BOS_ID
}

/// Returns `(trailing_hanzi_up_to_max_chars, boundary_prev_id)` where `boundary_prev_id` is
/// `PAUSE_ID` after pause punctuation, `NONE_ID` inside mid-sentence wrappers, or `BOS_ID`.
fn trailing_hanzi_clause(text: &str, max_chars: usize) -> (&str, u32) {
    let mut start_byte = text.len();
    let mut count = 0usize;
    let mut boundary_prev = BOS_ID;
    for (idx, ch) in text.char_indices().rev() {
        if is_hanzi(ch) {
            start_byte = idx;
            count += 1;
            if count >= max_chars {
                if let Some(prev_ch) = text[..start_byte].chars().next_back() {
                    if is_pause_punct(prev_ch) {
                        boundary_prev = PAUSE_ID;
                    } else if is_inline_wrapper(prev_ch) {
                        let prev_end = start_byte - prev_ch.len_utf8();
                        boundary_prev = wrapper_boundary_prev(&text[..prev_end]);
                    }
                }
                break;
            }
        } else {
            if is_pause_punct(ch) {
                boundary_prev = PAUSE_ID;
            } else if is_inline_wrapper(ch) {
                boundary_prev = wrapper_boundary_prev(&text[..idx]);
            }
            break;
        }
    }
    (&text[start_byte..], boundary_prev)
}

fn write_u32<W: Write>(w: &mut W, v: u32) -> io::Result<()> {
    w.write_all(&v.to_le_bytes())
}

fn write_f32<W: Write>(w: &mut W, v: f32) -> io::Result<()> {
    w.write_all(&v.to_le_bytes())
}

fn write_u32_slice<W: Write>(w: &mut W, s: &[u32]) -> io::Result<()> {
    for &v in s {
        w.write_all(&v.to_le_bytes())?;
    }
    Ok(())
}

fn write_u64_slice<W: Write>(w: &mut W, s: &[u64]) -> io::Result<()> {
    for &v in s {
        w.write_all(&v.to_le_bytes())?;
    }
    Ok(())
}

fn write_f32_slice<W: Write>(w: &mut W, s: &[f32]) -> io::Result<()> {
    for &v in s {
        w.write_all(&v.to_le_bytes())?;
    }
    Ok(())
}

fn read_u32<R: Read>(r: &mut R) -> io::Result<u32> {
    let mut buf = [0u8; 4];
    r.read_exact(&mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

fn read_u32_vec<R: Read>(r: &mut R, len: usize) -> io::Result<Vec<u32>> {
    let mut raw = vec![0u8; len * 4];
    r.read_exact(&mut raw)?;
    Ok(raw
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect())
}

fn read_u64_vec<R: Read>(r: &mut R, len: usize) -> io::Result<Vec<u64>> {
    let mut raw = vec![0u8; len * 8];
    r.read_exact(&mut raw)?;
    Ok(raw
        .chunks_exact(8)
        .map(|c| u64::from_le_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]))
        .collect())
}

fn read_f32_vec<R: Read>(r: &mut R, len: usize) -> io::Result<Vec<f32>> {
    let mut raw = vec![0u8; len * 4];
    r.read_exact(&mut raw)?;
    Ok(raw
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect())
}

fn read_lex_entries<R: Read>(r: &mut R, len: usize) -> io::Result<Vec<LexEntry>> {
    let mut raw = vec![0u8; len * 8];
    r.read_exact(&mut raw)?;
    Ok(raw
        .chunks_exact(8)
        .map(|c| LexEntry {
            word_id: u32::from_le_bytes([c[0], c[1], c[2], c[3]]),
            reading_logp: f32::from_le_bytes([c[4], c[5], c[6], c[7]]),
        })
        .collect())
}

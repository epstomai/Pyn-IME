//! Offline training tool: imports Wanxiang dictionaries + cleaned corpora,
//! runs dictionary DP segmentation, fits an interpolated Kneser-Ney trigram LM,
//! and exports `models/ime_model.bin`.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::Instant;

use ime_core::model::{
    LexEntry, LexTable, Model, BOS_ID, EOS_ID, FIRST_REAL_WORD_ID, NONE_ID, PAUSE_ID,
};
use ime_core::pinyin::{
    pack_abbrev_tail, pack_exact, pack_initials, syllable_first_letter, syllable_id, SyllableId,
};

const PROTECTED_FUNCTION_WORDS: &[&str] = &[
    "的", "得", "地", "了", "着", "过", "吗", "呢", "吧", "啊", "呀", "啦", "嘛", "哦", "么",
];

struct TrainArgs {
    dicts_dir: PathBuf,
    allowed_chars_path: Option<PathBuf>,
    corpora: Vec<PathBuf>,
    lexicon_path: Option<PathBuf>,
    out_path: PathBuf,
    min_bigram: u32,
    min_trigram: u32,
    max_bigrams: usize,
    max_trigrams: usize,
    max_multi_words: usize,
    shards: usize,
    resegment: bool,
}

fn parse_args() -> Result<TrainArgs, String> {
    let mut dicts_dir: Option<PathBuf> = None;
    let mut allowed_chars_path: Option<PathBuf> = None;
    let mut corpora: Vec<PathBuf> = Vec::new();
    let mut lexicon_path: Option<PathBuf> = None;
    let mut out_path: Option<PathBuf> = None;
    let mut min_bigram = 2u32;
    let mut min_trigram = 3u32;
    let mut max_bigrams = 7_000_000usize;
    let mut max_trigrams = 7_000_000usize;
    let mut max_multi_words = 420_000usize;
    let mut shards = 8usize;
    let mut resegment = false;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dicts" => dicts_dir = Some(PathBuf::from(args.next().ok_or("--dicts needs path")?)),
            "--allowed-chars" => {
                allowed_chars_path =
                    Some(PathBuf::from(args.next().ok_or("--allowed-chars needs path")?))
            }
            "--corpus" => corpora.push(PathBuf::from(args.next().ok_or("--corpus needs path")?)),
            "--lexicon" => lexicon_path = Some(PathBuf::from(args.next().ok_or("--lexicon needs path")?)),
            "--out" => out_path = Some(PathBuf::from(args.next().ok_or("--out needs path")?)),
            "--min-bigram" => min_bigram = args.next().ok_or("--min-bigram")?.parse().map_err(|_| "bad --min-bigram")?,
            "--min-trigram" => min_trigram = args.next().ok_or("--min-trigram")?.parse().map_err(|_| "bad --min-trigram")?,
            "--max-bigrams" => max_bigrams = args.next().ok_or("--max-bigrams")?.parse().map_err(|_| "bad --max-bigrams")?,
            "--max-trigrams" => max_trigrams = args.next().ok_or("--max-trigrams")?.parse().map_err(|_| "bad --max-trigrams")?,
            "--max-multi-words" => max_multi_words = args.next().ok_or("--max-multi-words")?.parse().map_err(|_| "bad --max-multi-words")?,
            "--shards" => shards = args.next().ok_or("--shards")?.parse::<usize>().map_err(|_| "bad --shards")?.max(1),
            "--resegment" => resegment = true,
            other => return Err(format!("unknown arg: {other}")),
        }
    }

    Ok(TrainArgs {
        dicts_dir: dicts_dir.ok_or("--dicts is required")?,
        allowed_chars_path,
        corpora: if corpora.is_empty() {
            return Err("at least one --corpus is required".into());
        } else {
            corpora
        },
        lexicon_path,
        out_path: out_path.ok_or("--out is required")?,
        min_bigram,
        min_trigram,
        max_bigrams,
        max_trigrams,
        max_multi_words,
        shards,
        resegment,
    })
}

/// Fast open-addressing u64 -> u32 counter/map with significantly lower memory than std HashMap.
struct U64Map {
    keys: Vec<u64>,
    vals: Vec<u32>,
    mask: usize,
    len: usize,
}

impl U64Map {
    fn with_capacity(expected_items: usize) -> Self {
        let cap = (expected_items.max(16) * 2).next_power_of_two();
        Self {
            keys: vec![u64::MAX; cap],
            vals: vec![0; cap],
            mask: cap - 1,
            len: 0,
        }
    }

    fn clear(&mut self) {
        self.keys.fill(u64::MAX);
        self.vals.fill(0);
        self.len = 0;
    }

    fn grow(&mut self) {
        let new_cap = self.keys.len() * 2;
        let new_mask = new_cap - 1;
        let mut new_keys = vec![u64::MAX; new_cap];
        let mut new_vals = vec![0u32; new_cap];
        for i in 0..self.keys.len() {
            let k = self.keys[i];
            if k != u64::MAX {
                let mut slot = (mix64(k) as usize) & new_mask;
                while new_keys[slot] != u64::MAX {
                    slot = (slot + 1) & new_mask;
                }
                new_keys[slot] = k;
                new_vals[slot] = self.vals[i];
            }
        }
        self.keys = new_keys;
        self.vals = new_vals;
        self.mask = new_mask;
    }

    #[inline]
    fn inc(&mut self, key: u64) {
        if (self.len + 1) * 10 >= self.keys.len() * 7 {
            self.grow();
        }
        let mut slot = (mix64(key) as usize) & self.mask;
        loop {
            let cur = self.keys[slot];
            if cur == key {
                self.vals[slot] = self.vals[slot].saturating_add(1);
                return;
            }
            if cur == u64::MAX {
                self.keys[slot] = key;
                self.vals[slot] = 1;
                self.len += 1;
                return;
            }
            slot = (slot + 1) & self.mask;
        }
    }

    #[inline]
    fn insert(&mut self, key: u64, val: u32) {
        if (self.len + 1) * 10 >= self.keys.len() * 7 {
            self.grow();
        }
        let mut slot = (mix64(key) as usize) & self.mask;
        loop {
            let cur = self.keys[slot];
            if cur == key {
                self.vals[slot] = val;
                return;
            }
            if cur == u64::MAX {
                self.keys[slot] = key;
                self.vals[slot] = val;
                self.len += 1;
                return;
            }
            slot = (slot + 1) & self.mask;
        }
    }

    #[inline]
    fn get(&self, key: u64) -> Option<u32> {
        let mut slot = (mix64(key) as usize) & self.mask;
        loop {
            let cur = self.keys[slot];
            if cur == key {
                return Some(self.vals[slot]);
            }
            if cur == u64::MAX {
                return None;
            }
            slot = (slot + 1) & self.mask;
        }
    }

    fn iter(&self) -> impl Iterator<Item = (u64, u32)> + '_ {
        self.keys
            .iter()
            .zip(self.vals.iter())
            .filter_map(|(&k, &v)| if k != u64::MAX { Some((k, v)) } else { None })
    }
}

#[inline]
fn mix64(mut x: u64) -> u64 {
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51afd7ed558ccd);
    x ^= x >> 33;
    x = x.wrapping_mul(0xc4ceb9fe1a85ec53);
    x ^= x >> 33;
    x
}

fn is_hanzi(ch: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&ch)
}

fn strip_tones(raw: &str, out: &mut String) {
    out.clear();
    for ch in raw.chars() {
        let mapped = match ch {
            'ā' | 'á' | 'ǎ' | 'à' => Some('a'),
            'ē' | 'é' | 'ě' | 'è' | 'ê' | 'ế' | 'ề' => Some('e'),
            'ī' | 'í' | 'ǐ' | 'ì' => Some('i'),
            'ō' | 'ó' | 'ǒ' | 'ò' => Some('o'),
            'ū' | 'ú' | 'ǔ' | 'ù' => Some('u'),
            'ǖ' | 'ǘ' | 'ǚ' | 'ǜ' | 'ü' => Some('v'),
            'ń' | 'ň' | 'ǹ' => Some('n'),
            'ḿ' => Some('m'),
            '\u{0308}' => {
                if out.ends_with('u') {
                    out.pop();
                    out.push('v');
                }
                None
            }
            '\u{0300}'..='\u{036f}' => None,
            c if c.is_ascii_alphabetic() => Some(c.to_ascii_lowercase()),
            _ => Some(ch),
        };
        if let Some(c) = mapped {
            out.push(c);
        }
    }
}

fn parse_syllables(raw_py: &str, expected_chars: usize, buf: &mut String) -> Option<Vec<SyllableId>> {
    let mut syls = Vec::with_capacity(expected_chars);
    for token in raw_py.split_whitespace() {
        strip_tones(token, buf);
        let id = syllable_id(buf)?;
        syls.push(id);
    }
    if syls.len() == expected_chars {
        Some(syls)
    } else {
        None
    }
}

struct DictData {
    // Words sorted deterministically: 0..3 special, 3.. real words
    word_offsets: Vec<u32>,
    word_bytes: Vec<u8>,
    dict_weights: Vec<f64>,
    readings: HashMap<u32, Vec<(Vec<SyllableId>, f64)>>,
}

fn load_allowed_chars(path: &Path) -> std::io::Result<HashSet<char>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut set = HashSet::new();
    for line in reader.lines() {
        let w = line?;
        if let Some(ch) = w.trim().chars().next() {
            set.insert(ch);
        }
    }
    Ok(set)
}

fn load_lexicon_filter(path: &Path) -> std::io::Result<HashSet<String>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut set = HashSet::new();
    for line in reader.lines() {
        let w = line?;
        let w = w.trim();
        if !w.is_empty() {
            set.insert(w.to_owned());
        }
    }
    Ok(set)
}

fn load_wanxiang_dicts(
    dicts_dir: &Path,
    max_multi_words: usize,
    lexicon: Option<&HashSet<String>>,
    allowed_chars: Option<&HashSet<char>>,
) -> std::io::Result<DictData> {
    // Accumulate word -> map<Vec<SyllableId>, u64 weight>
    let mut single_chars: HashMap<String, HashMap<Vec<SyllableId>, u64>> = HashMap::new();
    let mut multi_words: HashMap<String, HashMap<Vec<SyllableId>, u64>> = HashMap::new();
    let mut protected_duoyin: HashSet<String> = HashSet::new();

    let files = [
        ("zi.dict.yaml", true, false, false),
        ("duoyin.dict.yaml", false, true, false),
        ("jichu.dict.yaml", false, false, false),
        ("diming.dict.yaml", false, false, true),
        ("mingren.dict.yaml", false, false, true),
        ("yiren.dict.yaml", false, false, true),
    ];

    let mut scratch = String::with_capacity(32);
    for (filename, is_zi, is_duoyin, is_proper) in files {
        let path = dicts_dir.join(filename);
        let file = File::open(&path)?;
        let reader = BufReader::with_capacity(1 << 20, file);
        let mut in_body = false;

        for line in reader.lines() {
            let line = line?;
            if line == "..." {
                in_body = true;
                continue;
            }
            if !in_body || line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.split('\t');
            let Some(word) = parts.next() else { continue };
            let Some(py_raw) = parts.next() else { continue };
            let weight: u64 = parts
                .next()
                .and_then(|s| s.trim().parse::<u64>().ok())
                .unwrap_or(1)
                .max(1);

            let char_len = word.chars().count();
            if char_len == 0 || char_len > 6 || !word.chars().all(is_hanzi) {
                continue;
            }
            if let Some(allowed) = allowed_chars {
                if !word.chars().all(|c| allowed.contains(&c)) {
                    continue;
                }
            }
            if is_zi && char_len != 1 {
                continue;
            }
            if !is_zi && (char_len < 2 || char_len > 6) {
                continue;
            }
            if !is_zi {
                if let Some(lex) = lexicon {
                    let allowed = lex.contains(word) || (is_proper && weight >= 1000);
                    if !allowed {
                        continue;
                    }
                }
            }

            let Some(syls) = parse_syllables(py_raw, char_len, &mut scratch) else {
                continue;
            };

            if is_zi {
                let entry = single_chars.entry(word.to_owned()).or_default();
                *entry.entry(syls).or_insert(0) += weight;
            } else {
                if is_duoyin {
                    protected_duoyin.insert(word.to_owned());
                }
                let entry = multi_words.entry(word.to_owned()).or_default();
                *entry.entry(syls).or_insert(0) += weight;
            }
        }
    }

    // Select all single chars + duoyin + top `max_multi_words` multi-char words by total weight.
    let mut multi_list: Vec<(String, u64, HashMap<Vec<SyllableId>, u64>)> = multi_words
        .into_iter()
        .map(|(w, map)| {
            let total: u64 = map.values().sum();
            (w, total, map)
        })
        .collect();
    multi_list.sort_by(|a, b| {
        let ap = protected_duoyin.contains(&a.0);
        let bp = protected_duoyin.contains(&b.0);
        bp.cmp(&ap).then_with(|| b.1.cmp(&a.1)).then_with(|| a.0.cmp(&b.0))
    });
    if multi_list.len() > max_multi_words {
        multi_list.truncate(max_multi_words);
    }

    let mut single_list: Vec<(String, u64, HashMap<Vec<SyllableId>, u64>)> = single_chars
        .into_iter()
        .map(|(w, map)| {
            let total: u64 = map.values().sum();
            (w, total, map)
        })
        .collect();
    single_list.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let mut word_offsets = Vec::with_capacity(3 + single_list.len() + multi_list.len() + 1);
    let mut word_bytes = Vec::new();
    let mut dict_weights = Vec::with_capacity(3 + single_list.len() + multi_list.len());
    let mut readings = HashMap::new();

    for special in ["<bos>", "<eos>", "<pause>"] {
        word_offsets.push(word_bytes.len() as u32);
        word_bytes.extend_from_slice(special.as_bytes());
        dict_weights.push(1.0);
    }

    for (w, total, map) in single_list.into_iter().chain(multi_list.into_iter()) {
        let id = dict_weights.len() as u32;
        word_offsets.push(word_bytes.len() as u32);
        word_bytes.extend_from_slice(w.as_bytes());
        dict_weights.push(total as f64);
        let vec_r: Vec<(Vec<SyllableId>, f64)> =
            map.into_iter().map(|(s, wt)| (s, wt as f64)).collect();
        readings.insert(id, vec_r);
    }
    word_offsets.push(word_bytes.len() as u32);

    Ok(DictData {
        word_offsets,
        word_bytes,
        dict_weights,
        readings,
    })
}

struct VocabLookup {
    buckets: Vec<u32>,
    mask: usize,
}

impl VocabLookup {
    fn build(word_offsets: &[u32], word_bytes: &[u8]) -> Self {
        let n = word_offsets.len().saturating_sub(1);
        let cap = (n.max(16) * 2).next_power_of_two();
        let mask = cap - 1;
        let mut buckets = vec![NONE_ID; cap];
        for id in FIRST_REAL_WORD_ID as usize..n {
            let bytes = &word_bytes[word_offsets[id] as usize..word_offsets[id + 1] as usize];
            let mut slot = (fnv1a64(bytes) as usize) & mask;
            while buckets[slot] != NONE_ID {
                slot = (slot + 1) & mask;
            }
            buckets[slot] = id as u32;
        }
        Self { buckets, mask }
    }

    #[inline]
    fn get(&self, word: &str, word_offsets: &[u32], word_bytes: &[u8]) -> Option<u32> {
        let kb = word.as_bytes();
        let mut slot = (fnv1a64(kb) as usize) & self.mask;
        loop {
            let id = self.buckets[slot];
            if id == NONE_ID {
                return None;
            }
            let idx = id as usize;
            if &word_bytes[word_offsets[idx] as usize..word_offsets[idx + 1] as usize] == kb {
                return Some(id);
            }
            slot = (slot + 1) & self.mask;
        }
    }
}

#[inline]
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

enum SegScorer<'a> {
    Unigram(&'a [f32]),
    Bigram {
        unigram_logp: &'a [f32],
        unigram_bow: &'a [f32],
        bigram_offsets: &'a [u32],
        bigram_w2: &'a [u32],
        bigram_logp: &'a [f32],
    },
}

impl<'a> SegScorer<'a> {
    #[inline]
    fn step_score(&self, prev_w: u32, w: u32) -> f32 {
        match *self {
            SegScorer::Unigram(dict_logp) => dict_logp[w as usize],
            SegScorer::Bigram {
                unigram_logp,
                unigram_bow,
                bigram_offsets,
                bigram_w2,
                bigram_logp,
            } => {
                let uni = unigram_logp.get(w as usize).copied().unwrap_or(-20.0);
                if prev_w == NONE_ID || (prev_w as usize) >= unigram_bow.len() {
                    return uni;
                }
                let start = bigram_offsets[prev_w as usize] as usize;
                let end = bigram_offsets[prev_w as usize + 1] as usize;
                if let Ok(off) = bigram_w2[start..end].binary_search(&w) {
                    bigram_logp[start + off]
                } else {
                    unigram_bow[prev_w as usize] + uni
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn segment_clause(
    clause: &str,
    init_prev: u32,
    lookup: &VocabLookup,
    word_offsets: &[u32],
    word_bytes: &[u8],
    scorer: &SegScorer<'_>,
    out_tokens: &mut Vec<u32>,
    byte_pos: &mut Vec<usize>,
    dp_scores: &mut Vec<f32>,
    dp_prev: &mut Vec<usize>,
    dp_word: &mut Vec<u32>,
) {
    byte_pos.clear();
    for (idx, _) in clause.char_indices() {
        byte_pos.push(idx);
    }
    byte_pos.push(clause.len());
    let n = byte_pos.len() - 1;
    if n == 0 {
        return;
    }

    dp_scores.clear();
    dp_scores.resize(n + 1, f32::NEG_INFINITY);
    dp_prev.clear();
    dp_prev.resize(n + 1, 0);
    dp_word.clear();
    dp_word.resize(n + 1, NONE_ID);
    dp_scores[0] = 0.0;
    dp_word[0] = init_prev;

    for i in 0..n {
        let base = dp_scores[i];
        if !base.is_finite() {
            continue;
        }
        let prev_w = dp_word[i];
        let max_len = 6.min(n - i);
        for len in 1..=max_len {
            let slice = &clause[byte_pos[i]..byte_pos[i + len]];
            if let Some(id) = lookup.get(slice, word_offsets, word_bytes) {
                let cand = base + scorer.step_score(prev_w, id);
                if cand > dp_scores[i + len] {
                    dp_scores[i + len] = cand;
                    dp_prev[i + len] = i;
                    dp_word[i + len] = id;
                }
            } else if len == 1 {
                let cand = base - 35.0;
                if cand > dp_scores[i + 1] {
                    dp_scores[i + 1] = cand;
                    dp_prev[i + 1] = i;
                    dp_word[i + 1] = NONE_ID;
                }
            }
        }
    }

    let start_len = out_tokens.len();
    let mut cur = n;
    while cur > 0 {
        out_tokens.push(dp_word[cur]);
        cur = dp_prev[cur];
    }
    out_tokens[start_len..].reverse();
}

fn segment_corpora(
    corpora: &[PathBuf],
    lookup: &VocabLookup,
    dict: &DictData,
    scorer: &SegScorer<'_>,
    label: &str,
) -> std::io::Result<Vec<u32>> {
    let t0 = Instant::now();
    let mut tokens: Vec<u32> = Vec::with_capacity(120_000_000);
    let mut byte_pos = Vec::with_capacity(64);
    let mut dp_scores = Vec::with_capacity(64);
    let mut dp_prev = Vec::with_capacity(64);
    let mut dp_word = Vec::with_capacity(64);
    let mut sentences = 0usize;
    let mut sample_printed = 0usize;

    for corpus_path in corpora {
        let file = File::open(corpus_path)?;
        let reader = BufReader::with_capacity(1 << 20, file);
        for line in reader.lines() {
            let line = line?;
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let sent_start = tokens.len();
            tokens.push(BOS_ID);
            let mut first_clause = true;
            for clause in line.split('，') {
                if clause.is_empty() {
                    continue;
                }
                let init_prev = if first_clause {
                    BOS_ID
                } else {
                    tokens.push(PAUSE_ID);
                    PAUSE_ID
                };
                first_clause = false;
                segment_clause(
                    clause,
                    init_prev,
                    lookup,
                    &dict.word_offsets,
                    &dict.word_bytes,
                    scorer,
                    &mut tokens,
                    &mut byte_pos,
                    &mut dp_scores,
                    &mut dp_prev,
                    &mut dp_word,
                );
            }
            tokens.push(EOS_ID);
            sentences += 1;

            if sample_printed < 3 && tokens.len() - sent_start >= 6 {
                let segs: Vec<&str> = tokens[sent_start + 1..tokens.len() - 1]
                    .iter()
                    .map(|&id| {
                        if id == NONE_ID {
                            "?"
                        } else if id == PAUSE_ID {
                            "，"
                        } else {
                            let s = dict.word_offsets[id as usize] as usize;
                            let e = dict.word_offsets[id as usize + 1] as usize;
                            std::str::from_utf8(&dict.word_bytes[s..e]).unwrap_or("?")
                        }
                    })
                    .collect();
                println!("  [seg sample {label}] {}", segs.join("/"));
                sample_printed += 1;
            }
        }
    }
    println!(
        "[{label}] sentences={sentences} tokens={} ({:.2}s)",
        tokens.len(),
        t0.elapsed().as_secs_f64()
    );
    Ok(tokens)
}

struct Pass1Stats {
    unigram_count: Vec<u64>,
    p1: Vec<f64>,
    unigram_logp: Vec<f32>,
    unigram_bow: Vec<f32>,
    alpha1_vec: Vec<f64>,
    kept_bg: Vec<(u32, u32, u32)>,
    bg_id_map: U64Map,
    bigram_offsets: Vec<u32>,
    bigram_w2: Vec<u32>,
    bigram_prob: Vec<f64>,
    bigram_logp: Vec<f32>,
}

fn run_pass1(
    tokens: &[u32],
    num_words: usize,
    total_dict_weight: f64,
    dict_weights: &[f64],
    args: &TrainArgs,
    build_id_map: bool,
) -> Pass1Stats {
    let t2 = Instant::now();
    let mut unigram_count = vec![0u64; num_words];
    let mut bg_counts = U64Map::with_capacity(16_000_000);
    for i in 0..tokens.len() {
        let w1 = tokens[i];
        if w1 != NONE_ID {
            unigram_count[w1 as usize] += 1;
            if w1 != EOS_ID && i + 1 < tokens.len() {
                let w2 = tokens[i + 1];
                if w2 != NONE_ID {
                    bg_counts.inc(pack_bg(w1, w2));
                }
            }
        }
    }

    let mut n1_2 = 0u64;
    let mut n2_2 = 0u64;
    let mut n1_plus_dot_w = vec![0u32; num_words];
    let mut n1_plus_w_dot = vec![0u32; num_words];
    let mut c1_context = vec![0u64; num_words];
    let mut kept_bg: Vec<(u32, u32, u32)> = Vec::with_capacity(args.max_bigrams.min(bg_counts.len));

    for (key, c) in bg_counts.iter() {
        let w1 = (key >> 32) as u32;
        let w2 = key as u32;
        if c == 1 {
            n1_2 += 1;
        } else if c == 2 {
            n2_2 += 1;
        }
        n1_plus_w_dot[w1 as usize] += 1;
        n1_plus_dot_w[w2 as usize] += 1;
        c1_context[w1 as usize] += c as u64;
        if c >= args.min_bigram {
            kept_bg.push((w1, w2, c));
        }
    }
    drop(bg_counts);

    let d2 = ((n1_2 as f64) / ((n1_2 + 2 * n2_2).max(1) as f64)).clamp(0.4, 0.9);
    if kept_bg.len() > args.max_bigrams {
        kept_bg.sort_by(|a, b| b.2.cmp(&a.2));
        kept_bg.truncate(args.max_bigrams);
    }
    kept_bg.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    let num_bigrams = kept_bg.len();

    let bg_id_map = if build_id_map {
        let mut map = U64Map::with_capacity(num_bigrams + 16);
        for (idx, &(w1, w2, _)) in kept_bg.iter().enumerate() {
            map.insert(pack_bg(w1, w2), idx as u32);
        }
        map
    } else {
        U64Map::with_capacity(16)
    };
    println!(
        "[pass1] unique_bg_kept={} n1_2={} n2_2={} D2={:.4} ({:.2}s)",
        num_bigrams,
        n1_2,
        n2_2,
        d2,
        t2.elapsed().as_secs_f64()
    );

    let total_cont: f64 = n1_plus_dot_w[FIRST_REAL_WORD_ID as usize..]
        .iter()
        .map(|&x| x as f64)
        .sum::<f64>()
        + n1_plus_dot_w[EOS_ID as usize] as f64
        + n1_plus_dot_w[PAUSE_ID as usize] as f64;
    let total_uni: f64 = unigram_count[FIRST_REAL_WORD_ID as usize..]
        .iter()
        .map(|&x| x as f64)
        .sum::<f64>()
        + unigram_count[EOS_ID as usize] as f64
        + unigram_count[PAUSE_ID as usize] as f64;

    let mut p1 = vec![1e-12_f64; num_words];
    for w in 1..num_words {
        if w == BOS_ID as usize {
            continue;
        }
        let pc = n1_plus_dot_w[w] as f64 / total_cont.max(1.0);
        let pu = unigram_count[w] as f64 / total_uni.max(1.0);
        let pd = if w >= FIRST_REAL_WORD_ID as usize {
            dict_weights[w] / total_dict_weight.max(1.0)
        } else {
            0.0
        };
        p1[w] = (0.70 * pc + 0.25 * pu + 0.05 * pd).max(1e-12);
    }
    let p1_sum: f64 = p1[1..].iter().sum();
    for v in &mut p1[1..] {
        *v /= p1_sum;
    }
    let unigram_logp: Vec<f32> = p1.iter().map(|&p| p.ln() as f32).collect();

    let mut unigram_bow = vec![0.0_f32; num_words];
    let mut alpha1_vec = vec![1.0_f64; num_words];
    for w1 in 0..num_words {
        if c1_context[w1] > 0 {
            let a1 = (d2 * n1_plus_w_dot[w1] as f64 / c1_context[w1] as f64).clamp(1e-6, 1.0);
            alpha1_vec[w1] = a1;
            unigram_bow[w1] = a1.ln() as f32;
        }
    }

    let mut bigram_offsets = vec![0u32; num_words + 1];
    let mut bigram_w2 = Vec::with_capacity(num_bigrams);
    let mut bigram_prob = Vec::with_capacity(num_bigrams);
    let mut bigram_logp = Vec::with_capacity(num_bigrams);
    let mut cur_w1 = 0usize;
    for (bg_idx, &(w1, w2, c2)) in kept_bg.iter().enumerate() {
        while cur_w1 <= w1 as usize {
            bigram_offsets[cur_w1] = bg_idx as u32;
            cur_w1 += 1;
        }
        let p2 = (((c2 as f64 - d2) / c1_context[w1 as usize].max(1) as f64)
            + alpha1_vec[w1 as usize] * p1[w2 as usize])
            .max(1e-12);
        bigram_w2.push(w2);
        bigram_prob.push(p2);
        bigram_logp.push(p2.ln() as f32);
    }
    while cur_w1 <= num_words {
        bigram_offsets[cur_w1] = num_bigrams as u32;
        cur_w1 += 1;
    }

    Pass1Stats {
        unigram_count,
        p1,
        unigram_logp,
        unigram_bow,
        alpha1_vec,
        kept_bg,
        bg_id_map,
        bigram_offsets,
        bigram_w2,
        bigram_prob,
        bigram_logp,
    }
}

fn pack_bg(w1: u32, w2: u32) -> u64 {
    ((w1 as u64) << 32) | (w2 as u64)
}

fn build_lex_table(mut items: Vec<(u64, LexEntry)>, unigram_logp: &[f32], max_per_key: Option<usize>) -> LexTable {
    items.sort_by(|a, b| {
        a.0.cmp(&b.0).then_with(|| a.1.word_id.cmp(&b.1.word_id)).then_with(|| {
            b.1.reading_logp.total_cmp(&a.1.reading_logp)
        })
    });
    // Dedup (key, word_id) keeping highest reading_logp
    let mut deduped: Vec<(u64, LexEntry)> = Vec::with_capacity(items.len());
    for (k, e) in items {
        if let Some((lk, le)) = deduped.last() {
            if *lk == k && le.word_id == e.word_id {
                continue;
            }
        }
        deduped.push((k, e));
    }

    // Sort each key bucket by (unigram_logp[word_id] + reading_logp) descending
    deduped.sort_by(|a, b| {
        a.0.cmp(&b.0).then_with(|| {
            let sa = unigram_logp[a.1.word_id as usize] + a.1.reading_logp;
            let sb = unigram_logp[b.1.word_id as usize] + b.1.reading_logp;
            sb.total_cmp(&sa)
        })
    });

    let mut keys = Vec::new();
    let mut ranges = vec![0u32];
    let mut entries = Vec::new();
    let mut cur_key: Option<u64> = None;
    let mut bucket_count = 0usize;

    for (k, e) in deduped {
        if cur_key != Some(k) {
            if cur_key.is_some() {
                ranges.push(entries.len() as u32);
            }
            cur_key = Some(k);
            keys.push(k);
            bucket_count = 0;
        }
        if let Some(cap) = max_per_key {
            if bucket_count >= cap {
                continue;
            }
        }
        entries.push(e);
        bucket_count += 1;
    }
    if cur_key.is_some() {
        ranges.push(entries.len() as u32);
    }

    LexTable {
        keys,
        ranges,
        entries,
    }
}

fn main() -> std::io::Result<()> {
    let t_start = Instant::now();
    let args = match parse_args() {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("error: {msg}");
            eprintln!("usage: ime-train --dicts <DIR> --corpus <FILE>... --out <MODEL.bin>");
            std::process::exit(2);
        }
    };

    // 1. Load Wanxiang dictionaries (optionally filtered to clean simplified chars & lexical words)
    let t0 = Instant::now();
    let lexicon = match &args.lexicon_path {
        Some(p) => Some(load_lexicon_filter(p)?),
        None => None,
    };
    let allowed_chars = match &args.allowed_chars_path {
        Some(p) => Some(load_allowed_chars(p)?),
        None => None,
    };
    let dict = load_wanxiang_dicts(
        &args.dicts_dir,
        args.max_multi_words,
        lexicon.as_ref(),
        allowed_chars.as_ref(),
    )?;
    let num_words = dict.dict_weights.len();
    let total_dict_weight: f64 = dict.dict_weights[FIRST_REAL_WORD_ID as usize..].iter().sum();
    let dict_logp: Vec<f32> = dict
        .dict_weights
        .iter()
        .map(|&w| ((w / total_dict_weight.max(1.0)).max(1e-12)).ln() as f32)
        .collect();
    let lookup = VocabLookup::build(&dict.word_offsets, &dict.word_bytes);
    println!(
        "[dict] vocab={} words ({:.2}s)",
        num_words,
        t0.elapsed().as_secs_f64()
    );

    // 2. Segment corpora (Pass 1 unigram DP) and count unigrams & bigrams
    let mut tokens = segment_corpora(
        &args.corpora,
        &lookup,
        &dict,
        &SegScorer::Unigram(&dict_logp),
        "segment",
    )?;
    let mut pass1 = run_pass1(
        &tokens,
        num_words,
        total_dict_weight,
        &dict.dict_weights,
        &args,
        !args.resegment,
    );

    // Optional EM Pass 2: re-segment with bigram DP to match inference segmentation, then re-count Pass 1.
    if args.resegment {
        drop(tokens);
        let scorer = SegScorer::Bigram {
            unigram_logp: &pass1.unigram_logp,
            unigram_bow: &pass1.unigram_bow,
            bigram_offsets: &pass1.bigram_offsets,
            bigram_w2: &pass1.bigram_w2,
            bigram_logp: &pass1.bigram_logp,
        };
        tokens = segment_corpora(&args.corpora, &lookup, &dict, &scorer, "resegment")?;
        drop(pass1);
        pass1 = run_pass1(
            &tokens,
            num_words,
            total_dict_weight,
            &dict.dict_weights,
            &args,
            true,
        );
    }

    let Pass1Stats {
        unigram_count,
        p1,
        unigram_logp,
        unigram_bow,
        alpha1_vec,
        kept_bg,
        bg_id_map,
        bigram_offsets,
        bigram_w2,
        bigram_prob,
        bigram_logp,
    } = pass1;
    let num_bigrams = kept_bg.len();

    // 3. Pass 2: sharded trigram counting over kept bigrams + singleton discount stats
    let t3 = Instant::now();
    let mut n1_3 = 0u64;
    let mut n2_3 = 0u64;
    let mut bg_n1_plus = vec![0u32; num_bigrams];
    let mut bg_total_follow = vec![0u64; num_bigrams];

    // First count trigrams starting with singleton/dropped bigrams for exact D3 estimation.
    for i in 0..tokens.len().saturating_sub(2) {
        let (w1, w2, w3) = (tokens[i], tokens[i + 1], tokens[i + 2]);
        if w1 == NONE_ID || w2 == NONE_ID || w3 == NONE_ID || w1 == EOS_ID || w2 == EOS_ID {
            continue;
        }
        if bg_id_map.get(pack_bg(w1, w2)).is_none() {
            // Note: unkept bigrams with c2 == 1 necessarily have trigram count 1.
            n1_3 += 1;
        }
    }

    let shards = args.shards;
    let mut tri_map = U64Map::with_capacity(4_000_000);
    let mut kept_tri: Vec<(u32, u32, u32)> = Vec::with_capacity(args.max_trigrams.min(5_000_000));

    for shard in 0..shards {
        tri_map.clear();
        for i in 0..tokens.len().saturating_sub(2) {
            let (w1, w2, w3) = (tokens[i], tokens[i + 1], tokens[i + 2]);
            if w1 == NONE_ID || w2 == NONE_ID || w3 == NONE_ID || w1 == EOS_ID || w2 == EOS_ID {
                continue;
            }
            if let Some(bg_id) = bg_id_map.get(pack_bg(w1, w2)) {
                if (bg_id as usize) % shards == shard {
                    tri_map.inc(((bg_id as u64) << 32) | (w3 as u64));
                }
            }
        }
        for (key, c) in tri_map.iter() {
            let bg_id = (key >> 32) as u32;
            let w3 = key as u32;
            if c == 1 {
                n1_3 += 1;
            } else if c == 2 {
                n2_3 += 1;
            }
            bg_n1_plus[bg_id as usize] += 1;
            bg_total_follow[bg_id as usize] += c as u64;
            if c >= args.min_trigram || (w3 == EOS_ID && c >= 2) {
                kept_tri.push((bg_id, w3, c));
            }
        }
    }
    drop(tri_map);
    drop(bg_id_map);
    drop(tokens);

    let d3 = ((n1_3 as f64) / ((n1_3 + 2 * n2_3).max(1) as f64)).clamp(0.4, 0.9);
    if kept_tri.len() > args.max_trigrams {
        kept_tri.sort_by(|a, b| {
            let pa = a.2 + u32::from(a.1 == EOS_ID) * 2;
            let pb = b.2 + u32::from(b.1 == EOS_ID) * 2;
            pb.cmp(&pa)
        });
        kept_tri.truncate(args.max_trigrams);
    }
    kept_tri.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    let num_trigrams = kept_tri.len();
    println!(
        "[pass2] trigram_kept={} n1_3={} n2_3={} D3={:.4} ({:.2}s)",
        num_trigrams,
        n1_3,
        n2_3,
        d3,
        t3.elapsed().as_secs_f64()
    );

    let mut bigram_bow = vec![0.0_f32; num_bigrams];
    let mut alpha2_vec = vec![1.0_f64; num_bigrams];
    for bg_id in 0..num_bigrams {
        let denom = bg_total_follow[bg_id];
        if denom > 0 {
            let a2 = (d3 * bg_n1_plus[bg_id] as f64 / denom as f64).clamp(1e-6, 1.0);
            alpha2_vec[bg_id] = a2;
            bigram_bow[bg_id] = a2.ln() as f32;
        }
    }

    let mut trigram_offsets = vec![0u32; num_bigrams + 1];
    let mut trigram_w3 = Vec::with_capacity(num_trigrams);
    let mut trigram_logp = Vec::with_capacity(num_trigrams);
    let mut cur_bg = 0usize;
    for (tri_idx, &(bg_id, w3, c3)) in kept_tri.iter().enumerate() {
        while cur_bg <= bg_id as usize {
            trigram_offsets[cur_bg] = tri_idx as u32;
            cur_bg += 1;
        }
        let w2 = bigram_w2[bg_id as usize];
        let p2_w2_w3 = {
            let start = bigram_offsets[w2 as usize] as usize;
            let end = bigram_offsets[w2 as usize + 1] as usize;
            match bigram_w2[start..end].binary_search(&w3) {
                Ok(off) => bigram_prob[start + off],
                Err(_) => alpha1_vec[w2 as usize] * p1[w3 as usize],
            }
        };
        let denom = bg_total_follow[bg_id as usize].max(1) as f64;
        let p3 = (((c3 as f64 - d3) / denom) + alpha2_vec[bg_id as usize] * p2_w2_w3).max(1e-12);
        trigram_w3.push(w3);
        trigram_logp.push(p3.ln() as f32);
    }
    while cur_bg <= num_bigrams {
        trigram_offsets[cur_bg] = num_trigrams as u32;
        cur_bg += 1;
    }
    drop(kept_bg);
    drop(kept_tri);
    drop(bigram_prob);

    // 6. Build lexicon indices
    let t4 = Instant::now();
    let mut exact_items: Vec<(u64, LexEntry)> = Vec::with_capacity(num_words * 2);
    let mut initial_chars: [Vec<LexEntry>; 26] = std::array::from_fn(|_| Vec::new());
    let mut abbrev_tail_items: Vec<(u64, LexEntry)> = Vec::new();
    let mut abbrev_init_items: Vec<(u64, LexEntry)> = Vec::new();

    for (&word_id, syl_list) in &dict.readings {
        let word_bytes_slice =
            &dict.word_bytes[dict.word_offsets[word_id as usize] as usize..dict.word_offsets[word_id as usize + 1] as usize];
        let word_str = std::str::from_utf8(word_bytes_slice).unwrap_or("");
        let is_single_char = word_str.chars().count() == 1;
        let is_protected = PROTECTED_FUNCTION_WORDS.contains(&word_str);
        let in_corpus = unigram_count[word_id as usize] >= 1;
        let high_freq_word = unigram_count[word_id as usize] >= 5 || dict.dict_weights[word_id as usize] >= 200.0;

        let total_w: f64 = syl_list.iter().map(|(_, wt)| *wt).sum::<f64>().max(1.0);
        for (syls, wt) in syl_list {
            let reading_logp = (*wt / total_w).max(1e-9).ln() as f32;
            let entry = LexEntry {
                word_id,
                reading_logp,
            };
            if let Some(key) = pack_exact(syls) {
                exact_items.push((key, entry));
            }
            if is_single_char && syls.len() == 1 {
                let lt = syllable_first_letter(syls[0]) as usize;
                if lt < 26 {
                    initial_chars[lt].push(entry);
                }
            } else if (2..=5).contains(&syls.len()) && (in_corpus || dict.dict_weights[word_id as usize] >= 50.0) {
                let last_lt = syllable_first_letter(*syls.last().unwrap());
                if let Some(key) = pack_abbrev_tail(&syls[..syls.len() - 1], last_lt) {
                    abbrev_tail_items.push((key, entry));
                }
            }
            if !is_single_char && (2..=4).contains(&syls.len()) && high_freq_word {
                let letters: Vec<u8> = syls.iter().map(|&s| syllable_first_letter(s)).collect();
                if let Some(key) = pack_initials(&letters) {
                    abbrev_init_items.push((key, entry));
                }
            }
        }
        let _ = is_protected;
    }

    for bucket in &mut initial_chars {
        bucket.sort_by(|a, b| {
            let sa = unigram_logp[a.word_id as usize] + a.reading_logp;
            let sb = unigram_logp[b.word_id as usize] + b.reading_logp;
            sb.total_cmp(&sa)
        });
        // Dedup by word_id keeping best reading_logp
        let mut seen = HashSet::new();
        bucket.retain(|e| seen.insert(e.word_id));
        if bucket.len() > 80 {
            let mut kept = bucket[..80].to_vec();
            for e in &bucket[80..] {
                let s = dict.word_offsets[e.word_id as usize] as usize;
                let end = dict.word_offsets[e.word_id as usize + 1] as usize;
                let w = std::str::from_utf8(&dict.word_bytes[s..end]).unwrap_or("");
                if PROTECTED_FUNCTION_WORDS.contains(&w) {
                    kept.push(*e);
                }
            }
            *bucket = kept;
        }
    }

    let exact = build_lex_table(exact_items, &unigram_logp, None);
    let abbrev_tail = build_lex_table(abbrev_tail_items, &unigram_logp, Some(12));
    let abbrev_init = build_lex_table(abbrev_init_items, &unigram_logp, Some(12));
    println!(
        "[lexicon] exact_keys={} abbrev_tail_keys={} abbrev_init_keys={} ({:.2}s)",
        exact.keys.len(),
        abbrev_tail.keys.len(),
        abbrev_init.keys.len(),
        t4.elapsed().as_secs_f64()
    );

    if let Some(parent) = args.out_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let model = Model::from_parts(
        dict.word_offsets,
        dict.word_bytes,
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
    );
    model.save(&args.out_path)?;
    let size_mb = std::fs::metadata(&args.out_path)?.len() as f64 / 1e6;
    println!(
        "[done] saved {} ({:.1} MB) in {:.2}s",
        args.out_path.display(),
        size_mb,
        t_start.elapsed().as_secs_f64()
    );
    Ok(())
}

//! Standard Mandarin pinyin syllable inventory, key packing, and ambiguous DAG segmentation.

pub type SyllableId = u16;

/// Complete 415 standard toneless Mandarin syllables (ü written as `v`), sorted lexicographically.
/// Standalone nasal interjections (`hm`, `m`, `n`, `ng`) are excluded so abbreviations stay unambiguous.
pub const SYLLABLES: [&str; 415] = [
    "a", "ai", "an", "ang", "ao", "ba", "bai", "ban", "bang", "bao", "bei", "ben", "beng", "bi",
    "bian", "biang", "biao", "bie", "bin", "bing", "bo", "bu", "ca", "cai", "can", "cang", "cao", "ce",
    "cei", "cen", "ceng", "cha", "chai", "chan", "chang", "chao", "che", "chen", "cheng", "chi", "chong", "chou",
    "chu", "chua", "chuai", "chuan", "chuang", "chui", "chun", "chuo", "ci", "cong", "cou", "cu", "cuan", "cui",
    "cun", "cuo", "da", "dai", "dan", "dang", "dao", "de", "dei", "den", "deng", "di", "dia", "dian",
    "diao", "die", "ding", "diu", "dong", "dou", "du", "duan", "dui", "dun", "duo", "e", "ei", "en",
    "eng", "er", "fa", "fan", "fang", "fei", "fen", "feng", "fiao", "fo", "fou", "fu", "ga", "gai",
    "gan", "gang", "gao", "ge", "gei", "gen", "geng", "gong", "gou", "gu", "gua", "guai", "guan", "guang",
    "gui", "gun", "guo", "ha", "hai", "han", "hang", "hao", "he", "hei", "hen", "heng", "hong", "hou",
    "hu", "hua", "huai", "huan", "huang", "hui", "hun", "huo", "ji", "jia", "jian", "jiang", "jiao", "jie",
    "jin", "jing", "jiong", "jiu", "ju", "juan", "jue", "jun", "ka", "kai", "kan", "kang", "kao", "ke",
    "kei", "ken", "keng", "kong", "kou", "ku", "kua", "kuai", "kuan", "kuang", "kui", "kun", "kuo", "la",
    "lai", "lan", "lang", "lao", "le", "lei", "leng", "li", "lia", "lian", "liang", "liao", "lie", "lin",
    "ling", "liu", "lo", "long", "lou", "lu", "luan", "lun", "luo", "lv", "lve", "ma", "mai", "man",
    "mang", "mao", "me", "mei", "men", "meng", "mi", "mian", "miao", "mie", "min", "ming", "miu", "mo",
    "mou", "mu", "na", "nai", "nan", "nang", "nao", "ne", "nei", "nen", "neng", "ni", "nian", "niang",
    "niao", "nie", "nin", "ning", "niu", "nong", "nou", "nu", "nuan", "nun", "nuo", "nv", "nve", "o",
    "ou", "pa", "pai", "pan", "pang", "pao", "pei", "pen", "peng", "pi", "pian", "piao", "pie", "pin",
    "ping", "po", "pou", "pu", "qi", "qia", "qian", "qiang", "qiao", "qie", "qin", "qing", "qiong", "qiu",
    "qu", "quan", "que", "qun", "ran", "rang", "rao", "re", "ren", "reng", "ri", "rong", "rou", "ru",
    "rua", "ruan", "rui", "run", "ruo", "sa", "sai", "san", "sang", "sao", "se", "sen", "seng", "sha",
    "shai", "shan", "shang", "shao", "she", "shei", "shen", "sheng", "shi", "shou", "shu", "shua", "shuai", "shuan",
    "shuang", "shui", "shun", "shuo", "si", "song", "sou", "su", "suan", "sui", "sun", "suo", "ta", "tai",
    "tan", "tang", "tao", "te", "tei", "teng", "ti", "tian", "tiao", "tie", "ting", "tong", "tou", "tu",
    "tuan", "tui", "tun", "tuo", "wa", "wai", "wan", "wang", "wei", "wen", "weng", "wo", "wu", "xi",
    "xia", "xian", "xiang", "xiao", "xie", "xin", "xing", "xiong", "xiu", "xu", "xuan", "xue", "xun", "ya",
    "yan", "yang", "yao", "ye", "yi", "yin", "ying", "yo", "yong", "you", "yu", "yuan", "yue", "yun",
    "za", "zai", "zan", "zang", "zao", "ze", "zei", "zen", "zeng", "zha", "zhai", "zhan", "zhang", "zhao",
    "zhe", "zhei", "zhen", "zheng", "zhi", "zhong", "zhou", "zhu", "zhua", "zhuai", "zhuan", "zhuang", "zhui", "zhun",
    "zhuo", "zi", "zong", "zou", "zu", "zuan", "zui", "zun", "zuo",
];

pub fn syllable_id(syl: &str) -> Option<SyllableId> {
    SYLLABLES.binary_search(&syl).ok().map(|idx| (idx + 1) as u16)
}

pub fn syllable_str(id: SyllableId) -> &'static str {
    SYLLABLES.get(id.saturating_sub(1) as usize).copied().unwrap_or("")
}

pub fn letter_index(b: u8) -> Option<u8> {
    let lower = b.to_ascii_lowercase();
    if lower.is_ascii_lowercase() {
        Some(lower - b'a')
    } else {
        None
    }
}

pub fn is_valid_initial_letter(letter_idx: u8) -> bool {
    letter_idx < 26 && !matches!(letter_idx + b'a', b'i' | b'u' | b'v')
}

pub fn syllable_first_letter(id: SyllableId) -> u8 {
    let s = syllable_str(id);
    s.as_bytes().first().map(|b| b - b'a').unwrap_or(0)
}

/// Packs `1..=6` exact `SyllableId`s into a unique `u64` (9 bits each + 3-bit length at bit 54).
pub fn pack_exact(syls: &[SyllableId]) -> Option<u64> {
    let len = syls.len();
    if !(1..=6).contains(&len) {
        return None;
    }
    let mut key = (len as u64) << 54;
    for (i, &id) in syls.iter().enumerate() {
        if id == 0 || id as usize > SYLLABLES.len() {
            return None;
        }
        key |= (id as u64) << (i * 9);
    }
    Some(key)
}

/// Packs `1..=5` full leading syllables plus 1 trailing initial letter (`total len in 2..=6`).
pub fn pack_abbrev_tail(prefix_syls: &[SyllableId], last_letter: u8) -> Option<u64> {
    let plen = prefix_syls.len();
    if !(1..=5).contains(&plen) || last_letter >= 26 {
        return None;
    }
    let total_len = plen + 1;
    let mut key = (total_len as u64) << 54;
    for (i, &id) in prefix_syls.iter().enumerate() {
        if id == 0 || id as usize > SYLLABLES.len() {
            return None;
        }
        key |= (id as u64) << (i * 9);
    }
    key |= (last_letter as u64) << 45;
    Some(key)
}

/// Packs `2..=6` pure first-letter indices (`0..26`) into a unique `u64`.
pub fn pack_initials(letters: &[u8]) -> Option<u64> {
    let len = letters.len();
    if !(2..=6).contains(&len) {
        return None;
    }
    let mut key = (len as u64) << 30;
    for (i, &lt) in letters.iter().enumerate() {
        if lt >= 26 {
            return None;
        }
        key |= (lt as u64) << (i * 5);
    }
    Some(key)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DagEdgeKind {
    Exact(SyllableId),
    Abbrev(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanPron {
    Exact {
        len: u8,
        syls: [SyllableId; 6],
    },
    AbbrevTail {
        prefix_len: u8,
        prefix: [SyllableId; 5],
        last_letter: u8,
    },
    AbbrevInit {
        len: u8,
        letters: [u8; 6],
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DagEdge {
    pub start: usize,
    pub end: usize,
    pub kind: DagEdgeKind,
}

#[derive(Debug, Clone)]
pub struct PinyinDag {
    pub len: usize,
    pub exact_only: bool,
    pub edges_from: Vec<Vec<DagEdge>>,
}

/// Builds an ambiguous pinyin segmentation DAG for raw keystrokes.
/// Apostrophes (`'`) act as hard syllable boundaries without advancing key positions.
/// Whenever at least one full exact-syllable path reaches the end, `Abbrev` edges are omitted
/// completely so abbreviated matches can never outrank exact pinyin.
pub fn build_dag(keys: &str) -> PinyinDag {
    let mut norm = Vec::with_capacity(keys.len());
    let mut boundary_before = Vec::with_capacity(keys.len());
    let mut pending_sep = false;

    for &b in keys.as_bytes() {
        if b == b'\'' {
            if !norm.is_empty() {
                pending_sep = true;
            }
            continue;
        }
        if b.is_ascii_alphabetic() {
            norm.push(b.to_ascii_lowercase());
            boundary_before.push(pending_sep);
            pending_sep = false;
        } else {
            return PinyinDag {
                len: 0,
                exact_only: true,
                edges_from: Vec::new(),
            };
        }
    }

    let n = norm.len();
    if n == 0 {
        return PinyinDag {
            len: 0,
            exact_only: true,
            edges_from: Vec::new(),
        };
    }

    let mut edges_from = vec![Vec::new(); n];
    let mut can_reach = vec![false; n + 1];
    can_reach[0] = true;

    for i in 0..n {
        let max_len = 6.min(n - i);
        for len in 1..=max_len {
            if len > 1 && boundary_before[i + len - 1] {
                break;
            }
            if let Ok(sub) = std::str::from_utf8(&norm[i..i + len]) {
                if let Some(id) = syllable_id(sub) {
                    edges_from[i].push(DagEdge {
                        start: i,
                        end: i + len,
                        kind: DagEdgeKind::Exact(id),
                    });
                    if can_reach[i] {
                        can_reach[i + len] = true;
                    }
                }
            }
        }
    }

    let exact_only = can_reach[n];
    if !exact_only {
        for i in 0..n {
            let letter = norm[i] - b'a';
            if is_valid_initial_letter(letter) {
                edges_from[i].push(DagEdge {
                    start: i,
                    end: i + 1,
                    kind: DagEdgeKind::Abbrev(letter),
                });
            }
        }
    }

    PinyinDag {
        len: n,
        exact_only,
        edges_from,
    }
}

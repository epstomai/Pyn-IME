//! Platform-independent pinyin conversion engine.
//!
//! Driven through the five operations exposed by the evaluation protocol (`docs/eval-protocol.md`):
//! reset, set context, convert, commit, and undo.

pub mod decoder;
pub mod model;
pub mod pinyin;
pub mod reranker;
pub mod user_dict;

use std::path::Path;
use std::sync::Arc;

pub use decoder::DecoderConfig;
pub use model::Model;
pub use reranker::Reranker;
pub use user_dict::{LearnConfig, UserMemory, UserStats};

/// A piece of the text before the caret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextSegment {
    pub text: String,
    /// What the user typed to produce `text`; `None` for text the engine did
    /// not produce, such as punctuation or text read from the application.
    pub keys: Option<String>,
}

/// The conversion engine.
#[derive(Debug, Default)]
pub struct Engine {
    context: Vec<ContextSegment>,
    commit_deltas: Vec<Option<user_dict::LearnDelta>>,
    last_keys: String,
    last_base_top1: Option<String>,
    last_current_top1: Option<String>,
    model: Option<Arc<Model>>,
    reranker: Option<Arc<Reranker>>,
    user_memory: UserMemory,
    config: DecoderConfig,
}

impl Engine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_model(model: Arc<Model>) -> Self {
        Self {
            context: Vec::new(),
            commit_deltas: Vec::new(),
            last_keys: String::new(),
            last_base_top1: None,
            last_current_top1: None,
            model: Some(model),
            reranker: None,
            user_memory: UserMemory::default(),
            config: DecoderConfig::default(),
        }
    }

    pub fn load_model(path: &Path) -> std::io::Result<Self> {
        let model = Arc::new(Model::load(path)?);
        Ok(Self::with_model(model))
    }

    pub fn set_reranker(&mut self, reranker: Option<Arc<Reranker>>) {
        self.reranker = reranker;
    }

    pub fn load_reranker(&mut self, path: &Path) -> std::io::Result<()> {
        let reranker = Arc::new(Reranker::load(path)?);
        self.reranker = Some(reranker);
        Ok(())
    }

    pub fn set_config(&mut self, config: DecoderConfig) {
        self.config = config;
    }

    pub fn set_learn_config(&mut self, config: LearnConfig) {
        self.user_memory.set_config(config);
    }

    pub fn learn_config(&self) -> LearnConfig {
        self.user_memory.config()
    }

    pub fn set_learning_enabled(&mut self, enabled: bool) {
        self.user_memory.set_enabled(enabled);
    }

    pub fn user_stats(&self) -> UserStats {
        self.user_memory.stats()
    }

    pub fn active_user_words(&self) -> Vec<(String, f32, bool)> {
        self.user_memory.active_words_summary()
    }

    pub fn load_user_dict(&mut self, path: &Path) -> std::io::Result<()> {
        let Some(ref model) = self.model else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "cannot load user dict without base model",
            ));
        };
        let mem = UserMemory::load(path, model, self.user_memory.config())?;
        self.user_memory = mem;
        Ok(())
    }

    pub fn save_user_dict(&self, path: &Path) -> std::io::Result<()> {
        self.user_memory.save(path)
    }

    /// Forgets everything learned and clears the context.
    pub fn reset(&mut self) {
        self.context.clear();
        self.commit_deltas.clear();
        self.last_keys.clear();
        self.last_base_top1 = None;
        self.last_current_top1 = None;
        self.user_memory.clear();
    }

    /// Replaces the text before the caret. Not a learning event (beyond flushing completed runs).
    pub fn set_context(&mut self, segments: Vec<ContextSegment>) {
        if let Some(ref model) = self.model {
            self.user_memory.flush_compose_run(model);
        }
        self.commit_deltas = vec![None; segments.len()];
        self.context = segments;
        self.last_keys.clear();
        self.last_base_top1 = None;
        self.last_current_top1 = None;
    }

    pub fn context_text(&self) -> String {
        self.context.iter().map(|seg| seg.text.as_str()).collect()
    }

    /// Candidates for raw keystrokes, best first.
    pub fn convert(&mut self, keys: &str, limit: usize) -> Vec<String> {
        self.last_keys = keys.to_owned();
        let Some(ref model) = self.model else {
            self.last_base_top1 = None;
            self.last_current_top1 = None;
            return Vec::new();
        };
        let ctx = self.context_text();
        let outcome = decoder::decode_with_user(
            model,
            self.reranker.as_deref(),
            Some(&self.user_memory),
            keys,
            &ctx,
            limit,
            &self.config,
        );
        self.last_base_top1 = outcome.base_top1;
        self.last_current_top1 = outcome.candidates.first().cloned();
        outcome.candidates
    }

    /// The user picked `text` for the last converted keys; applies guarded incremental learning.
    pub fn commit(&mut self, text: &str) {
        let context_before = self.context_text();
        let keys = std::mem::take(&mut self.last_keys);
        let last_base = self.last_base_top1.take();
        let last_cur = self.last_current_top1.take();
        let delta = self.model.as_ref().map(|model| {
            self.user_memory.record_commit(
                model,
                &keys,
                text,
                &context_before,
                last_base.as_deref(),
                last_cur.as_deref(),
            )
        });
        self.commit_deltas.push(delta);
        self.context.push(ContextSegment {
            text: text.to_owned(),
            keys: Some(keys),
        });
    }

    /// The user deleted what was just committed. Returns whether there was a
    /// commit to take back.
    pub fn undo(&mut self) -> bool {
        match self.context.last() {
            Some(segment) if segment.keys.is_some() => {
                self.context.pop();
                if let Some(Some(delta)) = self.commit_deltas.pop() {
                    self.user_memory.undo_commit(delta);
                }
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{LexEntry, LexTable, BOS_ID, EOS_ID, PAUSE_ID};
    use crate::pinyin::{
        build_dag, pack_abbrev_tail, pack_exact, pack_initials, syllable_id, DagEdgeKind,
    };

    fn make_toy_model() -> Model {
        // Words: 0..12 original toy vocab, 13..25 Phase 2 boundary-join & wrapper test vocab.
        let words = [
            "<bos>", "<eos>", "<pause>", "的", "得", "地", "了", "乐", "西安", "先", "我", "吃过饭",
            "好的", "什", "么", "麽", "什么", "快", "快乐", "银", "行", "杏", "银行", "银杏",
            "沙漠", "雄狮",
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
        unigram_logp[3] = -1.5; // 的
        unigram_logp[4] = -3.0; // 得
        unigram_logp[5] = -3.2; // 地
        unigram_logp[6] = -1.8; // 了
        unigram_logp[7] = -4.5; // 乐
        unigram_logp[8] = -4.0; // 西安
        unigram_logp[9] = -3.5; // 先
        unigram_logp[10] = -2.0; // 我
        unigram_logp[11] = -5.0; // 吃过饭
        unigram_logp[12] = -3.8; // 好的
        unigram_logp[13] = -7.0; // 什
        unigram_logp[14] = -6.5; // 么
        unigram_logp[15] = -6.0; // 麽 (deliberately higher standalone unigram than 么)
        unigram_logp[16] = -2.0; // 什么
        unigram_logp[17] = -4.5; // 快
        unigram_logp[18] = -2.0; // 快乐
        unigram_logp[19] = -5.0; // 银
        unigram_logp[20] = -3.0; // 行
        unigram_logp[21] = -5.5; // 杏
        unigram_logp[22] = -2.5; // 银行
        unigram_logp[23] = -3.5; // 银杏
        unigram_logp[24] = -4.0; // 沙漠
        unigram_logp[25] = -4.5; // 雄狮
        let unigram_bow = vec![-0.2_f32; n];

        let de_id = syllable_id("de").unwrap();
        let di_id = syllable_id("di").unwrap();
        let le_id = syllable_id("le").unwrap();
        let xi_id = syllable_id("xi").unwrap();
        let an_id = syllable_id("an").unwrap();
        let xian_id = syllable_id("xian").unwrap();
        let wo_id = syllable_id("wo").unwrap();
        let hao_id = syllable_id("hao").unwrap();
        let shi_id = syllable_id("shi").unwrap();
        let me_id = syllable_id("me").unwrap();
        let kuai_id = syllable_id("kuai").unwrap();
        let yin_id = syllable_id("yin").unwrap();
        let hang_id = syllable_id("hang").unwrap();
        let xing_id = syllable_id("xing").unwrap();
        let sha_id = syllable_id("sha").unwrap();
        let mo_id = syllable_id("mo").unwrap();
        let xiong_id = syllable_id("xiong").unwrap();

        let mut exact_pairs = vec![
            (pack_exact(&[de_id]).unwrap(), LexEntry { word_id: 3, reading_logp: -0.01 }),
            (pack_exact(&[de_id]).unwrap(), LexEntry { word_id: 4, reading_logp: -0.05 }),
            (pack_exact(&[de_id]).unwrap(), LexEntry { word_id: 5, reading_logp: -0.2 }),
            (pack_exact(&[di_id]).unwrap(), LexEntry { word_id: 5, reading_logp: -1.8 }),
            (pack_exact(&[di_id]).unwrap(), LexEntry { word_id: 3, reading_logp: -8.0 }),
            (pack_exact(&[le_id]).unwrap(), LexEntry { word_id: 6, reading_logp: -0.02 }),
            (pack_exact(&[le_id]).unwrap(), LexEntry { word_id: 7, reading_logp: -0.05 }),
            (pack_exact(&[wo_id]).unwrap(), LexEntry { word_id: 10, reading_logp: 0.0 }),
            (pack_exact(&[xian_id]).unwrap(), LexEntry { word_id: 9, reading_logp: 0.0 }),
            (pack_exact(&[xi_id, an_id]).unwrap(), LexEntry { word_id: 8, reading_logp: 0.0 }),
            (pack_exact(&[hao_id, de_id]).unwrap(), LexEntry { word_id: 12, reading_logp: 0.0 }),
            (pack_exact(&[shi_id]).unwrap(), LexEntry { word_id: 13, reading_logp: 0.0 }),
            (pack_exact(&[me_id]).unwrap(), LexEntry { word_id: 14, reading_logp: -0.5 }),
            (pack_exact(&[me_id]).unwrap(), LexEntry { word_id: 15, reading_logp: -0.1 }),
            (pack_exact(&[shi_id, me_id]).unwrap(), LexEntry { word_id: 16, reading_logp: 0.0 }),
            (pack_exact(&[kuai_id]).unwrap(), LexEntry { word_id: 17, reading_logp: 0.0 }),
            (pack_exact(&[kuai_id, le_id]).unwrap(), LexEntry { word_id: 18, reading_logp: 0.0 }),
            (pack_exact(&[yin_id]).unwrap(), LexEntry { word_id: 19, reading_logp: 0.0 }),
            (pack_exact(&[xing_id]).unwrap(), LexEntry { word_id: 20, reading_logp: -0.3 }),
            (pack_exact(&[hang_id]).unwrap(), LexEntry { word_id: 20, reading_logp: -1.2 }),
            (pack_exact(&[xing_id]).unwrap(), LexEntry { word_id: 21, reading_logp: -0.5 }),
            (pack_exact(&[yin_id, hang_id]).unwrap(), LexEntry { word_id: 22, reading_logp: 0.0 }),
            (pack_exact(&[yin_id, xing_id]).unwrap(), LexEntry { word_id: 23, reading_logp: 0.0 }),
            (pack_exact(&[sha_id, mo_id]).unwrap(), LexEntry { word_id: 24, reading_logp: 0.0 }),
            (pack_exact(&[xiong_id, shi_id]).unwrap(), LexEntry { word_id: 25, reading_logp: 0.0 }),
        ];
        let exact = build_lex_table(exact_pairs.drain(..));

        let mut initial_chars: [Vec<LexEntry>; 26] = std::array::from_fn(|_| Vec::new());
        initial_chars[(b'd' - b'a') as usize] = vec![
            LexEntry { word_id: 3, reading_logp: -0.01 },
            LexEntry { word_id: 4, reading_logp: -0.05 },
        ];
        initial_chars[(b'l' - b'a') as usize] = vec![
            LexEntry { word_id: 6, reading_logp: -0.02 },
            LexEntry { word_id: 7, reading_logp: -0.05 },
        ];

        let abbrev_tail = build_lex_table(
            [(
                pack_abbrev_tail(&[hao_id], b'd' - b'a').unwrap(),
                LexEntry { word_id: 12, reading_logp: 0.0 },
            )]
            .into_iter(),
        );
        let abbrev_init = build_lex_table(
            [(
                pack_initials(&[b'h' - b'a', b'd' - b'a']).unwrap(),
                LexEntry { word_id: 12, reading_logp: 0.0 },
            )]
            .into_iter(),
        );

        let mut bigram_offsets = vec![0u32; n + 1];
        let mut bigram_w2 = Vec::new();
        let mut bigram_logp = Vec::new();
        let mut bigram_bow = Vec::new();
        for w1 in 0..n {
            bigram_offsets[w1] = bigram_w2.len() as u32;
            match w1 {
                0 => {
                    // <bos> -> 快, 快乐, 银行, 银杏
                    bigram_w2.extend_from_slice(&[17, 18, 22, 23]);
                    bigram_logp.extend_from_slice(&[-5.0, -2.0, -1.5, -2.5]);
                    bigram_bow.extend_from_slice(&[-0.2, -0.1, -0.1, -0.1]);
                }
                6 => {
                    bigram_w2.push(EOS_ID);
                    bigram_logp.push(-0.3);
                    bigram_bow.push(-0.1);
                }
                11 => {
                    bigram_w2.extend_from_slice(&[6, 7]);
                    bigram_logp.extend_from_slice(&[-0.4, -8.0]);
                    bigram_bow.extend_from_slice(&[-0.1, -0.5]);
                }
                16 => {
                    bigram_w2.push(EOS_ID);
                    bigram_logp.push(-0.2);
                    bigram_bow.push(-0.1);
                }
                17 => {
                    bigram_w2.push(6);
                    bigram_logp.push(-1.5);
                    bigram_bow.push(-0.2);
                }
                18 => {
                    bigram_w2.push(EOS_ID);
                    bigram_logp.push(-0.1);
                    bigram_bow.push(-0.1);
                }
                19 => {
                    bigram_w2.extend_from_slice(&[20, 21]);
                    bigram_logp.extend_from_slice(&[-4.2, -4.0]);
                    bigram_bow.extend_from_slice(&[-0.2, -0.2]);
                }
                22 => {
                    bigram_w2.push(EOS_ID);
                    bigram_logp.push(-0.3);
                    bigram_bow.push(-0.1);
                }
                23 => {
                    bigram_w2.push(EOS_ID);
                    bigram_logp.push(-0.5);
                    bigram_bow.push(-0.1);
                }
                _ => {}
            }
        }
        bigram_offsets[n] = bigram_w2.len() as u32;

        // Trigram: (11=吃过饭, 6=了, 1=<eos>) -> -0.1
        let mut trigram_offsets = vec![0u32; bigram_w2.len() + 1];
        let mut trigram_w3 = Vec::new();
        let mut trigram_logp = Vec::new();
        for (bg_idx, &w2) in bigram_w2.iter().enumerate() {
            trigram_offsets[bg_idx] = trigram_w3.len() as u32;
            if bg_idx >= bigram_offsets[11] as usize
                && bg_idx < bigram_offsets[12] as usize
                && w2 == 6
            {
                trigram_w3.push(EOS_ID);
                trigram_logp.push(-0.1);
            }
        }
        trigram_offsets[bigram_w2.len()] = trigram_w3.len() as u32;

        Model::from_parts(
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
        )
    }

    fn build_lex_table(iter: impl Iterator<Item = (u64, LexEntry)>) -> LexTable {
        let mut items: Vec<(u64, LexEntry)> = iter.collect();
        items.sort_by_key(|&(k, _)| k);
        let mut keys = Vec::new();
        let mut ranges = vec![0u32];
        let mut entries = Vec::new();
        for (k, e) in items {
            if keys.last().copied() != Some(k) {
                if !keys.is_empty() {
                    ranges.push(entries.len() as u32);
                }
                keys.push(k);
            }
            entries.push(e);
        }
        ranges.push(entries.len() as u32);
        LexTable {
            keys,
            ranges,
            entries,
        }
    }

    #[test]
    fn commit_extends_context_and_undo_takes_it_back() {
        let mut engine = Engine::new();
        engine.set_context(vec![ContextSegment {
            text: "下雨了，".into(),
            keys: None,
        }]);
        engine.convert("womenzouba", 5);
        engine.commit("我们走吧");
        assert_eq!(engine.context_text(), "下雨了，我们走吧");
        assert!(engine.undo());
        assert_eq!(engine.context_text(), "下雨了，");
        assert!(!engine.undo(), "text the engine did not produce is not undone");
    }

    #[test]
    fn reset_clears_context() {
        let mut engine = Engine::new();
        engine.convert("le", 5);
        engine.commit("了");
        engine.reset();
        assert_eq!(engine.context_text(), "");
    }

    #[test]
    fn pinyin_dag_preserves_ambiguity_and_enforces_exact_priority() {
        let xian = build_dag("xian");
        assert!(xian.exact_only);
        assert!(xian.edges_from[0].iter().any(|e| e.end == 4));
        assert!(xian.edges_from[0].iter().any(|e| e.end == 2));
        assert!(xian
            .edges_from
            .iter()
            .flatten()
            .all(|e| matches!(e.kind, DagEdgeKind::Exact(_))));

        let xi_an = build_dag("xi'an");
        assert!(xi_an.exact_only);
        assert!(!xi_an.edges_from[0].iter().any(|e| e.end == 4));
        assert!(xi_an.edges_from[0].iter().any(|e| e.end == 2));

        let lv = build_dag("lvnv");
        assert!(lv.exact_only);
        assert_eq!(lv.edges_from[0][0].kind, DagEdgeKind::Exact(syllable_id("lv").unwrap()));
        assert_eq!(lv.edges_from[2][0].kind, DagEdgeKind::Exact(syllable_id("nv").unwrap()));

        let haod = build_dag("haod");
        assert!(!haod.exact_only);
        assert!(haod.edges_from[3]
            .iter()
            .any(|e| matches!(e.kind, DagEdgeKind::Abbrev(_))));
    }

    #[test]
    fn model_binary_roundtrip_and_decoding() {
        let model = make_toy_model();
        let tmp = std::env::temp_dir().join("ime_core_roundtrip_test.bin");
        model.save(&tmp).unwrap();
        let loaded = Arc::new(Model::load(&tmp).unwrap());
        let _ = std::fs::remove_file(&tmp);

        let mut engine = Engine::with_model(loaded.clone());
        // Ambiguous xian yields both 先 and 西安
        let cands = engine.convert("xian", 5);
        assert!(cands.contains(&"先".to_string()));
        assert!(cands.contains(&"西安".to_string()));

        // Context + EOS prefers 了 over 乐
        engine.set_context(vec![ContextSegment {
            text: "我吃过饭".into(),
            keys: Some("wochiguofan".into()),
        }]);
        let le_cands = engine.convert("le", 5);
        assert_eq!(le_cands.first().map(String::as_str), Some("了"));

        // Abbreviated tail haod -> 好的
        engine.set_context(Vec::new());
        assert_eq!(engine.convert("haod", 5).first().map(String::as_str), Some("好的"));

        // Polyphone reading_logp separates 地 (di) from 的 (de)
        assert_eq!(engine.convert("di", 5).first().map(String::as_str), Some("地"));

        // Zero-context single-letter shorthand ranks by unigram prior (`d` -> `的`, `l` -> `了`)
        // instead of `<bos> X <eos>` standalone sentence probability.
        assert_eq!(engine.convert("d", 5).first().map(String::as_str), Some("的"));
        assert_eq!(engine.convert("l", 5).first().map(String::as_str), Some("了"));

        // Punctuation resets or pauses cleanly
        assert_eq!(loaded.context_history("你好。"), (crate::model::NONE_ID, BOS_ID));
        assert_eq!(loaded.context_history("我，"), (10, PAUSE_ID));
    }

    #[test]
    fn cross_caret_word_join_and_polyphone_safety() {
        let model = Arc::new(make_toy_model());
        let mut engine = Engine::with_model(model.clone());

        // [什] + me -> 么 via joined "什么", beating higher unigram distractor "麽".
        engine.set_context(vec![ContextSegment {
            text: "什".into(),
            keys: None,
        }]);
        assert_eq!(engine.convert("me", 5).first().map(String::as_str), Some("么"));

        // [快] + le -> 乐 via joined "快乐", while [我吃过饭] + le -> 了.
        engine.set_context(vec![ContextSegment {
            text: "快".into(),
            keys: None,
        }]);
        assert_eq!(engine.convert("le", 5).first().map(String::as_str), Some("乐"));
        engine.set_context(vec![ContextSegment {
            text: "我吃过饭".into(),
            keys: None,
        }]);
        assert_eq!(engine.convert("le", 5).first().map(String::as_str), Some("了"));

        // Polyphone safety: [银] + xing must NOT merge into 银行 (yin hang) and picks 杏 via 银杏,
        // while [银] + hang picks 行 via 银行.
        engine.set_context(vec![ContextSegment {
            text: "银".into(),
            keys: None,
        }]);
        assert_eq!(engine.convert("xing", 5).first().map(String::as_str), Some("杏"));
        assert_eq!(engine.convert("hang", 5).first().map(String::as_str), Some("行"));
    }

    #[test]
    fn inline_wrapper_whitespace_and_priority_defaults() {
        let model = make_toy_model();

        // Sentence-end inside quotes still resets to BOS.
        let quote_end = model.context_state("“你好。”");
        assert_eq!((quote_end.h1, quote_end.h2), (crate::model::NONE_ID, BOS_ID));
        assert!(!quote_end.has_context);
        assert!(quote_end.tail_joins.is_empty());

        // Mid-sentence title/parentheses keeps preceding word history without cross-caret join.
        let title_ctx = model.context_state("我《雄狮》");
        assert!(title_ctx.has_context);
        assert!(title_ctx.tail_joins.is_empty());
        assert_eq!((title_ctx.h1, title_ctx.h2), (crate::model::NONE_ID, 25));

        // Leading title at start of text starts with BOS.
        let start_title = model.context_state("《雄狮》");
        assert!(start_title.has_context);
        assert_eq!((start_title.h1, start_title.h2), (BOS_ID, 25));

        // Trailing whitespace preserves LM context while disabling mid-word merge.
        let ws_ctx = model.context_state("什 ");
        assert!(ws_ctx.has_context);
        assert!(ws_ctx.tail_joins.is_empty());

        // Explicit priority config defaults: fuzzy and typo disabled so exact always wins.
        let cfg = DecoderConfig::default();
        assert!(!cfg.fuzzy_enabled);
        assert!(!cfg.typo_correction_enabled);
    }

    #[test]
    fn engine_guarded_learning_oov_undo_and_pollution_immunity() {
        let model = Arc::new(make_toy_model());
        let mut engine = Engine::with_model(model.clone());
        assert!(engine.learn_config().enabled);

        // Baseline: "kuaixing" -> "快行" ("快杏" is OOV in toy model)
        assert_eq!(engine.convert("kuaixing", 5).first().map(String::as_str), Some("快行"));

        // 1st commit of OOV "快杏" stays in staging (does NOT flip #1 yet, but surfaces at #2 for 1-key confirm).
        engine.commit("快杏");
        assert_eq!(engine.user_stats().staged_words, 1);
        assert_eq!(engine.user_stats().active_oov_words, 0);
        engine.set_context(Vec::new());
        let staged_cands = engine.convert("kuaixing", 5);
        assert_eq!(staged_cands.first().map(String::as_str), Some("快行"));
        assert_eq!(staged_cands.get(1).map(String::as_str), Some("快杏"));

        // 2nd commit promotes OOV "快杏" to #1 standalone AND inside sentences.
        engine.commit("快杏");
        assert_eq!(engine.user_stats().active_oov_words, 1);
        engine.set_context(Vec::new());
        assert_eq!(engine.convert("kuaixing", 5).first().map(String::as_str), Some("快杏"));
        assert_eq!(engine.convert("wokuaixing", 5).first().map(String::as_str), Some("我快杏"));

        // Pollution attacks: single char "乐", abbrev "haod" / "d", and "快了" (protected slot)
        // must NOT corrupt [我吃过饭] + le -> 了 or kuaile -> 快乐.
        for _ in 0..3 {
            engine.set_context(Vec::new());
            engine.convert("le", 5);
            engine.commit("乐");
            engine.set_context(Vec::new());
            engine.convert("kuaile", 5);
            engine.commit("快了");
            engine.set_context(Vec::new());
            engine.convert("kuaile", 5);
            engine.commit("快乐");
        }
        engine.set_context(vec![ContextSegment {
            text: "我吃过饭".into(),
            keys: None,
        }]);
        assert_eq!(engine.convert("le", 5).first().map(String::as_str), Some("了"));
        engine.set_context(Vec::new());
        assert_eq!(engine.convert("kuaile", 5).first().map(String::as_str), Some("快乐"));

        // Undo demotes promoted "快杏" immediately back to "快行".
        engine.convert("kuaixing", 5);
        engine.commit("快杏");
        assert!(engine.undo());
        engine.set_context(Vec::new());
        assert_eq!(engine.convert("kuaixing", 5).first().map(String::as_str), Some("快行"));

        // Re-learn "快杏" (with 1 strike, requires 3 confirms), save & reload user dict.
        for _ in 0..3 {
            engine.set_context(Vec::new());
            engine.convert("kuaixing", 5);
            engine.commit("快杏");
        }
        engine.set_context(Vec::new());
        assert_eq!(engine.convert("kuaixing", 5).first().map(String::as_str), Some("快杏"));

        let tmp = std::env::temp_dir().join("ime_engine_user_dict_test.tsv");
        engine.save_user_dict(&tmp).unwrap();
        let mut engine2 = Engine::with_model(model);
        engine2.load_user_dict(&tmp).unwrap();
        let _ = std::fs::remove_file(&tmp);
        assert_eq!(engine2.convert("kuaixing", 5).first().map(String::as_str), Some("快杏"));
    }

    fn make_mixed_abbrev_toy_model() -> Model {
        let words = [
            "<bos>", "<eos>", "<pause>", "硬", "盘", "硬盘", "计", "算", "机", "计算机", "时",
            "间", "时间", "开", "始", "开始", "中", "华", "中华", "固", "态", "固态硬盘",
        ];
        let mut word_offsets = Vec::with_capacity(words.len() + 1);
        let mut word_bytes = Vec::new();
        for w in &words {
            word_offsets.push(word_bytes.len() as u32);
            word_bytes.extend_from_slice(w.as_bytes());
        }
        word_offsets.push(word_bytes.len() as u32);

        let n = words.len();
        let mut unigram_logp = vec![-8.0_f32; n];
        unigram_logp[EOS_ID as usize] = -2.0;
        unigram_logp[PAUSE_ID as usize] = -2.5;
        unigram_logp[3] = -5.5; // 硬
        unigram_logp[4] = -5.0; // 盘
        unigram_logp[5] = -2.0; // 硬盘
        unigram_logp[6] = -5.5; // 计
        unigram_logp[7] = -5.5; // 算
        unigram_logp[8] = -5.5; // 机
        unigram_logp[9] = -1.8; // 计算机
        unigram_logp[10] = -5.5; // 时
        unigram_logp[11] = -5.5; // 间
        unigram_logp[12] = -1.7; // 时间
        unigram_logp[13] = -5.5; // 开
        unigram_logp[14] = -5.5; // 始
        unigram_logp[15] = -1.7; // 开始
        unigram_logp[16] = -5.5; // 中
        unigram_logp[17] = -5.5; // 华
        unigram_logp[18] = -2.2; // 中华
        unigram_logp[19] = -5.5; // 固
        unigram_logp[20] = -5.5; // 态
        unigram_logp[21] = -2.5; // 固态硬盘
        let unigram_bow = vec![-0.2_f32; n];

        let ying = syllable_id("ying").unwrap();
        let pan = syllable_id("pan").unwrap();
        let ji = syllable_id("ji").unwrap();
        let suan = syllable_id("suan").unwrap();
        let shi = syllable_id("shi").unwrap();
        let jian = syllable_id("jian").unwrap();
        let kai = syllable_id("kai").unwrap();
        let zhong = syllable_id("zhong").unwrap();
        let hua = syllable_id("hua").unwrap();
        let gu = syllable_id("gu").unwrap();
        let tai = syllable_id("tai").unwrap();

        let exact = build_lex_table(
            vec![
                (pack_exact(&[ying]).unwrap(), LexEntry { word_id: 3, reading_logp: 0.0 }),
                (pack_exact(&[pan]).unwrap(), LexEntry { word_id: 4, reading_logp: 0.0 }),
                (pack_exact(&[ying, pan]).unwrap(), LexEntry { word_id: 5, reading_logp: 0.0 }),
                (pack_exact(&[ji]).unwrap(), LexEntry { word_id: 6, reading_logp: -0.2 }),
                (pack_exact(&[suan]).unwrap(), LexEntry { word_id: 7, reading_logp: 0.0 }),
                (pack_exact(&[ji]).unwrap(), LexEntry { word_id: 8, reading_logp: -0.5 }),
                (pack_exact(&[ji, suan, ji]).unwrap(), LexEntry { word_id: 9, reading_logp: 0.0 }),
                (pack_exact(&[shi]).unwrap(), LexEntry { word_id: 10, reading_logp: 0.0 }),
                (pack_exact(&[jian]).unwrap(), LexEntry { word_id: 11, reading_logp: 0.0 }),
                (pack_exact(&[shi, jian]).unwrap(), LexEntry { word_id: 12, reading_logp: 0.0 }),
                (pack_exact(&[kai]).unwrap(), LexEntry { word_id: 13, reading_logp: 0.0 }),
                (pack_exact(&[shi]).unwrap(), LexEntry { word_id: 14, reading_logp: -0.3 }),
                (pack_exact(&[kai, shi]).unwrap(), LexEntry { word_id: 15, reading_logp: 0.0 }),
                (pack_exact(&[zhong]).unwrap(), LexEntry { word_id: 16, reading_logp: 0.0 }),
                (pack_exact(&[hua]).unwrap(), LexEntry { word_id: 17, reading_logp: 0.0 }),
                (pack_exact(&[zhong, hua]).unwrap(), LexEntry { word_id: 18, reading_logp: 0.0 }),
                (pack_exact(&[gu]).unwrap(), LexEntry { word_id: 19, reading_logp: 0.0 }),
                (pack_exact(&[tai]).unwrap(), LexEntry { word_id: 20, reading_logp: 0.0 }),
                (
                    pack_exact(&[gu, tai, ying, pan]).unwrap(),
                    LexEntry { word_id: 21, reading_logp: 0.0 },
                ),
            ]
            .into_iter(),
        );

        let mut initial_chars: [Vec<LexEntry>; 26] = std::array::from_fn(|_| Vec::new());
        initial_chars[(b'y' - b'a') as usize] = vec![LexEntry { word_id: 3, reading_logp: 0.0 }];
        initial_chars[(b'j' - b'a') as usize] = vec![LexEntry { word_id: 6, reading_logp: 0.0 }];
        initial_chars[(b's' - b'a') as usize] = vec![LexEntry { word_id: 10, reading_logp: 0.0 }];
        initial_chars[(b'z' - b'a') as usize] = vec![LexEntry { word_id: 16, reading_logp: 0.0 }];
        initial_chars[(b'h' - b'a') as usize] = vec![LexEntry { word_id: 17, reading_logp: 0.0 }];

        let abbrev_tail = build_lex_table(
            [(
                pack_abbrev_tail(&[ying], b'p' - b'a').unwrap(),
                LexEntry { word_id: 5, reading_logp: 0.0 },
            )]
            .into_iter(),
        );
        let abbrev_init = build_lex_table(
            [
                (
                    pack_initials(&[b'y' - b'a', b'p' - b'a']).unwrap(),
                    LexEntry { word_id: 5, reading_logp: 0.0 },
                ),
                (
                    pack_initials(&[b'z' - b'a', b'h' - b'a']).unwrap(),
                    LexEntry { word_id: 18, reading_logp: 0.0 },
                ),
            ]
            .into_iter(),
        );

        let mut bigram_offsets = vec![0u32; n + 1];
        let mut bigram_w2 = Vec::new();
        let mut bigram_logp = Vec::new();
        let mut bigram_bow = Vec::new();
        for w1 in 0..n {
            bigram_offsets[w1] = bigram_w2.len() as u32;
            match w1 {
                0 => {
                    bigram_w2.extend_from_slice(&[5, 9, 12, 15, 18, 21]);
                    bigram_logp.extend_from_slice(&[-1.5, -1.2, -1.1, -1.1, -1.5, -1.5]);
                    bigram_bow.extend_from_slice(&[-0.1; 6]);
                }
                5 | 9 | 12 | 15 | 18 | 21 => {
                    bigram_w2.push(EOS_ID);
                    bigram_logp.push(-0.2);
                    bigram_bow.push(-0.1);
                }
                _ => {}
            }
        }
        bigram_offsets[n] = bigram_w2.len() as u32;
        let trigram_offsets = vec![0u32; bigram_w2.len() + 1];

        Model::from_parts(
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
            Vec::new(),
            Vec::new(),
        )
    }

    #[test]
    fn mixed_front_abbrev_digraphs_and_cross_caret_join() {
        use crate::pinyin::DigraphInitial;

        let shjian = build_dag("shjian");
        assert!(!shjian.exact_only);
        assert!(shjian.edges_from[0]
            .iter()
            .any(|e| e.end == 2 && e.kind == DagEdgeKind::Abbrev2(DigraphInitial::Sh)));

        let z_h = build_dag("z'h");
        assert!(!z_h.exact_only);
        assert!(z_h.edges_from[0]
            .iter()
            .all(|e| !matches!(e.kind, DagEdgeKind::Abbrev2(_))));

        // Full pinyin remains strictly exact-only without Abbrev/Abbrev2 edges.
        for exact_input in ["xian", "yuan", "zhou", "shuo"] {
            let dag = build_dag(exact_input);
            assert!(dag.exact_only, "{exact_input} should remain exact_only");
            assert!(dag
                .edges_from
                .iter()
                .flatten()
                .all(|e| matches!(e.kind, DagEdgeKind::Exact(_))));
        }

        let model = Arc::new(make_mixed_abbrev_toy_model());
        let mut engine = Engine::with_model(model);

        // Front-abbr + back-full matches symmetrically with back-abbr.
        assert_eq!(engine.convert("ypan", 5).first().map(String::as_str), Some("硬盘"));
        assert_eq!(engine.convert("yingp", 5).first().map(String::as_str), Some("硬盘"));

        // 3-char mixed: [A, E, E], [E, A, E], [A, A, E]
        assert_eq!(engine.convert("jsuanji", 5).first().map(String::as_str), Some("计算机"));
        assert_eq!(engine.convert("jisji", 5).first().map(String::as_str), Some("计算机"));
        assert_eq!(engine.convert("jsji", 5).first().map(String::as_str), Some("计算机"));

        // Digraph initials zh/ch/sh in [A2, E] and [E, A2] without breaking standalone `zh`.
        assert_eq!(engine.convert("shjian", 5).first().map(String::as_str), Some("时间"));
        assert_eq!(engine.convert("kaish", 5).first().map(String::as_str), Some("开始"));
        assert_eq!(engine.convert("zh", 5).first().map(String::as_str), Some("中华"));

        // Cross-caret join with SpanPron::Mixed: [固态] + ypan -> 硬盘
        let mut alt_model = make_mixed_abbrev_toy_model();
        // Give standalone 硬盘 worse <bos> score so joined 固态硬盘 wins when context is 固态.
        let pan_id = alt_model.word_id("硬盘").unwrap();
        let pan_bg = alt_model.bigram_index(BOS_ID, pan_id).unwrap();
        alt_model.bigram_logp[pan_bg] = -9.0;
        let mut engine2 = Engine::with_model(Arc::new(alt_model));
        engine2.set_context(vec![ContextSegment {
            text: "固态".into(),
            keys: None,
        }]);
        assert_eq!(engine2.convert("ypan", 5).first().map(String::as_str), Some("硬盘"));
    }
}

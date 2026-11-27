//! Beam search pinyin-to-Chinese decoder over a `Model` and `PinyinDag`.

use std::collections::HashSet;

use crate::model::{ContextState, LexEntry, Model, EOS_ID, NONE_ID};
use crate::pinyin::{
    build_dag, pack_abbrev_tail, pack_exact, pack_initials, syllable_matches_digraph,
    DagEdgeKind, PinyinDag, SpanPron, SYLLABLES,
};
use crate::reranker::Reranker;
use crate::user_dict::{ActiveKeyMatch, UserMemory};

#[derive(Debug, Clone, Copy)]
pub struct DecoderConfig {
    pub beam_width: usize,
    pub max_per_state: usize,
    pub max_span_exact_char: usize,
    pub max_span_abbrev_char: usize,
    pub max_span_word: usize,
    pub eos_weight: f32,
    pub short_nocontext_eos_weight: f32,
    pub reading_weight: f32,
    pub word_penalty: f32,
    pub abbrev_penalty: f32,
    pub neural_weight: f32,
    pub neural_eos_weight: f32,
    pub rerank_top_k: usize,
    pub fuzzy_enabled: bool,
    pub typo_correction_enabled: bool,
    pub cs_ai_boost: bool,
}

impl Default for DecoderConfig {
    fn default() -> Self {
        Self {
            beam_width: 48,
            max_per_state: 2,
            max_span_exact_char: 24,
            max_span_abbrev_char: 24,
            max_span_word: 12,
            eos_weight: 0.85,
            short_nocontext_eos_weight: 0.85,
            reading_weight: 1.0,
            word_penalty: 0.15,
            abbrev_penalty: 6.0,
            neural_weight: 0.45,
            neural_eos_weight: 0.85,
            rerank_top_k: 10,
            fuzzy_enabled: false,
            typo_correction_enabled: false,
            cs_ai_boost: false,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct DecodeOutcome {
    pub candidates: Vec<String>,
    pub base_top1: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CandidateKind {
    #[default]
    Full,
    PrefixWord,
    PrefixChar,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateItem {
    pub text: String,
    pub consumed_len: usize,
    pub kind: CandidateKind,
}

#[derive(Debug, Clone, Default)]
pub struct RichDecodeOutcome {
    pub items: Vec<CandidateItem>,
    pub base_top1: Option<String>,
    pub segmented_preedit: String,
    pub first_char_offset: Option<usize>,
    pub first_syllable_len: usize,
}

#[derive(Debug, Clone, Copy)]
enum SpanWord {
    Model(u32),
    UserOov(usize),
}

#[derive(Debug, Clone, Copy)]
struct SpanEdge {
    start: usize,
    word: SpanWord,
    reading_logp: f32,
    abbrev_count: u8,
    pron: SpanPron,
    user_boost: f32,
    domain_boost: f32,
}

#[derive(Debug, Clone, Copy)]
struct BackNode {
    parent: u32,
    word: SpanWord,
    start: u16,
    end: u16,
    pron: SpanPron,
}

#[derive(Debug, Clone, Copy)]
struct Hypothesis {
    lm_score: f32,
    acoustic_score: f32,
    user_score: f32,
    has_oov: bool,
    h2: u32,
    tri_start: u32,
    tri_end: u32,
    bg_bow: f32,
    node_idx: u32,
}

impl Hypothesis {
    #[inline]
    fn total_score(&self) -> f32 {
        self.lm_score + self.acoustic_score + self.user_score
    }
}

#[derive(Debug, Clone, Copy)]
struct RawHyp {
    lm_score: f32,
    acoustic_score: f32,
    user_score: f32,
    has_oov: bool,
    h1: u32,
    h2: u32,
    parent_node: u32,
    word: SpanWord,
    span_start: u16,
    span_end: u16,
    pron: SpanPron,
}

impl RawHyp {
    #[inline]
    fn total_score(&self) -> f32 {
        self.lm_score + self.acoustic_score + self.user_score
    }
}

fn materialize_chain(
    nodes: &[BackNode],
    mut cur: u32,
    tail_word: Option<SpanWord>,
    model: &Model,
    oov_pool: &[String],
    rev_buf: &mut Vec<SpanWord>,
    out: &mut String,
) {
    rev_buf.clear();
    if let Some(w) = tail_word {
        rev_buf.push(w);
    }
    while cur != u32::MAX {
        let node = nodes[cur as usize];
        rev_buf.push(node.word);
        cur = node.parent;
    }
    out.clear();
    for &w in rev_buf.iter().rev() {
        let s = match w {
            SpanWord::Model(id) => model.word_str(id),
            SpanWord::UserOov(idx) => oov_pool[idx].as_str(),
        };
        out.push_str(s);
    }
}

struct BeamScratch {
    kept_raw: Vec<RawHyp>,
    rev_buf: Vec<SpanWord>,
    text_slots: Vec<String>,
    cmp_buf: String,
}

impl BeamScratch {
    fn new(dag_len: usize, beam_width: usize, max_per_state: usize) -> Self {
        Self {
            kept_raw: Vec::with_capacity(beam_width * 4),
            rev_buf: Vec::with_capacity(dag_len + 2),
            text_slots: (0..max_per_state.saturating_sub(1).max(1))
                .map(|_| String::with_capacity(32))
                .collect(),
            cmp_buf: String::with_capacity(32),
        }
    }
}

struct ScoredCandidate {
    text: String,
    lm_score: f32,
    acoustic_score: f32,
    user_score: f32,
    has_oov: bool,
}

fn effective_eos_weight(dag: &PinyinDag, has_context: bool, cfg: &DecoderConfig) -> f32 {
    if has_context || (cfg.short_nocontext_eos_weight - cfg.eos_weight).abs() < 1e-6 {
        return cfg.eos_weight;
    }
    let mut dist = vec![usize::MAX; dag.len + 1];
    dist[0] = 0;
    for i in 0..dag.len {
        let d = dist[i];
        if d == usize::MAX {
            continue;
        }
        let next_d = d + 1;
        for edge in &dag.edges_from[i] {
            if next_d < dist[edge.end] {
                dist[edge.end] = next_d;
            }
        }
    }
    if dist[dag.len] <= 3 {
        cfg.short_nocontext_eos_weight
    } else {
        cfg.eos_weight
    }
}

pub fn decode(
    model: &Model,
    keys: &str,
    context_text: &str,
    limit: usize,
    cfg: &DecoderConfig,
) -> Vec<String> {
    decode_with_user(model, None, None, keys, context_text, limit, cfg).candidates
}

pub fn decode_with_reranker(
    model: &Model,
    reranker: Option<&Reranker>,
    keys: &str,
    context_text: &str,
    limit: usize,
    cfg: &DecoderConfig,
) -> Vec<String> {
    decode_with_user(model, reranker, None, keys, context_text, limit, cfg).candidates
}

pub fn decode_with_user(
    model: &Model,
    reranker: Option<&Reranker>,
    user: Option<&UserMemory>,
    keys: &str,
    context_text: &str,
    limit: usize,
    cfg: &DecoderConfig,
) -> DecodeOutcome {
    if limit == 0 || keys.is_empty() {
        return DecodeOutcome::default();
    }
    let dag = build_dag(keys);
    if dag.len == 0 {
        return DecodeOutcome::default();
    }

    let active_user = user.filter(|u| !u.is_empty() && u.config().enabled);
    let ctx = model.context_state(context_text);
    let eff_eos = effective_eos_weight(&dag, ctx.has_context, cfg);

    // Fast path when the entire input is unambiguously one syllable or one single-letter abbrev.
    if let Some(outcome) =
        try_decode_single_unit(model, reranker, &dag, &ctx, context_text, eff_eos, limit, cfg)
    {
        return outcome;
    }

    let (spans_to, oov_pool) = collect_spans(model, &dag, active_user, cfg);
    let beam_res = run_beam_search(model, &dag, &ctx, active_user, &spans_to, &oov_pool, cfg);
    let finals = &beam_res.beams[dag.len];
    if finals.is_empty() {
        return DecodeOutcome::default();
    }
    let mut rev_buf = Vec::with_capacity(dag.len + 2);
    let scored = score_beam_candidates(
        model,
        &beam_res.nodes,
        &oov_pool,
        finals,
        eff_eos,
        &mut rev_buf,
    );
    let mut outcome = finalize_candidates(reranker, context_text, scored, limit, cfg);
    apply_staged_oov_for_dag(&dag, active_user, limit, &mut outcome);
    outcome
}

pub fn decode_rich_with_user(
    model: &Model,
    reranker: Option<&Reranker>,
    user: Option<&UserMemory>,
    keys: &str,
    context_text: &str,
    limit: usize,
    cfg: &DecoderConfig,
) -> RichDecodeOutcome {
    if limit == 0 || keys.is_empty() {
        return RichDecodeOutcome::default();
    }
    let dag = build_dag(keys);
    if dag.len == 0 {
        return RichDecodeOutcome::default();
    }

    let active_user = user.filter(|u| !u.is_empty() && u.config().enabled);
    let ctx = model.context_state(context_text);
    let eff_eos = effective_eos_weight(&dag, ctx.has_context, cfg);

    // 1. Unambiguous single syllable or single-letter abbrev.
    if let Some(outcome) =
        try_decode_single_unit(model, reranker, &dag, &ctx, context_text, eff_eos, limit, cfg)
    {
        let items = outcome
            .candidates
            .into_iter()
            .map(|text| CandidateItem {
                text,
                consumed_len: keys.len(),
                kind: CandidateKind::Full,
            })
            .collect();
        return RichDecodeOutcome {
            items,
            base_top1: outcome.base_top1,
            segmented_preedit: keys.to_owned(),
            first_char_offset: Some(0),
            first_syllable_len: keys.len(),
        };
    }

    let (spans_to, oov_pool) = collect_spans(model, &dag, active_user, cfg);

    // 2. Single syllable that also allows secondary segmentation (e.g. jia, chuang, xiang, xian):
    // return ALL exact single-char readings + genuine multi-char lexicon/OOV words without 24-char truncation
    // or synthetic two-char joins (such as 急啊 / 除昂 / 西昂).
    if let Some(full_syl) = dag.edges_from[0].iter().find_map(|e| {
        if e.end == dag.len {
            if let DagEdgeKind::Exact(syl) = e.kind {
                return Some(syl);
            }
        }
        None
    }) {
        let mut outcome = decode_full_single_syllable_with_lex_words(
            model,
            reranker,
            &dag,
            &ctx,
            context_text,
            eff_eos,
            full_syl,
            &spans_to[dag.len],
            &oov_pool,
            active_user,
            limit,
            cfg,
        );
        apply_staged_oov_for_dag(&dag, active_user, limit, &mut outcome);
        let items = outcome
            .candidates
            .into_iter()
            .map(|text| CandidateItem {
                text,
                consumed_len: keys.len(),
                kind: CandidateKind::Full,
            })
            .collect();
        return RichDecodeOutcome {
            items,
            base_top1: outcome.base_top1,
            segmented_preedit: keys.to_owned(),
            first_char_offset: Some(0),
            first_syllable_len: keys.len(),
        };
    }

    // 3. Multi-syllable input: run Beam Search once and assemble 3-tier cascade.
    let beam_res = run_beam_search(model, &dag, &ctx, active_user, &spans_to, &oov_pool, cfg);
    let mut rev_buf = Vec::with_capacity(dag.len + 2);
    let finals = &beam_res.beams[dag.len];

    let syl_bounds = extract_best_syllable_bounds(model, &dag, &beam_res);
    let seg_ends: Vec<usize> = syl_bounds.iter().map(|&(_, e)| e).collect();
    let segmented_preedit = format_segmented_preedit(keys, &dag, &seg_ends);
    let primary_first_end = syl_bounds
        .first()
        .map(|&(_, e)| e)
        .or_else(|| {
            dag.edges_from[0]
                .iter()
                .filter(|e| matches!(e.kind, DagEdgeKind::Exact(_)) || !dag.exact_only)
                .map(|e| e.end)
                .max()
        })
        .unwrap_or(1)
        .min(dag.len);
    let first_syllable_len = dag.raw_consumed_len(primary_first_end);

    let mut items: Vec<CandidateItem> = Vec::with_capacity(limit.min(64));
    let mut seen_items: HashSet<(String, usize)> = HashSet::new();

    // Tier 1: concise full-span candidates.
    let mut base_top1 = None;
    if !finals.is_empty() {
        let scored = score_beam_candidates(
            model,
            &beam_res.nodes,
            &oov_pool,
            finals,
            eff_eos,
            &mut rev_buf,
        );
        let full_want = limit.min(10).max(3);
        let mut full_outcome = finalize_candidates(reranker, context_text, scored, full_want, cfg);
        let had_staged = apply_staged_oov_for_dag(&dag, active_user, full_want, &mut full_outcome);
        base_top1 = full_outcome.base_top1;

        let has_full_lex_word = had_staged
            || spans_to[dag.len].iter().any(|sp| {
                sp.start == 0
                    && match sp.word {
                        SpanWord::Model(wid) => model.word_str(wid).chars().count() >= 2,
                        SpanWord::UserOov(_) => true,
                    }
            });
        let max_full = if has_full_lex_word { 3 } else { 2 };
        for text in full_outcome.candidates.into_iter().take(max_full) {
            if items.len() >= limit {
                break;
            }
            if seen_items.insert((text.clone(), keys.len())) {
                items.push(CandidateItem {
                    text,
                    consumed_len: keys.len(),
                    kind: CandidateKind::Full,
                });
            }
        }
    }

    // Tier 2: prefix words ordered longest-to-shortest.
    const MAX_PREFIX_WORDS_TOTAL: usize = 8;
    let mut prefix_words_added = 0usize;
    let mut prefix_ends: Vec<(usize, usize)> = Vec::new();
    for (idx, &(_, end)) in syl_bounds.iter().enumerate() {
        let syl_count = idx + 1;
        if syl_count >= 2 && end > primary_first_end && end < dag.len {
            prefix_ends.push((end, syl_count));
        }
    }
    prefix_ends.sort_by(|a, b| b.0.cmp(&a.0));
    prefix_ends.dedup_by_key(|x| x.0);

    for (end, syl_count) in prefix_ends {
        if items.len() >= limit || prefix_words_added >= MAX_PREFIX_WORDS_TOTAL {
            break;
        }
        let consumed = dag.raw_consumed_len(end);
        if consumed == 0 || consumed >= keys.len() {
            continue;
        }
        let per_len_cap = if syl_count == 2 { 5 } else { 3 };
        let mut added_for_len = 0usize;

        let mut direct_scored: Vec<(f32, String)> = Vec::new();
        for sp in &spans_to[end] {
            if sp.start != 0 || sp.abbrev_count > 0 {
                continue;
            }
            let (text, score) = match sp.word {
                SpanWord::Model(wid) => {
                    let w = model.word_str(wid);
                    if w.chars().count() < 2 {
                        continue;
                    }
                    let lm = model.trigram_logp(ctx.h1, ctx.h2, wid);
                    let bg_boost = active_user.map_or(0.0, |u| u.active_bigram_boost(ctx.h2, w));
                    let s = lm
                        + cfg.reading_weight * sp.reading_logp
                        + sp.domain_boost
                        + sp.user_boost
                        + bg_boost;
                    (w.to_owned(), s)
                }
                SpanWord::UserOov(idx) => {
                    let w = &oov_pool[idx];
                    let lm = model.oov_transition_logp(ctx.h2);
                    let bg_boost = active_user.map_or(0.0, |u| u.active_bigram_boost(ctx.h2, w));
                    (w.clone(), lm + sp.user_boost + bg_boost)
                }
            };
            direct_scored.push((score, text));
        }
        direct_scored.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        for (_, text) in direct_scored {
            if items.len() >= limit
                || prefix_words_added >= MAX_PREFIX_WORDS_TOTAL
                || added_for_len >= per_len_cap
            {
                break;
            }
            if seen_items.insert((text.clone(), consumed)) {
                items.push(CandidateItem {
                    text,
                    consumed_len: consumed,
                    kind: CandidateKind::PrefixWord,
                });
                prefix_words_added += 1;
                added_for_len += 1;
            }
        }

        if added_for_len == 0 && syl_count == 2 && !beam_res.beams[end].is_empty() {
            let mut beam_cands: Vec<(f32, String)> = beam_res.beams[end]
                .iter()
                .map(|hyp| {
                    let mut text = String::new();
                    materialize_chain(
                        &beam_res.nodes,
                        hyp.node_idx,
                        None,
                        model,
                        &oov_pool,
                        &mut rev_buf,
                        &mut text,
                    );
                    (hyp.total_score(), text)
                })
                .collect();
            beam_cands.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            for (_, text) in beam_cands {
                if items.len() >= limit
                    || prefix_words_added >= MAX_PREFIX_WORDS_TOTAL
                    || added_for_len >= 2
                {
                    break;
                }
                if seen_items.insert((text.clone(), consumed)) {
                    items.push(CandidateItem {
                        text,
                        consumed_len: consumed,
                        kind: CandidateKind::PrefixWord,
                    });
                    prefix_words_added += 1;
                    added_for_len += 1;
                }
            }
        }
    }

    // Tier 3: full first-syllable single-character candidates (no 24-char truncation).
    let char_offset = items.len();
    let mut char_groups: Vec<(usize, Vec<ScoredCandidate>)> = Vec::new();
    let mut primary_cands: Vec<ScoredCandidate> = Vec::new();
    for edge in &dag.edges_from[0] {
        if edge.end == primary_first_end {
            collect_single_char_entries_for_edge(
                model,
                &ctx,
                edge.kind,
                cfg,
                &mut primary_cands,
            );
        }
    }
    if !primary_cands.is_empty() {
        char_groups.push((first_syllable_len, primary_cands));
    }

    // Ambiguous alternative first syllables (e.g. `xi` when primary is `xian` in `xianren`).
    let mut alt_edges: Vec<(usize, DagEdgeKind)> = dag.edges_from[0]
        .iter()
        .filter(|e| {
            e.end != primary_first_end
                && e.end < dag.len
                && matches!(e.kind, DagEdgeKind::Exact(_))
                && !dag.edges_from[e.end].is_empty()
        })
        .map(|e| (e.end, e.kind))
        .collect();
    alt_edges.sort_by(|a, b| b.0.cmp(&a.0));
    for (alt_end, alt_kind) in alt_edges {
        let mut alt_cands = Vec::new();
        collect_single_char_entries_for_edge(model, &ctx, alt_kind, cfg, &mut alt_cands);
        if !alt_cands.is_empty() {
            char_groups.push((dag.raw_consumed_len(alt_end), alt_cands));
        }
    }

    for (consumed, mut group) in char_groups {
        if items.len() >= limit {
            break;
        }
        group.sort_by(|a, b| {
            (b.lm_score + b.acoustic_score + b.user_score)
                .total_cmp(&(a.lm_score + a.acoustic_score + a.user_score))
        });
        let mut seen_in_group = HashSet::new();
        for cand in group {
            if items.len() >= limit {
                break;
            }
            if !seen_in_group.insert(cand.text.clone()) {
                continue;
            }
            if seen_items.insert((cand.text.clone(), consumed)) {
                items.push(CandidateItem {
                    text: cand.text,
                    consumed_len: consumed,
                    kind: CandidateKind::PrefixChar,
                });
            }
        }
    }

    let first_char_offset = if char_offset < items.len() {
        Some(char_offset)
    } else {
        None
    };

    RichDecodeOutcome {
        items,
        base_top1,
        segmented_preedit,
        first_char_offset,
        first_syllable_len,
    }
}

struct BeamSearchResult {
    beams: Vec<Vec<Hypothesis>>,
    nodes: Vec<BackNode>,
}

fn run_beam_search(
    model: &Model,
    dag: &PinyinDag,
    ctx: &ContextState,
    active_user: Option<&UserMemory>,
    spans_to: &[Vec<SpanEdge>],
    oov_pool: &[String],
    cfg: &DecoderConfig,
) -> BeamSearchResult {
    let mut beams: Vec<Vec<Hypothesis>> = vec![Vec::new(); dag.len + 1];
    let (init_tri_start, init_tri_end, init_bg_bow) = model.trigram_meta(ctx.h1, ctx.h2);
    beams[0].push(Hypothesis {
        lm_score: 0.0,
        acoustic_score: 0.0,
        user_score: 0.0,
        has_oov: false,
        h2: ctx.h2,
        tri_start: init_tri_start,
        tri_end: init_tri_end,
        bg_bow: init_bg_bow,
        node_idx: u32::MAX,
    });
    let mut nodes: Vec<BackNode> = Vec::with_capacity(dag.len * cfg.beam_width);
    let mut raw: Vec<RawHyp> = Vec::with_capacity(cfg.beam_width * 16);
    let mut scratch = BeamScratch::new(dag.len, cfg.beam_width, cfg.max_per_state);
    let mut joined_buf = String::with_capacity(24);

    for end in 1..=dag.len {
        let edges = &spans_to[end];
        if edges.is_empty() {
            continue;
        }
        raw.clear();
        for span in edges {
            let prev_hyps = &beams[span.start];
            match span.word {
                SpanWord::Model(word_id) => {
                    let word = model.word_str(word_id);
                    if word.is_empty() {
                        continue;
                    }
                    if !prev_hyps.is_empty() {
                        let local_acoustic = cfg.reading_weight * span.reading_logp
                            + span.domain_boost
                            - cfg.word_penalty
                            - (span.abbrev_count as f32) * cfg.abbrev_penalty;
                        let word_uni = model
                            .unigram_logp
                            .get(word_id as usize)
                            .copied()
                            .unwrap_or(-20.0);
                        let mut last_h2 = u32::MAX;
                        let mut has_cached_bg = false;
                        let mut cached_bg_logp = 0.0_f32;
                        for prev in prev_hyps {
                            let lm_step = if let Some(tlp) =
                                model.trigram_lookup_range(prev.tri_start, prev.tri_end, word_id)
                            {
                                tlp
                            } else {
                                if !has_cached_bg || prev.h2 != last_h2 {
                                    last_h2 = prev.h2;
                                    cached_bg_logp =
                                        model.bigram_logp_known_uni(prev.h2, word_id, word_uni);
                                    has_cached_bg = true;
                                }
                                prev.bg_bow + cached_bg_logp
                            };
                            let bg_boost = if span.abbrev_count == 0 {
                                active_user.map_or(0.0, |u| u.active_bigram_boost(prev.h2, word))
                            } else {
                                0.0
                            };
                            raw.push(RawHyp {
                                lm_score: prev.lm_score + lm_step,
                                acoustic_score: prev.acoustic_score + local_acoustic,
                                user_score: prev.user_score + span.user_boost + bg_boost,
                                has_oov: prev.has_oov,
                                h1: prev.h2,
                                h2: word_id,
                                parent_node: prev.node_idx,
                                word: SpanWord::Model(word_id),
                                span_start: span.start as u16,
                                span_end: end as u16,
                                pron: span.pron,
                            });
                        }
                    }

                    if span.start == 0 && !ctx.tail_joins.is_empty() {
                        let span_chars = word.chars().count();
                        for tail in &ctx.tail_joins {
                            if tail.tail_chars + span_chars > 6 {
                                continue;
                            }
                            joined_buf.clear();
                            joined_buf.push_str(&tail.tail_text);
                            joined_buf.push_str(word);
                            if let Some(joined_id) = model.word_id(&joined_buf) {
                                if let Some(joined_rp) =
                                    model.match_joined_reading(joined_id, tail.tail_chars, &span.pron)
                                {
                                    let lm_score =
                                        model.trigram_logp(tail.pre_h1, tail.pre_h2, joined_id)
                                            - tail.tail_lm_score;
                                    let joined_domain = if cfg.cs_ai_boost && span.abbrev_count == 0 {
                                        model.cs_ai_boost(joined_id)
                                    } else {
                                        0.0
                                    };
                                    let acoustic_score = cfg.reading_weight * joined_rp
                                        + joined_domain
                                        - cfg.word_penalty
                                        - (span.abbrev_count as f32) * cfg.abbrev_penalty;
                                    raw.push(RawHyp {
                                        lm_score,
                                        acoustic_score,
                                        user_score: 0.0,
                                        has_oov: false,
                                        h1: tail.pre_h2,
                                        h2: joined_id,
                                        parent_node: u32::MAX,
                                        word: SpanWord::Model(word_id),
                                        span_start: 0,
                                        span_end: end as u16,
                                        pron: span.pron,
                                    });
                                }
                            }
                        }
                    }
                }
                SpanWord::UserOov(oov_idx) => {
                    if prev_hyps.is_empty() {
                        continue;
                    }
                    let word = oov_pool[oov_idx].as_str();
                    let local_acoustic = -cfg.word_penalty;
                    for prev in prev_hyps {
                        let lm_step = model.oov_transition_logp(prev.h2);
                        let bg_boost =
                            active_user.map_or(0.0, |u| u.active_bigram_boost(prev.h2, word));
                        raw.push(RawHyp {
                            lm_score: prev.lm_score + lm_step,
                            acoustic_score: prev.acoustic_score + local_acoustic,
                            user_score: prev.user_score + span.user_boost + bg_boost,
                            has_oov: true,
                            h1: prev.h2,
                            h2: NONE_ID,
                            parent_node: prev.node_idx,
                            word: SpanWord::UserOov(oov_idx),
                            span_start: span.start as u16,
                            span_end: end as u16,
                            pron: span.pron,
                        });
                    }
                }
            }
        }
        if raw.is_empty() {
            continue;
        }
        beams[end] = prune_beam(
            model,
            oov_pool,
            &mut nodes,
            &mut raw,
            &mut scratch,
            cfg.beam_width,
            cfg.max_per_state,
        );
    }

    BeamSearchResult { beams, nodes }
}

fn score_beam_candidates(
    model: &Model,
    nodes: &[BackNode],
    oov_pool: &[String],
    finals: &[Hypothesis],
    eff_eos: f32,
    rev_buf: &mut Vec<SpanWord>,
) -> Vec<ScoredCandidate> {
    let eos_uni = model
        .unigram_logp
        .get(EOS_ID as usize)
        .copied()
        .unwrap_or(-20.0);
    finals
        .iter()
        .map(|hyp| {
            let eos_term = if hyp.h2 == NONE_ID {
                0.0
            } else if let Some(tlp) = model.trigram_lookup_range(hyp.tri_start, hyp.tri_end, EOS_ID)
            {
                eff_eos * tlp
            } else {
                eff_eos * (hyp.bg_bow + model.bigram_logp_known_uni(hyp.h2, EOS_ID, eos_uni))
            };
            let mut text = String::new();
            materialize_chain(
                nodes,
                hyp.node_idx,
                None,
                model,
                oov_pool,
                rev_buf,
                &mut text,
            );
            ScoredCandidate {
                lm_score: hyp.lm_score + eos_term,
                acoustic_score: hyp.acoustic_score,
                user_score: hyp.user_score,
                has_oov: hyp.has_oov,
                text,
            }
        })
        .collect()
}

fn apply_staged_oov_for_dag(
    dag: &PinyinDag,
    active_user: Option<&UserMemory>,
    limit: usize,
    outcome: &mut DecodeOutcome,
) -> bool {
    if let Some(user_mem) = active_user {
        if limit >= 2 && !outcome.candidates.is_empty() && dag.exact_only {
            let staged = collect_staged_oovs_for_dag(dag, user_mem);
            if let Some(word) = staged.into_iter().next() {
                if outcome.candidates[0] != word {
                    outcome.candidates.retain(|c| c != &word);
                    outcome.candidates.insert(1, word);
                    if outcome.candidates.len() > limit {
                        outcome.candidates.truncate(limit);
                    }
                }
                return true;
            }
        }
    }
    false
}

fn collect_single_char_entries_for_edge(
    model: &Model,
    ctx: &ContextState,
    kind: DagEdgeKind,
    cfg: &DecoderConfig,
    out: &mut Vec<ScoredCandidate>,
) {
    let mut score_and_push = |entries: &[LexEntry], penalty: f32| {
        for entry in entries {
            let word = model.word_str(entry.word_id);
            if word.is_empty() {
                continue;
            }
            let uni = model
                .unigram_logp
                .get(entry.word_id as usize)
                .copied()
                .unwrap_or(-20.0);
            let lm_score = if ctx.has_context {
                0.65 * model.trigram_logp(ctx.h1, ctx.h2, entry.word_id) + 0.35 * uni
            } else {
                uni
            };
            let acoustic_score = cfg.reading_weight * entry.reading_logp - penalty;
            out.push(ScoredCandidate {
                text: word.to_owned(),
                lm_score,
                acoustic_score,
                user_score: 0.0,
                has_oov: false,
            });
        }
    };

    match kind {
        DagEdgeKind::Exact(syl) => {
            if let Some(key) = pack_exact(&[syl]) {
                score_and_push(model.exact.lookup(key), 0.0);
            }
        }
        DagEdgeKind::Abbrev(letter) => {
            score_and_push(model.initial_entries(letter), cfg.abbrev_penalty);
        }
        DagEdgeKind::Abbrev2(di) => {
            let mut best_by_word: std::collections::HashMap<u32, f32> =
                std::collections::HashMap::new();
            for (idx, _) in SYLLABLES.iter().enumerate() {
                let sid = (idx + 1) as u16;
                if !syllable_matches_digraph(sid, di) {
                    continue;
                }
                if let Some(key) = pack_exact(&[sid]) {
                    for entry in model.exact.lookup(key) {
                        best_by_word
                            .entry(entry.word_id)
                            .and_modify(|v| *v = v.max(entry.reading_logp))
                            .or_insert(entry.reading_logp);
                    }
                }
            }
            let deduped: Vec<LexEntry> = best_by_word
                .into_iter()
                .map(|(word_id, reading_logp)| LexEntry {
                    word_id,
                    reading_logp,
                })
                .collect();
            score_and_push(&deduped, cfg.abbrev_penalty);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn decode_full_single_syllable_with_lex_words(
    model: &Model,
    reranker: Option<&Reranker>,
    _dag: &PinyinDag,
    ctx: &ContextState,
    context_text: &str,
    eff_eos: f32,
    full_syl: u16,
    end_spans: &[SpanEdge],
    oov_pool: &[String],
    active_user: Option<&UserMemory>,
    limit: usize,
    cfg: &DecoderConfig,
) -> DecodeOutcome {
    let entries = pack_exact(&[full_syl])
        .map(|k| model.exact.lookup(k))
        .unwrap_or(&[]);
    let mut one = [0; 6];
    one[0] = full_syl;
    let pron = SpanPron::Exact { len: 1, syls: one };
    let mut scored: Vec<ScoredCandidate> = Vec::with_capacity(entries.len() + 8);
    let mut joined_buf = String::with_capacity(24);

    for entry in entries {
        let word = model.word_str(entry.word_id);
        if word.is_empty() {
            continue;
        }
        let mut best_lm = model.trigram_logp(ctx.h1, ctx.h2, entry.word_id)
            + eff_eos * model.trigram_logp(ctx.h2, entry.word_id, EOS_ID);
        let mut best_ac = cfg.reading_weight * entry.reading_logp;
        let mut best_total = best_lm + best_ac;

        for tail in &ctx.tail_joins {
            if tail.tail_chars + 1 > 6 {
                continue;
            }
            joined_buf.clear();
            joined_buf.push_str(&tail.tail_text);
            joined_buf.push_str(word);
            if let Some(joined_id) = model.word_id(&joined_buf) {
                if let Some(joined_rp) =
                    model.match_joined_reading(joined_id, tail.tail_chars, &pron)
                {
                    let joined_lm = (model.trigram_logp(tail.pre_h1, tail.pre_h2, joined_id)
                        - tail.tail_lm_score)
                        + eff_eos * model.trigram_logp(tail.pre_h2, joined_id, EOS_ID);
                    let joined_ac = cfg.reading_weight * joined_rp;
                    let joined_total = joined_lm + joined_ac;
                    if joined_total > best_total {
                        best_total = joined_total;
                        best_lm = joined_lm;
                        best_ac = joined_ac;
                    }
                }
            }
        }
        scored.push(ScoredCandidate {
            text: word.to_owned(),
            lm_score: best_lm,
            acoustic_score: best_ac,
            user_score: 0.0,
            has_oov: false,
        });
    }

    // Keep genuine multi-char lexicon / user OOV words covering 0..dag.len (e.g. `xian` -> `西安`),
    // while excluding synthetic multi-char beam paths like `急啊` / `除昂` / `西昂`.
    for sp in end_spans {
        if sp.start != 0 || sp.abbrev_count > 0 {
            continue;
        }
        match sp.word {
            SpanWord::Model(wid) => {
                let word = model.word_str(wid);
                if word.chars().count() < 2 {
                    continue;
                }
                let lm = model.trigram_logp(ctx.h1, ctx.h2, wid)
                    + eff_eos * model.trigram_logp(ctx.h2, wid, EOS_ID);
                let ac = cfg.reading_weight * sp.reading_logp + sp.domain_boost - cfg.word_penalty;
                let bg_boost = active_user.map_or(0.0, |u| u.active_bigram_boost(ctx.h2, word));
                scored.push(ScoredCandidate {
                    text: word.to_owned(),
                    lm_score: lm,
                    acoustic_score: ac,
                    user_score: sp.user_boost + bg_boost,
                    has_oov: false,
                });
            }
            SpanWord::UserOov(idx) => {
                let word = &oov_pool[idx];
                let lm = model.oov_transition_logp(ctx.h2);
                let ac = -cfg.word_penalty;
                let bg_boost = active_user.map_or(0.0, |u| u.active_bigram_boost(ctx.h2, word));
                scored.push(ScoredCandidate {
                    text: word.clone(),
                    lm_score: lm,
                    acoustic_score: ac,
                    user_score: sp.user_boost + bg_boost,
                    has_oov: true,
                });
            }
        }
    }

    finalize_candidates(reranker, context_text, scored, limit, cfg)
}

fn extract_best_syllable_bounds(
    model: &Model,
    dag: &PinyinDag,
    beam_res: &BeamSearchResult,
) -> Vec<(usize, usize)> {
    let best_hyp = beam_res.beams[dag.len]
        .iter()
        .max_by(|a, b| a.total_score().total_cmp(&b.total_score()))
        .or_else(|| {
            (1..=dag.len)
                .rev()
                .find_map(|pos| {
                    beam_res.beams[pos]
                        .iter()
                        .max_by(|a, b| a.total_score().total_cmp(&b.total_score()))
                })
        });

    let mut span_chain: Vec<&BackNode> = Vec::new();
    if let Some(hyp) = best_hyp {
        let mut cur = hyp.node_idx;
        while cur != u32::MAX {
            let node = &beam_res.nodes[cur as usize];
            span_chain.push(node);
            cur = node.parent;
        }
        span_chain.reverse();
    }

    let mut syl_bounds = Vec::new();
    let mut covered_until = 0usize;
    for node in span_chain {
        let s = node.start as usize;
        let e = node.end as usize;
        split_span_into_syllables(model, dag, s, e, &node.pron, &mut syl_bounds);
        covered_until = e;
    }

    // If beam failed to reach the end (partial path), greedily fill the tail on the DAG.
    while covered_until < dag.len {
        let next_end = dag.edges_from[covered_until]
            .iter()
            .filter(|e| matches!(e.kind, DagEdgeKind::Exact(_)) || !dag.exact_only)
            .map(|e| e.end)
            .max()
            .unwrap_or((covered_until + 1).min(dag.len));
        syl_bounds.push((covered_until, next_end));
        covered_until = next_end;
    }
    syl_bounds
}

fn split_span_into_syllables(
    model: &Model,
    dag: &PinyinDag,
    span_start: usize,
    span_end: usize,
    pron: &SpanPron,
    out: &mut Vec<(usize, usize)>,
) {
    let expected_units: Vec<DagEdgeKind> = match *pron {
        SpanPron::Exact { len, syls } => (0..len as usize)
            .map(|i| DagEdgeKind::Exact(syls[i]))
            .collect(),
        SpanPron::AbbrevTail {
            prefix_len,
            prefix,
            last_letter,
        } => {
            let mut v: Vec<DagEdgeKind> = (0..prefix_len as usize)
                .map(|i| DagEdgeKind::Exact(prefix[i]))
                .collect();
            v.push(DagEdgeKind::Abbrev(last_letter));
            v
        }
        SpanPron::AbbrevInit { len, letters } => (0..len as usize)
            .map(|i| DagEdgeKind::Abbrev(letters[i]))
            .collect(),
        SpanPron::Mixed { len, units } => units[..len as usize].to_vec(),
    };

    if expected_units.len() <= 1 {
        out.push((span_start, span_end));
        return;
    }

    fn dfs_match(
        dag: &PinyinDag,
        pos: usize,
        span_end: usize,
        units: &[DagEdgeKind],
        idx: usize,
        acc: &mut Vec<(usize, usize)>,
    ) -> bool {
        if idx == units.len() {
            return pos == span_end;
        }
        if pos >= span_end {
            return false;
        }
        let target = units[idx];
        for edge in &dag.edges_from[pos] {
            if edge.end <= span_end && edge.kind == target {
                acc.push((pos, edge.end));
                if dfs_match(dag, edge.end, span_end, units, idx + 1, acc) {
                    return true;
                }
                acc.pop();
            }
        }
        false
    }

    let _ = model;
    let mut matched = Vec::with_capacity(expected_units.len());
    if dfs_match(dag, span_start, span_end, &expected_units, 0, &mut matched) {
        out.extend(matched);
        return;
    }

    // Fallback: divide span greedily on exact/abbrev DAG edges.
    let mut pos = span_start;
    while pos < span_end {
        let next = dag.edges_from[pos]
            .iter()
            .filter(|e| e.end <= span_end && (matches!(e.kind, DagEdgeKind::Exact(_)) || !dag.exact_only))
            .map(|e| e.end)
            .max()
            .unwrap_or((pos + 1).min(span_end));
        out.push((pos, next));
        pos = next;
    }
}

fn format_segmented_preedit(keys: &str, dag: &PinyinDag, seg_ends: &[usize]) -> String {
    if dag.len == 0 || seg_ends.is_empty() {
        return keys.to_owned();
    }
    let mut out = String::with_capacity(keys.len() + seg_ends.len());
    let mut prev_norm = 0usize;
    for (idx, &end_norm) in seg_ends.iter().enumerate() {
        let end_norm = end_norm.min(dag.len);
        if end_norm <= prev_norm {
            continue;
        }
        if idx > 0 && !out.is_empty() {
            out.push('\'');
        }
        for pos in prev_norm..end_norm {
            let b_idx = dag.norm_byte_starts[pos];
            if let Some(&ch) = keys.as_bytes().get(b_idx) {
                out.push((ch as char).to_ascii_lowercase());
            }
        }
        prev_norm = end_norm;
    }
    if prev_norm < dag.len {
        if !out.is_empty() {
            out.push('\'');
        }
        for pos in prev_norm..dag.len {
            let b_idx = dag.norm_byte_starts[pos];
            if let Some(&ch) = keys.as_bytes().get(b_idx) {
                out.push((ch as char).to_ascii_lowercase());
            }
        }
    }
    out
}

fn collect_staged_oovs_for_dag(dag: &PinyinDag, user: &UserMemory) -> Vec<String> {
    fn dfs(
        dag: &PinyinDag,
        user: &UserMemory,
        pos: usize,
        len: usize,
        syls: &mut [u16; 6],
        out: &mut Vec<String>,
    ) {
        if pos == dag.len {
            if len >= 2 {
                if let Some(key) = pack_exact(&syls[..len]) {
                    for w in user.staged_oov_for_key(key) {
                        if !out.contains(&w) {
                            out.push(w);
                        }
                    }
                }
            }
            return;
        }
        if len >= 6 {
            return;
        }
        for edge in &dag.edges_from[pos] {
            if let DagEdgeKind::Exact(syl) = edge.kind {
                syls[len] = syl;
                dfs(dag, user, edge.end, len + 1, syls, out);
            }
        }
    }

    let mut out = Vec::new();
    let mut syls = [0u16; 6];
    dfs(dag, user, 0, 0, &mut syls, &mut out);
    out
}

#[allow(clippy::too_many_arguments)]
fn try_decode_single_unit(
    model: &Model,
    reranker: Option<&Reranker>,
    dag: &PinyinDag,
    ctx: &ContextState,
    context_text: &str,
    eff_eos: f32,
    limit: usize,
    cfg: &DecoderConfig,
) -> Option<DecodeOutcome> {
    let edges = &dag.edges_from[0];
    if edges.len() != 1 || edges[0].end != dag.len {
        return None;
    }
    let has_other = (1..dag.len).any(|pos| !dag.edges_from[pos].is_empty());
    if has_other {
        return None;
    }

    let (entries, pron): (&[LexEntry], SpanPron) = match edges[0].kind {
        DagEdgeKind::Exact(syl) => {
            let mut syls = [0; 6];
            syls[0] = syl;
            (
                pack_exact(&[syl])
                    .map(|k| model.exact.lookup(k))
                    .unwrap_or(&[]),
                SpanPron::Exact { len: 1, syls },
            )
        }
        DagEdgeKind::Abbrev(letter) => {
            let mut letters = [0; 6];
            letters[0] = letter;
            (
                model.initial_entries(letter),
                SpanPron::AbbrevInit { len: 1, letters },
            )
        }
        DagEdgeKind::Abbrev2(_) => return None,
    };
    if entries.is_empty() {
        return Some(DecodeOutcome::default());
    }

    // Without preceding Chinese context, rank single-letter initial shorthands (`d` -> `的`,
    // `l` -> `了`, `j` -> `就`, `g` -> `个`, etc.) by unigram prior + reading probability
    // instead of `<bos> X <eos>` standalone-sentence probability (which favors `对`/`来`/`据`).
    let nocontext_single_abbrev =
        !ctx.has_context && matches!(edges[0].kind, DagEdgeKind::Abbrev(_));

    let mut joined_buf = String::with_capacity(24);
    let mut scored: Vec<ScoredCandidate> = Vec::with_capacity(entries.len());
    for entry in entries {
        let word = model.word_str(entry.word_id);
        if word.is_empty() {
            continue;
        }
        let mut best_lm = if nocontext_single_abbrev {
            model
                .unigram_logp
                .get(entry.word_id as usize)
                .copied()
                .unwrap_or(-20.0)
        } else {
            model.trigram_logp(ctx.h1, ctx.h2, entry.word_id)
                + eff_eos * model.trigram_logp(ctx.h2, entry.word_id, EOS_ID)
        };
        let mut best_ac = cfg.reading_weight * entry.reading_logp;
        let mut best_total = best_lm + best_ac;

        for tail in &ctx.tail_joins {
            if tail.tail_chars + 1 > 6 {
                continue;
            }
            joined_buf.clear();
            joined_buf.push_str(&tail.tail_text);
            joined_buf.push_str(word);
            if let Some(joined_id) = model.word_id(&joined_buf) {
                if let Some(joined_rp) = model.match_joined_reading(joined_id, tail.tail_chars, &pron)
                {
                    let joined_lm = (model.trigram_logp(tail.pre_h1, tail.pre_h2, joined_id)
                        - tail.tail_lm_score)
                        + eff_eos * model.trigram_logp(tail.pre_h2, joined_id, EOS_ID);
                    let joined_ac = cfg.reading_weight * joined_rp;
                    let joined_total = joined_lm + joined_ac;
                    if joined_total > best_total {
                        best_total = joined_total;
                        best_lm = joined_lm;
                        best_ac = joined_ac;
                    }
                }
            }
        }
        scored.push(ScoredCandidate {
            text: word.to_owned(),
            lm_score: best_lm,
            acoustic_score: best_ac,
            user_score: 0.0,
            has_oov: false,
        });
    }
    let rr = if nocontext_single_abbrev { None } else { reranker };
    Some(finalize_candidates(rr, context_text, scored, limit, cfg))
}

fn finalize_candidates(
    reranker: Option<&Reranker>,
    context_text: &str,
    mut raw: Vec<ScoredCandidate>,
    limit: usize,
    cfg: &DecoderConfig,
) -> DecodeOutcome {
    raw.sort_by(|a, b| {
        (b.lm_score + b.acoustic_score + b.user_score)
            .total_cmp(&(a.lm_score + a.acoustic_score + a.user_score))
    });
    let has_any_user = raw.iter().any(|c| c.user_score > 0.0);
    let want = if reranker.is_some() {
        limit.max(cfg.rerank_top_k)
    } else if has_any_user {
        limit.max(2)
    } else {
        limit
    };
    let mut seen = HashSet::with_capacity(want * 2);
    let mut unique: Vec<ScoredCandidate> = Vec::with_capacity(want);
    for cand in raw {
        if seen.insert(cand.text.clone()) {
            unique.push(cand);
            if unique.len() >= want {
                break;
            }
        }
    }

    if let Some(rr) = reranker {
        if unique.len() > 1 && cfg.rerank_top_k > 1 && cfg.neural_weight > 0.0 {
            let k = cfg.rerank_top_k.min(unique.len());
            let texts: Vec<&str> = unique[..k].iter().map(|c| c.text.as_str()).collect();
            let neural_scores = rr.score_candidates(context_text, &texts, cfg.neural_eos_weight);
            let mut best_base: Option<(f32, usize)> = None;
            let mut rescored: Vec<(f32, usize)> = Vec::with_capacity(k);
            for i in 0..k {
                let base_s = (1.0 - cfg.neural_weight) * unique[i].lm_score
                    + cfg.neural_weight * neural_scores[i]
                    + unique[i].acoustic_score;
                if !unique[i].has_oov {
                    let better = match best_base {
                        None => true,
                        Some((bs, _)) => base_s > bs,
                    };
                    if better {
                        best_base = Some((base_s, i));
                    }
                }
                rescored.push((base_s + unique[i].user_score, i));
            }
            let base_top1 = best_base
                .map(|(_, idx)| unique[idx].text.clone())
                .or_else(|| unique[k..].iter().find(|c| !c.has_oov).map(|c| c.text.clone()))
                .or_else(|| unique.first().map(|c| c.text.clone()));

            rescored.sort_by(|a, b| b.0.total_cmp(&a.0));
            let mut out = Vec::with_capacity(limit.min(unique.len()));
            for (_, idx) in rescored {
                out.push(unique[idx].text.clone());
                if out.len() >= limit {
                    return DecodeOutcome {
                        candidates: out,
                        base_top1,
                    };
                }
            }
            for cand in &unique[k..] {
                out.push(cand.text.clone());
                if out.len() >= limit {
                    break;
                }
            }
            return DecodeOutcome {
                candidates: out,
                base_top1,
            };
        }
    }

    let base_top1 = unique
        .iter()
        .filter(|c| !c.has_oov)
        .max_by(|a, b| (a.lm_score + a.acoustic_score).total_cmp(&(b.lm_score + b.acoustic_score)))
        .or_else(|| unique.first())
        .map(|c| c.text.clone());
    let candidates = unique.into_iter().take(limit).map(|c| c.text).collect();
    DecodeOutcome {
        candidates,
        base_top1,
    }
}

fn collect_spans(
    model: &Model,
    dag: &PinyinDag,
    user: Option<&UserMemory>,
    cfg: &DecoderConfig,
) -> (Vec<Vec<SpanEdge>>, Vec<String>) {
    const MAX_EXACT_LEN: usize = 6;
    const MAX_ABBREV_INIT_LEN: usize = 6;
    const MAX_MIXED_LEN: usize = 4;

    let mut spans_to: Vec<Vec<SpanEdge>> = vec![Vec::new(); dag.len + 1];
    let mut oov_pool: Vec<String> = Vec::new();
    let mut mixed_buf: Vec<LexEntry> = Vec::with_capacity(cfg.max_span_word);

    #[allow(clippy::too_many_arguments)]
    fn dfs(
        model: &Model,
        dag: &PinyinDag,
        user: Option<&UserMemory>,
        cfg: &DecoderConfig,
        start: usize,
        pos: usize,
        len: usize,
        units: [DagEdgeKind; 6],
        abbrev_count: u8,
        has_abbrev2: bool,
        mixed_buf: &mut Vec<LexEntry>,
        spans_to: &mut Vec<Vec<SpanEdge>>,
        oov_pool: &mut Vec<String>,
    ) {
        if pos >= dag.len {
            return;
        }
        for edge in &dag.edges_from[pos] {
            let is_single_abbrev = matches!(edge.kind, DagEdgeKind::Abbrev(_));
            if len >= MAX_MIXED_LEN
                && abbrev_count > 0
                && !(abbrev_count == len as u8 && !has_abbrev2 && is_single_abbrev)
            {
                continue;
            }
            let next_len = len + 1;
            let mut next_units = units;
            next_units[len] = edge.kind;
            let next_abbrev = abbrev_count + u8::from(!matches!(edge.kind, DagEdgeKind::Exact(_)));
            let next_has_abbrev2 = has_abbrev2 || matches!(edge.kind, DagEdgeKind::Abbrev2(_));

            if next_len == 1 {
                match edge.kind {
                    DagEdgeKind::Exact(syl) => {
                        if let Some(key) = pack_exact(&[syl]) {
                            let entries = model.exact.lookup(key);
                            let mut one = [0; 6];
                            one[0] = syl;
                            push_spans(
                                model,
                                entries,
                                &[],
                                cfg.max_span_exact_char,
                                start,
                                edge.end,
                                0,
                                SpanPron::Exact { len: 1, syls: one },
                                cfg.cs_ai_boost,
                                &mut spans_to[edge.end],
                            );
                        }
                    }
                    DagEdgeKind::Abbrev(letter) => {
                        let entries = model.initial_entries(letter);
                        let mut one = [0; 6];
                        one[0] = letter;
                        push_spans(
                            model,
                            entries,
                            &[],
                            cfg.max_span_abbrev_char,
                            start,
                            edge.end,
                            1,
                            SpanPron::AbbrevInit {
                                len: 1,
                                letters: one,
                            },
                            cfg.cs_ai_boost,
                            &mut spans_to[edge.end],
                        );
                    }
                    // Digraph initials (zh/ch/sh) only participate in multi-char word spans.
                    DagEdgeKind::Abbrev2(_) => {}
                }
                dfs(
                    model,
                    dag,
                    user,
                    cfg,
                    start,
                    edge.end,
                    next_len,
                    next_units,
                    next_abbrev,
                    next_has_abbrev2,
                    mixed_buf,
                    spans_to,
                    oov_pool,
                );
                continue;
            }

            if next_abbrev == 0 {
                let mut syls = [0; 6];
                for (i, u) in next_units[..next_len].iter().enumerate() {
                    if let DagEdgeKind::Exact(s) = *u {
                        syls[i] = s;
                    }
                }
                if let Some(key) = pack_exact(&syls[..next_len]) {
                    let entries = model.exact.lookup(key);
                    let user_matches = user
                        .map(|u| u.active_entries_for_key(key))
                        .unwrap_or_default();
                    let pron = SpanPron::Exact {
                        len: next_len as u8,
                        syls,
                    };
                    push_spans(
                        model,
                        entries,
                        &user_matches,
                        cfg.max_span_word,
                        start,
                        edge.end,
                        0,
                        pron,
                        cfg.cs_ai_boost,
                        &mut spans_to[edge.end],
                    );
                    for m in &user_matches {
                        if m.is_oov && m.boost > 0.0 {
                            let idx = oov_pool.len();
                            oov_pool.push(m.word.clone());
                            spans_to[edge.end].push(SpanEdge {
                                start,
                                word: SpanWord::UserOov(idx),
                                reading_logp: 0.0,
                                abbrev_count: 0,
                                pron,
                                user_boost: m.boost,
                                domain_boost: 0.0,
                            });
                        }
                    }
                }
                if next_len < MAX_EXACT_LEN {
                    dfs(
                        model,
                        dag,
                        user,
                        cfg,
                        start,
                        edge.end,
                        next_len,
                        next_units,
                        next_abbrev,
                        next_has_abbrev2,
                        mixed_buf,
                        spans_to,
                        oov_pool,
                    );
                }
            } else if next_abbrev == next_len as u8 && !next_has_abbrev2 {
                let mut letters = [0; 6];
                for (i, u) in next_units[..next_len].iter().enumerate() {
                    if let DagEdgeKind::Abbrev(lt) = *u {
                        letters[i] = lt;
                    }
                }
                if let Some(key) = pack_initials(&letters[..next_len]) {
                    let entries = model.abbrev_init.lookup(key);
                    push_spans(
                        model,
                        entries,
                        &[],
                        cfg.max_span_word,
                        start,
                        edge.end,
                        next_abbrev,
                        SpanPron::AbbrevInit {
                            len: next_len as u8,
                            letters,
                        },
                        cfg.cs_ai_boost,
                        &mut spans_to[edge.end],
                    );
                }
                if next_len < MAX_ABBREV_INIT_LEN {
                    dfs(
                        model,
                        dag,
                        user,
                        cfg,
                        start,
                        edge.end,
                        next_len,
                        next_units,
                        next_abbrev,
                        next_has_abbrev2,
                        mixed_buf,
                        spans_to,
                        oov_pool,
                    );
                }
            } else if next_abbrev == 1 && is_single_abbrev {
                if let DagEdgeKind::Abbrev(letter) = edge.kind {
                    let mut prefix = [0; 5];
                    for (i, u) in next_units[..len].iter().enumerate() {
                        if let DagEdgeKind::Exact(s) = *u {
                            prefix[i] = s;
                        }
                    }
                    if let Some(key) = pack_abbrev_tail(&prefix[..len], letter) {
                        let entries = model.abbrev_tail.lookup(key);
                        push_spans(
                            model,
                            entries,
                            &[],
                            cfg.max_span_word,
                            start,
                            edge.end,
                            1,
                            SpanPron::AbbrevTail {
                                prefix_len: len as u8,
                                prefix,
                                last_letter: letter,
                            },
                            cfg.cs_ai_boost,
                            &mut spans_to[edge.end],
                        );
                    }
                }
                if next_len < MAX_MIXED_LEN {
                    dfs(
                        model,
                        dag,
                        user,
                        cfg,
                        start,
                        edge.end,
                        next_len,
                        next_units,
                        next_abbrev,
                        next_has_abbrev2,
                        mixed_buf,
                        spans_to,
                        oov_pool,
                    );
                }
            } else if next_len <= MAX_MIXED_LEN {
                model.lookup_mixed(&next_units[..next_len], cfg.max_span_word, mixed_buf);
                push_spans(
                    model,
                    mixed_buf,
                    &[],
                    cfg.max_span_word,
                    start,
                    edge.end,
                    next_abbrev,
                    SpanPron::Mixed {
                        len: next_len as u8,
                        units: next_units,
                    },
                    cfg.cs_ai_boost,
                    &mut spans_to[edge.end],
                );
                if next_len < MAX_MIXED_LEN {
                    dfs(
                        model,
                        dag,
                        user,
                        cfg,
                        start,
                        edge.end,
                        next_len,
                        next_units,
                        next_abbrev,
                        next_has_abbrev2,
                        mixed_buf,
                        spans_to,
                        oov_pool,
                    );
                }
            }
        }
    }

    let init_units = [DagEdgeKind::Abbrev(0); 6];
    for start in 0..dag.len {
        dfs(
            model,
            dag,
            user,
            cfg,
            start,
            start,
            0,
            init_units,
            0,
            false,
            &mut mixed_buf,
            &mut spans_to,
            &mut oov_pool,
        );
    }
    (spans_to, oov_pool)
}

#[allow(clippy::too_many_arguments)]
fn push_spans(
    model: &Model,
    entries: &[LexEntry],
    user_matches: &[ActiveKeyMatch],
    max_keep: usize,
    start: usize,
    end: usize,
    abbrev_count: u8,
    pron: SpanPron,
    cs_ai_boost_enabled: bool,
    out: &mut Vec<SpanEdge>,
) {
    if entries.is_empty() {
        return;
    }
    let boost_for = |word_id: u32| -> f32 {
        for m in user_matches {
            if m.model_word_id == Some(word_id) {
                return m.boost;
            }
        }
        0.0
    };
    let domain_for = |word_id: u32| -> f32 {
        if cs_ai_boost_enabled && abbrev_count == 0 {
            model.cs_ai_boost(word_id)
        } else {
            0.0
        }
    };
    let head = max_keep.min(entries.len());
    for e in &entries[..head] {
        out.push(SpanEdge {
            start,
            word: SpanWord::Model(e.word_id),
            reading_logp: e.reading_logp,
            abbrev_count,
            pron,
            user_boost: boost_for(e.word_id),
            domain_boost: domain_for(e.word_id),
        });
    }
    let check_fn = end - start <= 6;
    if check_fn || !user_matches.is_empty() || (cs_ai_boost_enabled && abbrev_count == 0) {
        for e in &entries[head..] {
            let user_boost = boost_for(e.word_id);
            let domain_boost = domain_for(e.word_id);
            let is_prot_fn = check_fn && model.is_protected_function_word(e.word_id);
            if user_boost > 0.0 || is_prot_fn || domain_boost > 0.0 {
                out.push(SpanEdge {
                    start,
                    word: SpanWord::Model(e.word_id),
                    reading_logp: e.reading_logp,
                    abbrev_count,
                    pron,
                    user_boost,
                    domain_boost,
                });
            }
        }
    }
}

fn prune_beam(
    model: &Model,
    oov_pool: &[String],
    nodes: &mut Vec<BackNode>,
    raw: &mut [RawHyp],
    scratch: &mut BeamScratch,
    beam_width: usize,
    max_per_state: usize,
) -> Vec<Hypothesis> {
    if max_per_state == 0 || beam_width == 0 || raw.is_empty() {
        return Vec::new();
    }

    // Group by (h1, h2) keeping up to `max_per_state` distinct texts per LM state.
    raw.sort_by(|a, b| {
        (a.h1, a.h2)
            .cmp(&(b.h1, b.h2))
            .then_with(|| b.total_score().total_cmp(&a.total_score()))
    });

    scratch.kept_raw.clear();
    let mut cur_state: Option<(u32, u32)> = None;
    let mut state_count = 0usize;
    let mut group_first: Option<RawHyp> = None;
    let mut materialized_slots = 0usize;

    for &rh in raw.iter() {
        let state = (rh.h1, rh.h2);
        if cur_state != Some(state) {
            cur_state = Some(state);
            state_count = 1;
            group_first = Some(rh);
            materialized_slots = 0;
            scratch.kept_raw.push(rh);
            continue;
        }
        if state_count >= max_per_state {
            continue;
        }
        if materialized_slots == 0 {
            if scratch.text_slots.is_empty() {
                scratch.text_slots.push(String::with_capacity(32));
            }
            let first = group_first.unwrap();
            materialize_chain(
                nodes,
                first.parent_node,
                Some(first.word),
                model,
                oov_pool,
                &mut scratch.rev_buf,
                &mut scratch.text_slots[0],
            );
            materialized_slots = 1;
        }
        materialize_chain(
            nodes,
            rh.parent_node,
            Some(rh.word),
            model,
            oov_pool,
            &mut scratch.rev_buf,
            &mut scratch.cmp_buf,
        );
        if scratch.text_slots[..materialized_slots]
            .iter()
            .any(|t| t == &scratch.cmp_buf)
        {
            continue;
        }
        state_count += 1;
        scratch.kept_raw.push(rh);
        if state_count < max_per_state {
            if scratch.text_slots.len() <= materialized_slots {
                scratch.text_slots.push(String::with_capacity(32));
            }
            std::mem::swap(&mut scratch.text_slots[materialized_slots], &mut scratch.cmp_buf);
            materialized_slots += 1;
        }
    }

    scratch
        .kept_raw
        .sort_by(|a, b| b.total_score().total_cmp(&a.total_score()));
    if scratch.kept_raw.len() > beam_width {
        scratch.kept_raw.truncate(beam_width);
    }

    let mut kept = Vec::with_capacity(scratch.kept_raw.len());
    for &rh in &scratch.kept_raw {
        let node_idx = nodes.len() as u32;
        nodes.push(BackNode {
            parent: rh.parent_node,
            word: rh.word,
            start: rh.span_start,
            end: rh.span_end,
            pron: rh.pron,
        });
        let (tri_start, tri_end, bg_bow) = model.trigram_meta(rh.h1, rh.h2);
        kept.push(Hypothesis {
            lm_score: rh.lm_score,
            acoustic_score: rh.acoustic_score,
            user_score: rh.user_score,
            has_oov: rh.has_oov,
            h2: rh.h2,
            tri_start,
            tri_end,
            bg_bow,
            node_idx,
        });
    }
    kept.sort_unstable_by_key(|h| h.h2);
    kept
}

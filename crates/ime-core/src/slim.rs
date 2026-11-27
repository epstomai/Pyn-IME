//! Mobile model slimming: relative-entropy (Stolcke) n-gram pruning and 8-bit codebook quantization.
//!
//! The pruned model stays a proper back-off model: every history's back-off weight is re-derived
//! so that `sum_w P(w | h)` keeps the same normalisation the trainer produced. Quantization is a
//! storage-only transform (`IMEMOD02`); the in-memory model is plain `f32`, so the decoder hot path
//! is untouched.

use crate::model::{Model, BOS_ID, EOS_ID};

/// Number of codebook entries for 8-bit quantized tables.
pub const Q8_LEVELS: usize = 256;

#[derive(Debug, Clone, Default)]
pub struct PruneReport {
    pub bigrams_before: usize,
    pub bigrams_after: usize,
    pub trigrams_before: usize,
    pub trigrams_after: usize,
    /// Bigrams kept only because they still carry trigram children.
    pub forced_bigrams: usize,
    /// N-grams predicting `</s>` or a protected function word (candidates / kept after pruning).
    pub protected_bigrams: usize,
    pub protected_bigrams_kept: usize,
    pub protected_trigrams: usize,
    pub protected_trigrams_kept: usize,
    /// Mean / max |stored bow - normalised bow| in the source model (diagnostic for trainer consistency).
    pub unigram_bow_mean_err: f64,
    pub unigram_bow_max_err: f64,
    pub bigram_bow_mean_err: f64,
    pub bigram_bow_max_err: f64,
}

const MIN_MASS: f64 = 1e-12;

#[inline]
fn bow_from_mass(num: f64, den: f64) -> f64 {
    (num.max(MIN_MASS) / den.max(MIN_MASS)).ln()
}

/// Relative-entropy increase of removing one explicit n-gram `p = P(w|h)` whose back-off estimate
/// is `q = P_lower(w|h')`, given history mass `ph = P(h)` and the history's remaining back-off
/// mass `num = 1 - sum explicit P`, `den = 1 - sum explicit P_lower` (Stolcke 1998, as in SRILM).
#[inline]
fn delta_entropy(ph: f64, p: f64, q: f64, num: f64, den: f64) -> f64 {
    let bow = bow_from_mass(num, den);
    let bow_new = bow_from_mass(num + p, den + q);
    let p = p.max(MIN_MASS);
    let q = q.max(MIN_MASS);
    ph * (p * (p.ln() - (q.ln() + bow_new)) + num.max(0.0) * (bow - bow_new))
}

/// Indices of the `keep` highest-scoring candidates (all of them if `keep >= len`).
fn select_top(scores: &[(f32, u32)], keep: usize) -> Vec<u32> {
    if keep >= scores.len() {
        return scores.iter().map(|s| s.1).collect();
    }
    if keep == 0 {
        return Vec::new();
    }
    let mut v = scores.to_vec();
    v.select_nth_unstable_by(keep - 1, |a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    v.truncate(keep);
    v.into_iter().map(|s| s.1).collect()
}

fn history_unigram_prob(m: &Model, w: u32) -> f64 {
    // `<s>` never appears as a predicted token, so its stored unigram is not a usable marginal.
    // Sentence starts are exactly as frequent as sentence ends.
    let id = if w == BOS_ID { EOS_ID } else { w };
    let lp = m.unigram_logp.get(id as usize).copied().unwrap_or(-20.0);
    if lp.is_finite() {
        (lp as f64).exp()
    } else {
        0.0
    }
}

/// Prune `m` down to at most `max_bigrams` bigrams and `max_trigrams` trigrams, removing the
/// n-grams whose removal costs the least relative entropy, then re-normalise back-off weights.
///
/// Bigrams that still own trigram children are always kept (the trigram table is keyed by
/// bigram index), so the bigram budget may be exceeded only if the trigram budget forces it.
///
/// N-grams that predict `</s>` or a protected function word (sentence-final particles, 的/地/得…)
/// have their pruning cost multiplied by `protect_weight`: their history mass `P(h)` is often
/// small, yet they decide exactly the particle/structural-word choices the gates guard.
/// `f64::INFINITY` keeps all of them.
pub fn prune_model(
    m: &Model,
    max_bigrams: usize,
    max_trigrams: usize,
    protect_weight: f64,
) -> (Model, PruneReport) {
    let is_protected = |w: u32| w == EOS_ID || m.is_protected_function_word(w);
    let weigh = |score: f64, protected: bool| -> f32 {
        if protected {
            if protect_weight.is_infinite() {
                f32::INFINITY
            } else {
                (score * protect_weight) as f32
            }
        } else {
            score as f32
        }
    };
    let num_words = m.unigram_logp.len();
    let nb = m.bigram_w2.len();
    let nt = m.trigram_w3.len();
    let mut report = PruneReport {
        bigrams_before: nb,
        trigrams_before: nt,
        ..PruneReport::default()
    };

    let mut bg_w1 = vec![0u32; nb];
    for w1 in 0..num_words {
        let (s, e) = (m.bigram_offsets[w1] as usize, m.bigram_offsets[w1 + 1] as usize);
        for slot in &mut bg_w1[s..e] {
            *slot = w1 as u32;
        }
    }

    // ---- Stage 1: score trigrams against the original model. ----
    let mut tri_scores: Vec<(f32, u32)> = Vec::with_capacity(nt);
    let mut old_tri_mass = vec![(1.0f64, 1.0f64); nb];
    let (mut err_sum, mut err_max, mut err_n) = (0.0f64, 0.0f64, 0usize);
    let mut qbuf: Vec<f64> = Vec::new();
    for bg in 0..nb {
        let (s, e) = (m.trigram_offsets[bg] as usize, m.trigram_offsets[bg + 1] as usize);
        if s == e {
            continue;
        }
        let w2 = m.bigram_w2[bg];
        let ph = history_unigram_prob(m, bg_w1[bg]) * (m.bigram_logp[bg] as f64).exp();
        qbuf.clear();
        let (mut sp, mut sq) = (0.0f64, 0.0f64);
        for t in s..e {
            let q = (m.bigram_logp(w2, m.trigram_w3[t]) as f64).exp();
            qbuf.push(q);
            sp += (m.trigram_logp[t] as f64).exp();
            sq += q;
        }
        let (num, den) = (1.0 - sp, 1.0 - sq);
        old_tri_mass[bg] = (num, den);
        let err = (bow_from_mass(num, den) - m.bigram_bow[bg] as f64).abs();
        err_sum += err;
        err_max = err_max.max(err);
        err_n += 1;
        for (i, t) in (s..e).enumerate() {
            let p = (m.trigram_logp[t] as f64).exp();
            let prot = is_protected(m.trigram_w3[t]);
            report.protected_trigrams += usize::from(prot);
            tri_scores.push((weigh(delta_entropy(ph, p, qbuf[i], num, den), prot), t as u32));
        }
    }
    report.bigram_bow_mean_err = if err_n > 0 { err_sum / err_n as f64 } else { 0.0 };
    report.bigram_bow_max_err = err_max;

    let mut keep_tri = vec![false; nt];
    for t in select_top(&tri_scores, max_trigrams) {
        keep_tri[t as usize] = true;
    }
    drop(tri_scores);

    // ---- Stage 2: score bigrams; bigrams with surviving trigram children are forced. ----
    let mut forced = vec![false; nb];
    for bg in 0..nb {
        let (s, e) = (m.trigram_offsets[bg] as usize, m.trigram_offsets[bg + 1] as usize);
        forced[bg] = keep_tri[s..e].iter().any(|&k| k);
    }
    let forced_count = forced.iter().filter(|&&f| f).count();
    report.forced_bigrams = forced_count;

    let uni_p = |w: u32| -> f64 {
        let lp = m.unigram_logp.get(w as usize).copied().unwrap_or(-20.0);
        if lp.is_finite() {
            (lp as f64).exp()
        } else {
            0.0
        }
    };

    let mut old_bg_mass = vec![(1.0f64, 1.0f64); num_words];
    let mut bg_scores: Vec<(f32, u32)> = Vec::with_capacity(nb - forced_count);
    let (mut err_sum, mut err_max, mut err_n) = (0.0f64, 0.0f64, 0usize);
    for w1 in 0..num_words {
        let (s, e) = (m.bigram_offsets[w1] as usize, m.bigram_offsets[w1 + 1] as usize);
        if s == e {
            continue;
        }
        let (mut sp, mut sq) = (0.0f64, 0.0f64);
        for bg in s..e {
            sp += (m.bigram_logp[bg] as f64).exp();
            sq += uni_p(m.bigram_w2[bg]);
        }
        let (num, den) = (1.0 - sp, 1.0 - sq);
        old_bg_mass[w1] = (num, den);
        let err = (bow_from_mass(num, den) - m.unigram_bow[w1] as f64).abs();
        err_sum += err;
        err_max = err_max.max(err);
        err_n += 1;
        let ph = history_unigram_prob(m, w1 as u32);
        for bg in s..e {
            if forced[bg] {
                continue;
            }
            let p = (m.bigram_logp[bg] as f64).exp();
            let q = uni_p(m.bigram_w2[bg]);
            let prot = is_protected(m.bigram_w2[bg]);
            bg_scores.push((weigh(delta_entropy(ph, p, q, num, den), prot), bg as u32));
        }
    }
    report.unigram_bow_mean_err = if err_n > 0 { err_sum / err_n as f64 } else { 0.0 };
    report.unigram_bow_max_err = err_max;

    let mut keep_bg = forced;
    for bg in select_top(&bg_scores, max_bigrams.saturating_sub(forced_count)) {
        keep_bg[bg as usize] = true;
    }
    drop(bg_scores);

    // ---- Stage 3: rebuild tables. ----
    let kept_bg_total = keep_bg.iter().filter(|&&k| k).count();
    let kept_tri_total = keep_tri.iter().filter(|&&k| k).count();
    let mut bigram_offsets = Vec::with_capacity(num_words + 1);
    let mut bigram_w2 = Vec::with_capacity(kept_bg_total);
    let mut bigram_logp = Vec::with_capacity(kept_bg_total);
    let mut bigram_bow = Vec::with_capacity(kept_bg_total);
    let mut old_bg_of_new = Vec::with_capacity(kept_bg_total);
    let mut trigram_offsets = Vec::with_capacity(kept_bg_total + 1);
    let mut trigram_w3 = Vec::with_capacity(kept_tri_total);
    let mut trigram_logp = Vec::with_capacity(kept_tri_total);
    let mut unigram_bow = m.unigram_bow.clone();

    bigram_offsets.push(0u32);
    trigram_offsets.push(0u32);
    for w1 in 0..num_words {
        let (s, e) = (m.bigram_offsets[w1] as usize, m.bigram_offsets[w1 + 1] as usize);
        let (mut sp, mut sq) = (0.0f64, 0.0f64);
        for bg in s..e {
            if !keep_bg[bg] {
                continue;
            }
            sp += (m.bigram_logp[bg] as f64).exp();
            sq += uni_p(m.bigram_w2[bg]);
            bigram_w2.push(m.bigram_w2[bg]);
            bigram_logp.push(m.bigram_logp[bg]);
            bigram_bow.push(m.bigram_bow[bg]);
            old_bg_of_new.push(bg);
            let (ts, te) = (m.trigram_offsets[bg] as usize, m.trigram_offsets[bg + 1] as usize);
            for t in ts..te {
                if keep_tri[t] {
                    trigram_w3.push(m.trigram_w3[t]);
                    trigram_logp.push(m.trigram_logp[t]);
                }
            }
            trigram_offsets.push(trigram_w3.len() as u32);
        }
        bigram_offsets.push(bigram_w2.len() as u32);
        if s < e {
            // Shift the stored weight by the change in normalisation mass. Equals the exact
            // re-normalised weight when the source is consistent, and preserves any deliberate
            // trainer offset when it is not.
            let (num_old, den_old) = old_bg_mass[w1];
            let shift = bow_from_mass(1.0 - sp, 1.0 - sq) - bow_from_mass(num_old, den_old);
            unigram_bow[w1] = (m.unigram_bow[w1] as f64 + shift) as f32;
        }
    }
    report.bigrams_after = bigram_w2.len();
    report.trigrams_after = trigram_w3.len();
    report.protected_bigrams = m.bigram_w2.iter().filter(|&&w| is_protected(w)).count();
    report.protected_bigrams_kept = bigram_w2.iter().filter(|&&w| is_protected(w)).count();
    report.protected_trigrams_kept = trigram_w3.iter().filter(|&&w| is_protected(w)).count();

    let mut pruned = Model::from_parts(
        m.word_offsets.clone(),
        m.word_bytes.clone(),
        m.unigram_logp.clone(),
        unigram_bow,
        m.exact.clone(),
        m.initial_chars.clone(),
        m.abbrev_tail.clone(),
        m.abbrev_init.clone(),
        bigram_offsets,
        bigram_w2,
        bigram_logp,
        bigram_bow,
        trigram_offsets,
        trigram_w3,
        trigram_logp,
    );

    // Trigram-level back-off weights: lower-order estimates now come from the pruned bigram model.
    let mut new_bows = Vec::with_capacity(pruned.bigram_w2.len());
    for (new_bg, &old_bg) in old_bg_of_new.iter().enumerate() {
        let w2 = pruned.bigram_w2[new_bg];
        let (s, e) = (
            pruned.trigram_offsets[new_bg] as usize,
            pruned.trigram_offsets[new_bg + 1] as usize,
        );
        let (mut sp, mut sq) = (0.0f64, 0.0f64);
        for t in s..e {
            sp += (pruned.trigram_logp[t] as f64).exp();
            sq += (pruned.bigram_logp(w2, pruned.trigram_w3[t]) as f64).exp();
        }
        let (num_old, den_old) = old_tri_mass[old_bg];
        let shift = bow_from_mass(1.0 - sp, 1.0 - sq) - bow_from_mass(num_old, den_old);
        new_bows.push((m.bigram_bow[old_bg] as f64 + shift) as f32);
    }
    pruned.bigram_bow = new_bows;

    (pruned, report)
}

/// 1-D Lloyd-Max quantizer over sorted data; returns `k` ascending centroids.
fn lloyd_1d(sorted: &[f32], k: usize) -> Vec<f32> {
    let n = sorted.len();
    if n == 0 || k == 0 {
        return vec![0.0; k.max(1)];
    }
    let mut prefix = Vec::with_capacity(n + 1);
    prefix.push(0.0f64);
    for &v in sorted {
        prefix.push(prefix.last().unwrap() + v as f64);
    }
    let mean = |lo: usize, hi: usize| -> Option<f32> {
        (hi > lo).then(|| ((prefix[hi] - prefix[lo]) / (hi - lo) as f64) as f32)
    };
    // Equal-population initialisation.
    let mut c: Vec<f32> = (0..k)
        .map(|i| {
            let lo = i * n / k;
            let hi = ((i + 1) * n / k).max(lo + 1).min(n);
            mean(lo, hi).unwrap_or(sorted[lo.min(n - 1)])
        })
        .collect();
    for _ in 0..30 {
        let mut changed = false;
        let mut lo = 0usize;
        for i in 0..k {
            let hi = if i + 1 < k {
                let b = 0.5 * (c[i] + c[i + 1]);
                lo + sorted[lo..].partition_point(|&v| v < b)
            } else {
                n
            };
            if let Some(nc) = mean(lo, hi) {
                if nc != c[i] {
                    changed = true;
                    c[i] = nc;
                }
            }
            lo = hi;
        }
        c.sort_by(f32::total_cmp);
        if !changed {
            break;
        }
    }
    c
}

/// Quantize to an 8-bit codebook. Exact zeros (common for back-off weights of leaf histories)
/// get a dedicated lossless code when they are frequent.
pub fn quantize_q8(values: &[f32]) -> ([f32; Q8_LEVELS], Vec<u8>) {
    let mut codebook = [0.0f32; Q8_LEVELS];
    if values.is_empty() {
        return (codebook, Vec::new());
    }
    let clean = |v: f32| if v.is_finite() { v } else { -99.0 };
    let zeros = values.iter().filter(|&&v| v == 0.0).count();
    let reserve_zero = zeros * 64 >= values.len();
    let mut sorted: Vec<f32> = values
        .iter()
        .copied()
        .filter(|&v| !(reserve_zero && v == 0.0))
        .map(clean)
        .collect();
    sorted.sort_by(f32::total_cmp);
    let first = usize::from(reserve_zero);
    let k = Q8_LEVELS - first;
    let cents = lloyd_1d(&sorted, k);
    for (i, &c) in cents.iter().enumerate() {
        codebook[first + i] = c;
    }
    let bounds: Vec<f32> = cents.windows(2).map(|w| 0.5 * (w[0] + w[1])).collect();
    let codes = values
        .iter()
        .map(|&v| {
            if reserve_zero && v == 0.0 {
                0u8
            } else {
                let v = clean(v);
                (first + bounds.partition_point(|&b| b < v)) as u8
            }
        })
        .collect();
    (codebook, codes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn q8_is_accurate_and_keeps_zero_exact() {
        let mut vals: Vec<f32> = (0..20_000).map(|i| -0.001 * i as f32 - 0.5).collect();
        vals.extend(std::iter::repeat(0.0).take(2_000));
        let (cb, codes) = quantize_q8(&vals);
        let mut max_err = 0.0f32;
        for (&v, &c) in vals.iter().zip(&codes) {
            let d = cb[c as usize];
            if v == 0.0 {
                assert_eq!(d, 0.0);
            }
            max_err = max_err.max((d - v).abs());
        }
        // Range is ~20 nats over 255 levels => step ~0.08, max error half a step.
        assert!(max_err < 0.05, "max quantization error {max_err}");
    }

    #[test]
    fn select_top_keeps_highest_scores() {
        let scores = vec![(0.1, 0), (5.0, 1), (0.3, 2), (2.0, 3)];
        let mut kept = select_top(&scores, 2);
        kept.sort_unstable();
        assert_eq!(kept, vec![1, 3]);
        assert_eq!(select_top(&scores, 10).len(), 4);
        assert!(select_top(&scores, 0).is_empty());
    }

    #[test]
    fn removing_a_ngram_equal_to_its_backoff_costs_nothing() {
        // p == bow * q exactly: removing it leaves the distribution unchanged.
        let (num, den) = (0.5f64, 0.6f64);
        let bow = (num / den).ln();
        let q = 0.1;
        let p = q * bow.exp();
        assert!(delta_entropy(0.2, p, q, num, den).abs() < 1e-9);
        // A sharply boosted n-gram is expensive to remove.
        assert!(delta_entropy(0.2, 0.3, 0.01, num, den) > 1e-3);
    }
}

//! Build a slim (mobile) model from a full `IMEMOD01` model.
//!
//! ```text
//! ime-slim --in models/ime_model.bin --out models/ime_model_mobile.bin \
//!          --max-bigrams 2000000 --max-trigrams 2000000 [--protect-weight inf] [--no-quantize]
//! ```
//!
//! Prunes bigrams/trigrams by relative entropy (back-off weights re-normalised) and, unless
//! `--no-quantize`, writes the n-gram tables as 8-bit codebooks (`IMEMOD02`).

use std::path::PathBuf;
use std::time::Instant;

use ime_core::model::Model;
use ime_core::slim::prune_model;

fn parse_count(s: &str) -> Result<usize, String> {
    let s = s.trim().to_ascii_lowercase().replace('_', "");
    let (num, mul) = if let Some(n) = s.strip_suffix('m') {
        (n, 1_000_000.0)
    } else if let Some(n) = s.strip_suffix('k') {
        (n, 1_000.0)
    } else {
        (s.as_str(), 1.0)
    };
    num.parse::<f64>()
        .map(|v| (v * mul).round() as usize)
        .map_err(|_| format!("bad count: {s}"))
}

fn main() {
    let mut input: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut max_bigrams = 2_000_000usize;
    let mut max_trigrams = 2_000_000usize;
    let mut quantize = true;
    let mut protect_weight = f64::INFINITY;

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut val = |name: &str| args.next().unwrap_or_else(|| die(&format!("{name} needs a value")));
        match a.as_str() {
            "--in" => input = Some(PathBuf::from(val("--in"))),
            "--out" => output = Some(PathBuf::from(val("--out"))),
            "--max-bigrams" => max_bigrams = parse_count(&val("--max-bigrams")).unwrap_or_else(|e| die(&e)),
            "--max-trigrams" => max_trigrams = parse_count(&val("--max-trigrams")).unwrap_or_else(|e| die(&e)),
            "--no-quantize" => quantize = false,
            "--protect-weight" => {
                let v = val("--protect-weight");
                protect_weight = if v.eq_ignore_ascii_case("inf") {
                    f64::INFINITY
                } else {
                    v.parse().unwrap_or_else(|_| die("bad --protect-weight"))
                }
            }
            "-h" | "--help" => {
                eprintln!("usage: ime-slim --in FULL.bin --out SLIM.bin [--max-bigrams 2M] [--max-trigrams 2M] [--protect-weight inf|W] [--no-quantize]");
                return;
            }
            other => die(&format!("unknown argument {other}")),
        }
    }
    let input = input.unwrap_or_else(|| die("--in is required"));
    let output = output.unwrap_or_else(|| die("--out is required"));

    let t = Instant::now();
    let model = Model::load(&input).unwrap_or_else(|e| die(&format!("load {}: {e}", input.display())));
    eprintln!(
        "loaded {} in {:.1}s: {} words, {} bigrams, {} trigrams",
        input.display(),
        t.elapsed().as_secs_f32(),
        model.num_words(),
        model.bigram_w2.len(),
        model.trigram_w3.len()
    );

    let t = Instant::now();
    let (slim, rep) = prune_model(&model, max_bigrams, max_trigrams, protect_weight);
    eprintln!("pruned in {:.1}s", t.elapsed().as_secs_f32());
    eprintln!(
        "  bigrams  {:>9} -> {:>9}  ({} forced by trigram children)",
        rep.bigrams_before, rep.bigrams_after, rep.forced_bigrams
    );
    eprintln!("  trigrams {:>9} -> {:>9}", rep.trigrams_before, rep.trigrams_after);
    eprintln!(
        "  protected (</s> / function word) bigrams {} -> {}, trigrams {} -> {}  (weight {})",
        rep.protected_bigrams,
        rep.protected_bigrams_kept,
        rep.protected_trigrams,
        rep.protected_trigrams_kept,
        protect_weight
    );
    eprintln!(
        "  source back-off consistency |stored - normalised|: unigram mean {:.4} max {:.4}; bigram mean {:.4} max {:.4}",
        rep.unigram_bow_mean_err, rep.unigram_bow_max_err, rep.bigram_bow_mean_err, rep.bigram_bow_max_err
    );
    drop(model);

    let res = if quantize {
        slim.save_quantized(&output)
    } else {
        slim.save(&output)
    };
    res.unwrap_or_else(|e| die(&format!("save {}: {e}", output.display())));

    // Verify the written file loads and measure quantization error.
    let reloaded = Model::load(&output).unwrap_or_else(|e| die(&format!("reload: {e}")));
    let max_err = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0f32, f32::max);
    let mean_err = |a: &[f32], b: &[f32]| {
        a.iter().zip(b).map(|(x, y)| (x - y).abs() as f64).sum::<f64>() / a.len().max(1) as f64
    };
    assert_eq!(reloaded.bigram_w2, slim.bigram_w2);
    assert_eq!(reloaded.trigram_w3, slim.trigram_w3);
    eprintln!(
        "  quantization |err| mean/max: bigram_logp {:.4}/{:.4}  bigram_bow {:.4}/{:.4}  trigram_logp {:.4}/{:.4}",
        mean_err(&reloaded.bigram_logp, &slim.bigram_logp),
        max_err(&reloaded.bigram_logp, &slim.bigram_logp),
        mean_err(&reloaded.bigram_bow, &slim.bigram_bow),
        max_err(&reloaded.bigram_bow, &slim.bigram_bow),
        mean_err(&reloaded.trigram_logp, &slim.trigram_logp),
        max_err(&reloaded.trigram_logp, &slim.trigram_logp),
    );
    let size = std::fs::metadata(&output).map(|m| m.len()).unwrap_or(0);
    eprintln!(
        "wrote {} ({:.1} MiB, {})",
        output.display(),
        size as f64 / (1024.0 * 1024.0),
        if quantize { "IMEMOD02 q8" } else { "IMEMOD01 f32" }
    );
}

fn die(msg: &str) -> ! {
    eprintln!("ime-slim: {msg}");
    std::process::exit(2);
}

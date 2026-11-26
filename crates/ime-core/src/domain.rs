//! Domain-specific boost tables parsed from curated data files.

const CS_AI_BOOST_RAW: &str = include_str!("../../../data/curated/cs_ai_boost.tsv");

/// Returns curated `(word, boost_nats)` entries for the optional CS / AI domain mode.
pub fn cs_ai_entries() -> Vec<(&'static str, f32)> {
    let mut out = Vec::new();
    for raw_line in CS_AI_BOOST_RAW.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((word_raw, val_raw)) = line.split_once('\t') else {
            continue;
        };
        let word = word_raw.trim();
        if word.chars().count() < 2 {
            continue;
        }
        if let Ok(val) = val_raw.trim().parse::<f32>() {
            if val.is_finite() && val > 0.0 {
                out.push((word, val.clamp(0.0, 10.0)));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn cs_ai_entries_are_unique_valid_and_free_of_single_chars() {
        let entries = cs_ai_entries();
        assert!(entries.len() >= 250);
        let mut seen = HashSet::new();
        for (w, b) in &entries {
            assert!(w.chars().count() >= 2, "unexpected single char: {w}");
            assert!(*b > 0.0 && *b <= 10.0, "out of range boost for {w}: {b}");
            assert!(seen.insert(*w), "duplicate cs_ai_boost entry: {w}");
        }
    }
}

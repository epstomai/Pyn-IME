"""Curate Pyn's v2 lexicon: remove verified typos, rescue high-PMI modern words from Wanxiang,
validate curated extra dictionaries and confusable pairs, and emit `.external/corpora/lexicon_words_v2.txt`.
"""
from __future__ import annotations

import argparse
import collections
import json
import math
import random
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CURATED_DIR = ROOT / "data" / "curated"
CORPORA_DIR = ROOT / ".external" / "corpora"
WANXIANG_DICTS = ROOT / ".external" / "wanxiang" / "dicts"
PINYIN_RS = ROOT / "crates" / "ime-core" / "src" / "pinyin.rs"
EVAL_SETS = ROOT / "eval" / "sets"

ANYWHERE_BAN = set(
    "的得地了着过吗呢吧啊呀啦嘛哦噢呐哒么我你他她它您咱这那哪谁啥怎在完把被让给往向从较更最很太"
)
EDGE_START_BAN = set(
    "是不有都也就还会能要可没对为跟和与或而但虽若每各某且并其即已曾将才又再刚只竟却越先去来做说看听想问找买吃喝拿放一二三四五六七八九十两几第半"
)
EDGE_END_BAN = set(
    "是不有都也就还会能要可没对为跟和与或而但虽若于到出里外前后左右中内间个些点种样件位名次遍趟回们下上时来去"
)


def load_rust_syllables() -> set[str]:
    text = PINYIN_RS.read_text(encoding="utf-8")
    m = re.search(r"pub const SYLLABLES:\s*\[&str;\s*415\]\s*=\s*\[(.*?)\];", text, re.S)
    if not m:
        raise RuntimeError(f"failed to parse SYLLABLES from {PINYIN_RS}")
    syls = set(re.findall(r'"([a-z]+)"', m.group(1)))
    if len(syls) != 415:
        raise RuntimeError(f"expected 415 syllables, got {len(syls)}")
    return syls


def load_banlist(path: Path) -> set[str]:
    ban: set[str] = set()
    for lineno, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        if "\t" in line or " " in line:
            raise ValueError(f"{path}:{lineno}: unexpected whitespace in {line!r}")
        ban.add(line)
    return ban


def load_and_validate_extra(
    path: Path, allowed: set[str], syllables: set[str], ban: set[str]
) -> dict[str, tuple[str, int]]:
    extras: dict[str, tuple[str, int]] = {}
    for lineno, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        parts = line.split("\t")
        if len(parts) != 3:
            raise ValueError(f"{path}:{lineno}: expected 3 tab columns, got {line!r}")
        word, py_str, wt_str = parts[0].strip(), parts[1].strip(), parts[2].strip()
        if not (2 <= len(word) <= 6):
            raise ValueError(f"{path}:{lineno}: word len must be 2..6: {word!r}")
        bad_chars = [c for c in word if c not in allowed]
        if bad_chars:
            raise ValueError(f"{path}:{lineno}: chars {bad_chars} not in allowed_chars: {word!r}")
        if word in ban:
            raise ValueError(f"{path}:{lineno}: word {word!r} is in typo_banlist")
        pys = py_str.split()
        if len(pys) != len(word):
            raise ValueError(f"{path}:{lineno}: syllable count mismatch for {word!r}: {pys}")
        for s in pys:
            if s not in syllables:
                raise ValueError(f"{path}:{lineno}: invalid syllable {s!r} for {word!r}")
        wt = int(wt_str)
        if not (100 <= wt <= 40000):
            raise ValueError(f"{path}:{lineno}: weight out of safe range 100..40000: {wt} for {word!r}")
        if word in extras:
            raise ValueError(f"{path}:{lineno}: duplicate word {word!r}")
        extras[word] = (py_str, wt)
    return extras


def load_and_validate_cs_ai(path: Path, allowed: set[str], ban: set[str]) -> dict[str, float]:
    boosts: dict[str, float] = {}
    for lineno, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        parts = line.split("\t")
        if len(parts) != 2:
            raise ValueError(f"{path}:{lineno}: expected 2 tab columns: {line!r}")
        word, val_str = parts[0].strip(), parts[1].strip()
        if not (2 <= len(word) <= 6):
            raise ValueError(f"{path}:{lineno}: word len must be 2..6: {word!r}")
        if not all(c in allowed for c in word):
            raise ValueError(f"{path}:{lineno}: char not in allowed_chars: {word!r}")
        if word in ban:
            raise ValueError(f"{path}:{lineno}: word {word!r} is in typo_banlist")
        val = float(val_str)
        if not (0.0 < val <= 10.0):
            raise ValueError(f"{path}:{lineno}: boost out of (0, 10.0]: {val}")
        boosts[word] = val
    return boosts


def validate_confusable_pairs(
    path: Path,
    ban: set[str],
    extras: dict[str, tuple[str, int]],
    cs_boosts: dict[str, float],
    base_lex: set[str],
) -> tuple[int, int]:
    groups = 0
    pairs = 0
    for lineno, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        parts = line.split("\t")
        if len(parts) != 2:
            raise ValueError(f"{path}:{lineno}: expected 2 tab columns: {line!r}")
        good, bads_str = parts[0].strip(), parts[1].strip()
        if good in ban:
            raise ValueError(f"{path}:{lineno}: good word {good!r} is in banlist!")
        bads = [b.strip() for b in bads_str.split("|") if b.strip()]
        if not bads:
            raise ValueError(f"{path}:{lineno}: empty bad list for {good!r}")
        for b in bads:
            if len(b) != len(good):
                raise ValueError(f"{path}:{lineno}: length mismatch {good!r} vs {b!r}")
            if b == good:
                raise ValueError(f"{path}:{lineno}: bad == good {good!r}")
            if b in extras or b in cs_boosts:
                raise ValueError(f"{path}:{lineno}: bad word {b!r} is in curated lexicon!")
            pairs += 1
        groups += 1
    return groups, pairs


def rescue_from_wanxiang(
    allowed: set[str], base_lex: set[str], ban: set[str], enable_rescue: bool = False
) -> tuple[list[tuple[int, float, str]], dict[str, int]]:
    wt: dict[str, int] = collections.defaultdict(int)
    for fname in ("zi.dict.yaml", "jichu.dict.yaml"):
        path = WANXIANG_DICTS / fname
        in_body = False
        with path.open(encoding="utf-8") as handle:
            for line in handle:
                line = line.rstrip("\r\n")
                if line == "...":
                    in_body = True
                    continue
                if not in_body or not line or line.startswith("#"):
                    continue
                parts = line.split("\t")
                w = parts[0]
                if 1 <= len(w) <= 6 and all(c in allowed for c in w):
                    v = int(parts[2]) if len(parts) > 2 and parts[2].strip().isdigit() else 1
                    wt[w] += v

    if not enable_rescue:
        return [], wt

    total_lex_wt = sum(v for w, v in wt.items() if len(w) == 1 or w in base_lex)
    log_tot = math.log(max(total_lex_wt, 1))

    def log_p(u: str) -> float:
        return math.log(max(wt.get(u, 1), 1)) - log_tot

    def internal_pmi(w: str) -> float:
        n = len(w)
        dp = [-1e9] * (n + 1)
        dp[0] = 0.0
        for i in range(n):
            if dp[i] < -1e8:
                continue
            for L in range(1, n):
                j = i + L
                if j > n:
                    break
                part = w[i:j]
                if L == 1 or part in base_lex:
                    val = dp[i] + log_p(part)
                    if val > dp[j]:
                        dp[j] = val
        return log_p(w) - dp[n]

    rescued: list[tuple[int, float, str]] = []
    for w, v in wt.items():
        n = len(w)
        if not (2 <= n <= 4):
            continue
        if w in base_lex or w in ban:
            continue
        if v < 1000:
            continue
        if any(c in ANYWHERE_BAN for c in w):
            continue
        if w[0] in EDGE_START_BAN or w[-1] in EDGE_END_BAN:
            continue
        pmi = internal_pmi(w)
        keep = False
        if n <= 3:
            keep = (v >= 2000 and pmi >= 7.0) or (v >= 1000 and pmi >= 8.2)
        else:
            keep = v >= 3000 and pmi >= 10.0
        if keep:
            rescued.append((v, pmi, w))

    rescued.sort(reverse=True)
    return rescued, wt


def main() -> None:
    parser = argparse.ArgumentParser(description="Curate Pyn's v2 lexicon and validate curated TSV assets.")
    parser.add_argument(
        "--enable-pmi-rescue",
        action="store_true",
        help="Also rescue high-PMI Wanxiang jichu words (disabled by default to avoid noisy fragments crowding 7M n-grams).",
    )
    args = parser.parse_args()
    sys.stdout.reconfigure(encoding="utf-8")
    syllables = load_rust_syllables()
    allowed = {
        line.strip()
        for line in (CORPORA_DIR / "allowed_chars.txt").read_text(encoding="utf-8").splitlines()
        if line.strip()
    }
    base_lex_list = [
        line.strip()
        for line in (CORPORA_DIR / "lexicon_words.txt").read_text(encoding="utf-8").splitlines()
        if line.strip()
    ]
    base_lex = set(base_lex_list)

    ban = load_banlist(CURATED_DIR / "typo_banlist.txt")
    extras = load_and_validate_extra(CURATED_DIR / "modern_extra.tsv", allowed, syllables, ban)
    cs_boosts = load_and_validate_cs_ai(CURATED_DIR / "cs_ai_boost.tsv", allowed, ban)
    pair_groups, pair_count = validate_confusable_pairs(
        CURATED_DIR / "confusable_pairs.tsv", ban, extras, cs_boosts, base_lex
    )

    # Check that typo_banlist never removes any expected word from the baseline eval sets.
    for path in sorted(EVAL_SETS.glob("*.jsonl")):
        with path.open(encoding="utf-8") as handle:
            for line in handle:
                row = json.loads(line)
                exp = row.get("expected") or ""
                for b in ban:
                    if b in exp:
                        print(f"[WARN] ban word {b!r} appears inside eval expected {exp!r} ({path.name})")

    rescued, wt = rescue_from_wanxiang(allowed, base_lex, ban, enable_rescue=args.enable_pmi_rescue)
    rescued_words = {w for _, _, w in rescued}

    removed_from_base = sorted(base_lex & ban)
    merged = (base_lex - ban) | rescued_words | set(extras.keys()) | set(cs_boosts.keys())

    # Check that every CS/AI boost word has a reading somewhere (either Wanxiang or modern_extra)
    missing_from_extra_and_wanxiang = [
        w for w in cs_boosts if w not in extras and wt.get(w, 0) == 0
    ]
    if missing_from_extra_and_wanxiang:
        raise RuntimeError(
            f"CS/AI boost words missing from both modern_extra.tsv and Wanxiang jichu: {missing_from_extra_and_wanxiang}"
        )

    out_path = CORPORA_DIR / "lexicon_words_v2.txt"
    out_path.write_text("\n".join(sorted(merged)) + "\n", encoding="utf-8", newline="\n")

    print(f"[curate] base lexicon: {len(base_lex):,}")
    print(f"[curate] typo banlist: {len(ban):,} (removed from base: {len(removed_from_base)}: {removed_from_base})")
    print(f"[curate] PMI rescued from Wanxiang jichu: {len(rescued):,}")
    print(f"  top 40 rescued: {[w for _, _, w in rescued[:40]]}")
    rng = random.Random(42)
    samp = rng.sample(rescued, min(40, len(rescued)))
    print(f"  random 40 rescued: {[w for _, _, w in samp]}")
    print(f"[curate] modern_extra.tsv: {len(extras):,} words (validated 100%)")
    print(f"[curate] cs_ai_boost.tsv: {len(cs_boosts):,} words (validated 100%)")
    print(f"[curate] confusable_pairs.tsv: {pair_groups:,} groups / {pair_count:,} pairs (validated 100%)")
    print(f"[curate] final lexicon_words_v2.txt: {len(merged):,} words -> {out_path}")


if __name__ == "__main__":
    main()

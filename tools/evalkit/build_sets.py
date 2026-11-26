"""Build the evaluation sets under eval/sets from the sources in eval/src.

    python tools/evalkit/build_sets.py [--tatoeba FILE.tsv.bz2] [--review]

Hand-written suites are rebuilt on every run. The general suite is rebuilt
only when --tatoeba is given, because it needs the downloaded corpus.
"""
from __future__ import annotations

import argparse
import bz2
import hashlib
import json
import re
import sys
from pathlib import Path

import jieba

sys.path.insert(0, str(Path(__file__).parent))
from pinyin_util import (  # noqa: E402
    FUNCTION_READINGS,
    RISKY_POLYPHONES,
    is_hanzi,
    keys_of,
    readings_of,
    split_clauses,
    syllables_of,
)

ROOT = Path(__file__).resolve().parents[2]
SRC = ROOT / "eval" / "src"
SETS = ROOT / "eval" / "sets"

HELDOUT_PERCENT = 40
GENERAL_SENTENCES = 2500
GENERAL_PUNCT = set("，。？！、；：“”‘’…—《》（）")
SYLLABLE = re.compile(
    r"^(b|p|m|f|d|t|n|l|g|k|h|j|q|x|zh|ch|sh|r|z|c|s|y|w)?"
    r"(a|o|e|i|u|v|ai|ei|ao|ou|an|en|ang|eng|ong|er|ia|ie|iao|iu|ian|in|iang|ing|iong"
    r"|ua|uo|uai|ui|uan|un|uang|ue|ve)$"
)
ID_PREFIX = {
    "function_words": "fw",
    "de_grammar": "dg",
    "incremental": "inc",
    "abbrev": "abr",
    "precedence": "pre",
    "general": "gen",
    "modern_orthography": "mod",
}


def digest(*parts: str) -> str:
    return hashlib.sha1("\x1f".join(parts).encode("utf-8")).hexdigest()


def split_of(key: str) -> str:
    return "heldout" if int(digest(key)[:8], 16) % 100 < HELDOUT_PERCENT else "dev"


def context_segments(text: str) -> list[dict]:
    segments = []
    for piece, hanzi in split_clauses(text):
        keys = keys_of(syllables_of(piece)) if hanzi else None
        segments.append({"text": piece, "keys": keys})
    return segments


def make_case(suite, expected, syllables, *, context=(), focus=(), gate=False,
              tags=(), source="hand", split=None, check="text", keys: str | None = None) -> dict:
    context = list(context)
    context_str = "".join(seg["text"] for seg in context)
    keys = keys_of(syllables) if keys is None else keys
    ident = digest(suite, context_str, keys, expected or "")[:8]
    return {
        "id": f"{ID_PREFIX[suite]}-{ident}",
        "suite": suite,
        "split": split or split_of(context_str + "|" + (expected or keys)),
        "gate": bool(gate),
        "check": check,
        "context": context,
        "keys": keys,
        "syllables": list(syllables),
        "expected": expected,
        "focus": sorted(focus),
        "tags": list(tags),
        "source": source,
    }


# -- hand-written suites ------------------------------------------------------

def parse_marked(sentence: str) -> tuple[str, list[int]]:
    """'我说[了]' -> ('我说了', [2])."""
    plain, focus, pos = [], [], 0
    while pos < len(sentence):
        if sentence[pos] == "[":
            if pos + 2 >= len(sentence) or sentence[pos + 2] != "]":
                raise ValueError(f"bad focus mark in {sentence!r}")
            focus.append(len(plain))
            plain.append(sentence[pos + 1])
            pos += 3
        else:
            plain.append(sentence[pos])
            pos += 1
    return "".join(plain), focus


def load_hand_cases(path: Path) -> list[dict]:
    cases, attrs = [], {}
    for number, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("@"):
            attrs = dict(item.split("=", 1) for item in line[1:].split())
            continue
        try:
            body, _, override = line.partition("||")
            context_text, sep, sentence = body.partition(">>")
            if not sep:
                context_text, sentence = "", body
            text, focus = parse_marked(sentence.strip())
            if not is_hanzi(text):
                raise ValueError("sentence must be one pure-hanzi clause")
            py_override = None
            keys_override = None
            override = override.strip()
            if override:
                for part in override.split(";"):
                    part = part.strip()
                    if not part:
                        continue
                    if part.startswith("py="):
                        py_override = part[3:].strip().split()
                    elif part.startswith("keys="):
                        keys_override = part[5:].strip()
                        if not re.fullmatch(r"[a-z]+", keys_override):
                            raise ValueError(f"bad keys= override: {keys_override!r}")
                    else:
                        raise ValueError(f"unknown override directive: {part!r}")
            if py_override is not None:
                syllables = py_override
                if len(syllables) != len(text):
                    raise ValueError("py= length does not match the sentence")
            else:
                syllables = syllables_of(text)
                for index in focus:
                    forced = FUNCTION_READINGS.get(text[index])
                    if forced:
                        syllables[index] = forced
            cases.append(make_case(
                attrs["suite"], text, syllables,
                context=context_segments(context_text.strip()),
                focus=focus, gate=attrs.get("gate") == "1",
                tags=attrs.get("tags", "").split(","),
                keys=keys_override,
            ))
        except Exception as error:
            raise SystemExit(f"{path.name}:{number}: {error}: {raw}")
    return cases


def derive_incremental(base_cases: list[dict]) -> list[dict]:
    """Cases for text committed in pieces: the tail is typed with left context."""
    derived = []

    def add(base, context_text, start, end, tag, gate_if_focus=True):
        expected = base["expected"][start:end]
        focus = [i - start for i in base["focus"] if start <= i < end]
        context = []
        if context_text:
            context = [{"text": context_text, "keys": keys_of(base["syllables"][:start])}]
        derived.append(make_case(
            "incremental", expected, base["syllables"][start:end],
            context=context, focus=focus,
            gate=base["gate"] and bool(focus) and gate_if_focus,
            tags=base["tags"] + [tag], source=f"derived:{base['id']}",
            split=base["split"],
        ))

    for base in base_cases:
        if base["context"]:
            continue
        text = base["expected"]
        size = len(text)
        bounds, pos = [], 0
        for token in jieba.cut(text):
            pos += len(token)
            bounds.append(pos)
        last_word_start = bounds[-2] if len(bounds) > 1 else 0
        final_focus = (size - 1) in base["focus"]
        contrast = "对照" in base["tags"]

        if final_focus and size >= 3 and not contrast and text[-1] in FUNCTION_READINGS:
            add(base, text[:-1], size - 1, size, "尾字单打")
        if final_focus and contrast and size - last_word_start >= 2 and last_word_start >= 2:
            add(base, text[:last_word_start], last_word_start, size, "尾词单打")

        inner = [b for b in bounds[:-1] if 2 <= b <= size - 2]
        if inner:
            cut = min(inner, key=lambda b: (abs(b - size / 2), b))
            # A cut-off first half can become ambiguous ("你懂得" / "你懂的"),
            # so it is scored but never gated.
            add(base, "", 0, cut, "半句先打", gate_if_focus=False)
            add(base, text[:cut], cut, size, "半句后打")
    return derived


# -- precedence ---------------------------------------------------------------

def common_hanzi() -> list[str]:
    """GB2312 level-1 characters (3755 most common), in code order."""
    chars = []
    for high in range(0xB0, 0xD8):
        for low in range(0xA1, 0xFF):
            try:
                chars.append(bytes([high, low]).decode("gb2312"))
            except UnicodeDecodeError:
                pass
    return chars


def build_precedence(words_path: Path) -> list[dict]:
    example = {}
    for char in common_hanzi():
        primary = syllables_of(char)[0]
        if SYLLABLE.match(primary):
            example.setdefault(primary, char)
    cases = []
    for syllable in sorted(example):
        case = make_case("precedence", None, [syllable], gate=True,
                         tags=["单音节"], source="generated", check="reading")
        case["example"] = example[syllable]
        cases.append(case)
    for raw in words_path.read_text(encoding="utf-8").splitlines():
        word = raw.strip()
        if not word or word.startswith("#"):
            continue
        # Homophones such as 知识 / 只是 have no single right answer, so a
        # word is only an example of what the keys may spell.
        case = make_case("precedence", None, syllables_of(word), gate=True,
                         tags=["词"], check="reading")
        case["example"] = word
        cases.append(case)
    return cases


# -- general (Tatoeba) --------------------------------------------------------

def build_general(tatoeba_path: Path) -> list[dict]:
    from opencc import OpenCC

    to_simplified = OpenCC("t2s")
    seen, picked = set(), []
    with bz2.open(tatoeba_path, "rt", encoding="utf-8") as handle:
        for line in handle:
            fields = line.rstrip("\n").split("\t")
            if len(fields) < 4 or fields[1] != "cmn":
                continue
            ident, text, user = fields[0], fields[2].strip(), fields[3]
            if text in seen:
                continue
            hanzi_count = sum(1 for ch in text if is_hanzi(ch))
            if not 4 <= hanzi_count <= 40:
                continue
            if any(not (is_hanzi(ch) or ch in GENERAL_PUNCT) for ch in text):
                continue
            if to_simplified.convert(text) != text:
                continue
            seen.add(text)
            picked.append((digest("tatoeba", ident), ident, text, user))
    picked.sort()
    cases = []
    for _, ident, text, user in picked[:GENERAL_SENTENCES]:
        split = split_of("tatoeba:" + ident)
        before = ""
        for piece, hanzi in split_clauses(text):
            if hanzi:
                cases.append(make_case(
                    "general", piece, syllables_of(piece),
                    context=context_segments(before), tags=["tatoeba"],
                    source=f"tatoeba:{ident}:{user}", split=split,
                ))
            before += piece
    return cases


# -- checks and output --------------------------------------------------------

def is_abbrev_of(keys: str, syllables: list[str]) -> bool:
    """True if `keys` is formed by taking a non-empty prefix of each syllable in order."""
    positions = {0}
    for syl in syllables:
        next_positions = set()
        for pos in positions:
            for length in range(1, len(syl) + 1):
                if keys.startswith(syl[:length], pos):
                    next_positions.add(pos + length)
        positions = next_positions
        if not positions:
            return False
    return len(keys) in positions


def validate(cases: list[dict]) -> list[str]:
    problems = []
    seen = {}
    for case in cases:
        if case["id"] in seen:
            problems.append(f"duplicate case {case['id']}: {case['expected']}")
        seen[case["id"]] = case
        if not re.fullmatch(r"[a-z]+", case["keys"]):
            problems.append(f"{case['id']} {case['expected']}: invalid keys {case['keys']!r}")
        if case["suite"] == "abbrev" and not is_abbrev_of(case["keys"], case["syllables"]):
            problems.append(
                f"{case['id']} {case['expected']}: keys {case['keys']!r} is not an abbreviation of {case['syllables']}"
            )
        text = case["expected"] or case.get("example", "")
        for char, syllable in zip(text, case["syllables"]):
            if syllable not in readings_of(char):
                problems.append(f"{case['id']} {text}: {char} is not read {syllable}")
        if any(i >= len(text) for i in case["focus"]):
            problems.append(f"{case['id']} {text}: focus out of range")
    return problems


def review_listing(cases: list[dict]) -> list[str]:
    lines = []
    for case in cases:
        text = case["expected"]
        risky = [i for i, ch in enumerate(text)
                 if ch in RISKY_POLYPHONES and i not in case["focus"]]
        if risky:
            marked = " ".join(
                f"{ch}:{syl}" + ("*" if i in risky else "")
                for i, (ch, syl) in enumerate(zip(text, case["syllables"]))
            )
            lines.append(f"{text}\t{marked}")
    return lines


def first_of_each(cases: list[dict]) -> list[dict]:
    """Generated suites repeat pieces such as '我们' or the same keys; keep one of each."""
    unique = {}
    for case in cases:
        unique.setdefault(case["id"], case)
    return list(unique.values())


def write_set(name: str, cases: list[dict]) -> None:
    SETS.mkdir(parents=True, exist_ok=True)
    with (SETS / f"{name}.jsonl").open("w", encoding="utf-8", newline="\n") as handle:
        for case in cases:
            handle.write(json.dumps(case, ensure_ascii=False) + "\n")
    by_split = {}
    for case in cases:
        by_split[case["split"]] = by_split.get(case["split"], 0) + 1
    gated = sum(1 for case in cases if case["gate"])
    print(f"{name:16s} {len(cases):6d} cases  gate={gated:5d}  {by_split}")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--tatoeba", type=Path)
    parser.add_argument("--review", action="store_true",
                        help="list hand-written cases with risky polyphones")
    args = parser.parse_args()
    sys.stdout.reconfigure(encoding="utf-8")
    jieba.setLogLevel(60)

    hand = load_hand_cases(SRC / "function_words.txt")
    abbrev = load_hand_cases(SRC / "abbrev.txt")
    suites = {
        "function_words": [c for c in hand if c["suite"] == "function_words"],
        "de_grammar": [c for c in hand if c["suite"] == "de_grammar"],
        "incremental": derive_incremental(hand),
        "abbrev": abbrev,
        "precedence": build_precedence(SRC / "precedence_words.txt"),
    }
    modern_src = SRC / "modern_orthography.txt"
    if modern_src.exists():
        suites["modern_orthography"] = load_hand_cases(modern_src)
    if args.tatoeba:
        suites["general"] = build_general(args.tatoeba)
    for name in ("incremental", "precedence", "general"):
        if name in suites:
            suites[name] = first_of_each(suites[name])

    problems = []
    for name, cases in suites.items():
        problems += validate(cases)
        write_set(name, cases)
    if args.review:
        print("\n-- risky polyphones (non-focus), check each reading --")
        print("\n".join(review_listing(hand + abbrev)))
    if problems:
        print("\n-- problems --")
        print("\n".join(problems))
        raise SystemExit(1)


if __name__ == "__main__":
    main()

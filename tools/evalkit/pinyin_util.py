"""Pinyin annotation helpers for building evaluation cases.

Raw keystrokes are what a user types: syllables joined with no separator,
with u-umlaut written as `v`. Segmenting them is the engine's job.
"""
from __future__ import annotations

import re
from functools import lru_cache

from pypinyin import Style, lazy_pinyin, pinyin

HANZI = re.compile(r"[一-鿿]")
HANZI_RUN = re.compile(r"[一-鿿]+")

# Reading typed for a function word when the case marks it as the focus.
FUNCTION_READINGS = {
    "了": "le",
    "的": "de",
    "得": "de",
    "地": "de",
    "着": "zhe",
    "过": "guo",
    "吗": "ma",
    "呢": "ne",
    "吧": "ba",
    "啊": "a",
    "呀": "ya",
    "啦": "la",
    "嘛": "ma",
    "哦": "o",
    "么": "me",
}

# Characters whose reading pypinyin most often gets wrong in running text.
RISKY_POLYPHONES = set("了的得地着还都行长觉重乐为和要好只种背便差朝处传当倒调分干给更几间将教结解空量落没难强少数似相兴应中转作")


# Everyday readings missing from pypinyin's table.
EXTRA_READINGS = {"嗯": {"en"}}


def is_hanzi(text: str) -> bool:
    return bool(text) and all(HANZI.fullmatch(ch) for ch in text)


def syllables_of(text: str) -> list[str]:
    """Toneless syllables for a pure-hanzi string, u-umlaut as `v`."""
    if not is_hanzi(text):
        raise ValueError(f"not pure hanzi: {text!r}")
    out = lazy_pinyin(text, style=Style.NORMAL, v_to_u=False, errors="ignore")
    if len(out) != len(text):
        raise ValueError(f"pinyin length mismatch for {text!r}: {out}")
    return [normalize_syllable(s) for s in out]


def normalize_syllable(syllable: str) -> str:
    return syllable.lower().replace("ü", "v").replace("u:", "v")


@lru_cache(maxsize=None)
def readings_of(char: str) -> frozenset[str]:
    """All toneless readings pypinyin knows for one character."""
    found = pinyin(char, style=Style.NORMAL, heteronym=True, v_to_u=False)
    readings = {normalize_syllable(s) for s in (found[0] if found else [])}
    return frozenset(readings | EXTRA_READINGS.get(char, set()))


def keys_of(syllables: list[str]) -> str:
    return "".join(syllables)


def split_clauses(sentence: str) -> list[tuple[str, bool]]:
    """Split text into (piece, is_hanzi_run) pieces, keeping everything."""
    pieces: list[tuple[str, bool]] = []
    pos = 0
    for match in HANZI_RUN.finditer(sentence):
        if match.start() > pos:
            pieces.append((sentence[pos : match.start()], False))
        pieces.append((match.group(), True))
        pos = match.end()
    if pos < len(sentence):
        pieces.append((sentence[pos:], False))
    return pieces

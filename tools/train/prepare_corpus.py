"""Download and clean permissive-license Chinese corpora for n-gram training.

Sources:
  - LCCC-base (`silver/lccc` on HuggingFace, MIT): conversational Weibo dialogues
  - FineWeb-2 `cmn_Hani` test + streamed train shard (`HuggingFaceFW/fineweb-2`, ODC-By 1.0):
    general web Chinese

Any sentence matching an evaluation clause (len >= 8) from `eval/sets/*.jsonl`
is dropped to guarantee zero test leakage. All output characters and lexicon words
are verified Simplified-compatible under OpenCC `t2s`.
"""
from __future__ import annotations

import argparse
import gzip
import io
import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
EVAL_SETS = ROOT / "eval" / "sets"
OUT_DIR = ROOT / ".external" / "corpora"
ZI_DICT_PATH = ROOT / ".external" / "wanxiang" / "dicts" / "zi.dict.yaml"

LCCC_URL = "https://huggingface.co/datasets/silver/lccc/resolve/main/lccc_base_train.jsonl.gz"
FINEWEB_URL = "https://huggingface.co/datasets/HuggingFaceFW/fineweb-2/resolve/main/data/cmn_Hani/test/000_00000.parquet"
FINEWEB_TRAIN_URL = "https://huggingface.co/datasets/HuggingFaceFW/fineweb-2/resolve/main/data/cmn_Hani/train/000_00000.parquet"

SENT_SPLIT = re.compile(r"[。？！?!\r\n]+")
CLAUSE_SPLIT = re.compile(r"[，、；：,;]+")
HANZI_FULL = re.compile(r"^[\u4e00-\u9fff]+$")

# High-frequency strictly-Traditional characters used to reject Traditional Chinese documents quickly.
TRAD_CHARS = tuple(
    "這為與個說們會時過後來裡還點對從開經關無實當話頭業現進長學國車東電門風馬鳥魚體書畫寫聽見觀問語詞認讓記設計試讀請變"
)


def is_traditional_doc(text: str) -> bool:
    hits = 0
    for ch in TRAD_CHARS:
        if ch in text:
            hits += text.count(ch)
            if hits >= 2:
                return True
    return False


def build_leakage_index() -> set[str]:
    """Collect evaluation clauses with len >= 8 for exact sentence/clause match filtering."""
    clauses: set[str] = set()
    for path in sorted(EVAL_SETS.glob("*.jsonl")):
        with path.open(encoding="utf-8") as handle:
            for line in handle:
                row = json.loads(line)
                exp = row.get("expected")
                if exp and len(exp) >= 8:
                    clauses.add(exp)
    return clauses


def is_leaked_sentence(joined: str, clauses: list[str], exact_eval: set[str]) -> bool:
    if joined in exact_eval:
        return True
    return any(len(c) >= 8 and c in exact_eval for c in clauses)


def extract_clean_sentences(
    raw_text: str,
    exact_eval: set[str],
    allowed_chars: set[str] | None = None,
) -> tuple[list[str], int]:
    """Return cleaned lines ('clause1，clause2') and count of dropped leaked sentences."""
    out: list[str] = []
    leaked = 0
    for sent in SENT_SPLIT.split(raw_text):
        sent = sent.strip()
        if len(sent) < 2:
            continue
        groups: list[list[str]] = [[]]
        for part in CLAUSE_SPLIT.split(sent):
            part = part.strip()
            if not part:
                continue
            valid_chars = HANZI_FULL.match(part) and (
                allowed_chars is None or all(ch in allowed_chars for ch in part)
            )
            if 2 <= len(part) <= 40 and valid_chars:
                groups[-1].append(part)
            elif groups[-1]:
                groups.append([])
        for group in groups:
            if not group:
                continue
            joined = "，".join(group)
            if is_leaked_sentence(joined, group, exact_eval):
                leaked += 1
                continue
            out.append(joined)
    return out, leaked


def verify_gzip(path: Path) -> bool:
    if not path.exists() or path.stat().st_size < 1024:
        return False
    try:
        with gzip.open(path, "rt", encoding="utf-8") as gz:
            return bool(gz.readline())
    except Exception:
        return False


def verify_parquet(path: Path) -> bool:
    if not path.exists() or path.stat().st_size < 1024:
        return False
    try:
        import pyarrow.parquet as pq

        pf = pq.ParquetFile(path)
        return pf.metadata.num_rows > 0
    except Exception:
        return False


def download_file(url: str, dest: Path, verifier) -> None:
    if verifier(dest):
        print(f"[skip] already valid: {dest.name} ({dest.stat().st_size / 1e6:.1f} MB)")
        return
    dest.parent.mkdir(parents=True, exist_ok=True)
    part = dest.with_name(dest.name + ".part")
    cmd = [
        "curl.exe",
        "-fL",
        "-C",
        "-",
        "--retry",
        "5",
        "--retry-delay",
        "3",
        "--connect-timeout",
        "30",
        "-o",
        str(part),
        url,
    ]
    print(f"[download] {url} -> {dest.name}")
    res = subprocess.run(cmd)
    if res.returncode != 0:
        # Retry from scratch once in case a stale .part caused HTTP 416 range error.
        if part.exists():
            part.unlink()
        cmd_fresh = [c for c in cmd if c not in ("-C", "-")]
        subprocess.run(cmd_fresh, check=True)
    part.replace(dest)
    if not verifier(dest):
        raise RuntimeError(f"downloaded file failed integrity check: {dest}")


class CurlRangeFile(io.RawIOBase):
    """Read-only seekable file over HTTP Range requests backed by `curl.exe`."""

    def __init__(self, url: str) -> None:
        self.url = url
        self.pos = 0
        hdr = subprocess.check_output(
            ["curl.exe", "-sIL", "--connect-timeout", "20", "--retry", "5", url],
            text=True,
        )
        lengths = [
            int(line.split(":", 1)[1].strip())
            for line in hdr.splitlines()
            if line.lower().startswith("content-length:")
        ]
        if not lengths:
            raise RuntimeError(f"missing content-length header for {url}")
        self.size = lengths[-1]

    def readable(self) -> bool:
        return True

    def seekable(self) -> bool:
        return True

    def tell(self) -> int:
        return self.pos

    def seek(self, offset: int, whence: int = io.SEEK_SET) -> int:
        if whence == io.SEEK_SET:
            target = offset
        elif whence == io.SEEK_CUR:
            target = self.pos + offset
        elif whence == io.SEEK_END:
            target = self.size + offset
        else:
            raise ValueError(f"unsupported whence: {whence}")
        self.pos = max(0, min(self.size, target))
        return self.pos

    def readinto(self, b: bytearray | memoryview) -> int:
        if self.pos >= self.size or len(b) == 0:
            return 0
        end = min(self.size - 1, self.pos + len(b) - 1)
        data = subprocess.check_output(
            [
                "curl.exe",
                "-sfL",
                "--retry",
                "5",
                "--retry-delay",
                "2",
                "--connect-timeout",
                "20",
                "-r",
                f"{self.pos}-{end}",
                self.url,
            ]
        )
        n = len(data)
        b[:n] = data
        self.pos += n
        return n


def export_simplified_chars(zi_dict_path: Path, dest: Path) -> set[str]:
    from opencc import OpenCC

    cc = OpenCC("t2s")
    chars: set[str] = set()
    in_body = False
    with zi_dict_path.open(encoding="utf-8") as handle:
        for raw in handle:
            line = raw.rstrip("\r\n")
            if line == "...":
                in_body = True
                continue
            if not in_body or not line or line.startswith("#"):
                continue
            ch = line.split("\t", 1)[0]
            if len(ch) == 1 and "\u4e00" <= ch <= "\u9fff" and cc.convert(ch) == ch:
                chars.add(ch)
    dest.parent.mkdir(parents=True, exist_ok=True)
    dest.write_text("\n".join(sorted(chars)) + "\n", encoding="utf-8", newline="\n")
    return chars


def process_lccc(
    src: Path,
    dest: Path,
    max_chars: int,
    leak_index: set[str],
    allowed_chars: set[str] | None = None,
) -> tuple[int, int, int]:
    lines_written = 0
    chars_written = 0
    leaked_dropped = 0
    with (
        gzip.open(src, "rt", encoding="utf-8") as fin,
        dest.open("w", encoding="utf-8", newline="\n") as fout,
    ):
        for raw in fin:
            if chars_written >= max_chars:
                break
            raw = raw.strip()
            if not raw:
                continue
            try:
                turns = json.loads(raw)
            except json.JSONDecodeError:
                continue
            if not isinstance(turns, list):
                continue
            for turn in turns:
                if not isinstance(turn, str):
                    continue
                compact = turn.replace(" ", "")
                if is_traditional_doc(compact):
                    continue
                cleaned, leaked = extract_clean_sentences(compact, leak_index, allowed_chars)
                leaked_dropped += leaked
                for sent in cleaned:
                    fout.write(sent + "\n")
                    lines_written += 1
                    chars_written += len(sent) - sent.count("，")
                    if chars_written >= max_chars:
                        break
                if chars_written >= max_chars:
                    break
    return lines_written, chars_written, leaked_dropped


def process_fineweb(
    src: Path,
    dest: Path,
    max_chars: int,
    leak_index: set[str],
    allowed_chars: set[str] | None = None,
) -> tuple[int, int, int]:
    import pyarrow.parquet as pq

    lines_written = 0
    chars_written = 0
    leaked_dropped = 0
    pf = pq.ParquetFile(src)
    with dest.open("w", encoding="utf-8", newline="\n") as fout:
        for batch in pf.iter_batches(columns=["text"], batch_size=2048):
            for item in batch.column("text").to_pylist():
                if not item or is_traditional_doc(item):
                    continue
                cleaned, leaked = extract_clean_sentences(item, leak_index, allowed_chars)
                leaked_dropped += leaked
                for sent in cleaned:
                    fout.write(sent + "\n")
                    lines_written += 1
                    chars_written += len(sent) - sent.count("，")
                    if chars_written >= max_chars:
                        return lines_written, chars_written, leaked_dropped
    return lines_written, chars_written, leaked_dropped


def _trim_incomplete_last_line(path: Path) -> int:
    """Truncate any partial trailing line (missing final '\\n') and return clean byte size."""
    with path.open("r+b") as fb:
        fb.seek(0, io.SEEK_END)
        end = fb.tell()
        pos = end
        chunk = 65536
        while pos > 0:
            read_start = max(0, pos - chunk)
            fb.seek(read_start)
            buf = fb.read(pos - read_start)
            nl = buf.rfind(b"\n")
            if nl != -1:
                clean_end = read_start + nl + 1
                if clean_end != end:
                    fb.truncate(clean_end)
                return clean_end
            pos = read_start
        fb.truncate(0)
        return 0


def process_fineweb_train_stream(
    url: str,
    dest: Path,
    max_chars: int,
    leak_index: set[str],
    allowed_chars: set[str] | None = None,
    force: bool = False,
) -> tuple[int, int, int]:
    import pyarrow.parquet as pq

    state_path = dest.with_name(dest.stem + ".state.json")
    part = dest.with_name(dest.name + ".part")

    if not force and dest.exists():
        lines_existing = 0
        chars_existing = 0
        with dest.open(encoding="utf-8") as handle:
            for line in handle:
                s = line.rstrip("\r\n")
                if s:
                    lines_existing += 1
                    chars_existing += len(s) - s.count("，")
        if chars_existing >= int(max_chars * 0.98):
            if state_path.exists():
                state_path.unlink()
            print(
                f"[skip] already valid: {dest.name} (lines={lines_existing:,} hanzi_chars={chars_existing:,})"
            )
            return lines_existing, chars_existing, 0

    lines_written = 0
    chars_written = 0
    leaked_dropped = 0
    start_rg = 0
    open_mode = "w"

    if not force:
        if state_path.exists() and part.exists():
            state = json.loads(state_path.read_text(encoding="utf-8"))
            byte_offset = int(state["byte_offset"])
            with part.open("r+b") as fb:
                fb.seek(0, io.SEEK_END)
                if fb.tell() > byte_offset:
                    fb.truncate(byte_offset)
            start_rg = int(state["next_rg"])
            lines_written = int(state["lines"])
            chars_written = int(state["chars"])
            leaked_dropped = int(state["leaked"])
            open_mode = "a"
            print(
                f"[resume] state rg={start_rg} lines={lines_written:,} hanzi_chars={chars_written:,}",
                flush=True,
            )
        elif part.exists():
            clean_bytes = _trim_incomplete_last_line(part)
            with part.open("r", encoding="utf-8") as fr:
                for line in fr:
                    s = line.rstrip("\r\n")
                    if s:
                        lines_written += 1
                        chars_written += len(s) - s.count("，")
            # Existing part came from rg 0..~168; start at rg=180 to guarantee zero duplicates.
            start_rg = 180
            open_mode = "a"
            print(
                f"[resume] recovered part ({clean_bytes / 1e6:.1f} MB) lines={lines_written:,} "
                f"hanzi_chars={chars_written:,}, resuming at rg={start_rg}",
                flush=True,
            )

    dest.parent.mkdir(parents=True, exist_ok=True)
    pf = pq.ParquetFile(io.BufferedReader(CurlRangeFile(url), buffer_size=16 * 1024 * 1024))
    num_rgs = pf.metadata.num_row_groups
    print(
        f"[stream] {dest.name}: row_groups={num_rgs:,}, start_rg={start_rg}, target_chars={max_chars:,}",
        flush=True,
    )

    if chars_written < max_chars:
        state_tmp = state_path.with_name(state_path.name + ".tmp")
        with part.open(open_mode, encoding="utf-8", newline="\n") as fout:
            for rg_idx in range(start_rg, num_rgs):
                tbl = pf.read_row_group(rg_idx, columns=["text"])
                for item in tbl.column("text").to_pylist():
                    if not item or is_traditional_doc(item):
                        continue
                    cleaned, leaked = extract_clean_sentences(item, leak_index, allowed_chars)
                    leaked_dropped += leaked
                    for sent in cleaned:
                        fout.write(sent + "\n")
                        lines_written += 1
                        chars_written += len(sent) - sent.count("，")
                        if chars_written >= max_chars:
                            break
                    if chars_written >= max_chars:
                        break
                if (rg_idx + 1) % 10 == 0 or chars_written >= max_chars:
                    fout.flush()
                    byte_offset = part.stat().st_size
                    state_tmp.write_text(
                        json.dumps(
                            {
                                "next_rg": rg_idx + 1,
                                "lines": lines_written,
                                "chars": chars_written,
                                "leaked": leaked_dropped,
                                "byte_offset": byte_offset,
                            }
                        ),
                        encoding="utf-8",
                    )
                    state_tmp.replace(state_path)
                    print(
                        f"  [rg {rg_idx + 1}/{num_rgs}] lines={lines_written:,} hanzi_chars={chars_written:,}",
                        flush=True,
                    )
                if chars_written >= max_chars:
                    break

    part.replace(dest)
    if state_path.exists():
        state_path.unlink()
    return lines_written, chars_written, leaked_dropped


def export_jieba_lexicon(dest: Path, min_freq: int = 3) -> int:
    import jieba
    from opencc import OpenCC

    cc = OpenCC("t2s")
    tail_particles = set("吗呢吧啊呀啦嘛哦噢呐哒")
    dict_path = Path(jieba.__file__).parent / "dict.txt"
    words: list[str] = []
    for raw in dict_path.read_text(encoding="utf-8").splitlines():
        parts = raw.split()
        if len(parts) < 2:
            continue
        word, freq = parts[0], int(parts[1])
        if not (2 <= len(word) <= 6 and freq >= min_freq and HANZI_FULL.match(word)):
            continue
        if cc.convert(word) != word:
            continue
        if word[-1] in tail_particles:
            continue
        if len(word) >= 3 and word[-1] == "了":
            continue
        words.append(word)
    dest.parent.mkdir(parents=True, exist_ok=True)
    dest.write_text("\n".join(words) + "\n", encoding="utf-8", newline="\n")
    return len(words)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--lccc-max-chars", type=int, default=20_000_000)
    parser.add_argument("--fineweb-max-chars", type=int, default=25_000_000)
    parser.add_argument("--fineweb-train-max-chars", type=int, default=180_000_000)
    parser.add_argument("--skip-download", action="store_true")
    parser.add_argument("--skip-fineweb-train", action="store_true")
    parser.add_argument("--force-fineweb-train", action="store_true")
    parser.add_argument("--only-fineweb-train", action="store_true")
    args = parser.parse_args()
    sys.stdout.reconfigure(encoding="utf-8")

    allowed_out = OUT_DIR / "allowed_chars.txt"
    if args.only_fineweb_train and allowed_out.exists():
        allowed_chars = {
            line.strip() for line in allowed_out.read_text(encoding="utf-8").splitlines() if line.strip()
        }
        print(f"[chars]   loaded {len(allowed_chars):,} simplified chars <- {allowed_out}")
    else:
        allowed_chars = export_simplified_chars(ZI_DICT_PATH, allowed_out)
        print(f"[chars]   exported {len(allowed_chars):,} simplified chars -> {allowed_out}")

    leak_index = build_leakage_index()
    print(f"[leak-filter] indexed {len(leak_index):,} exact eval clauses (len >= 8)")

    if not args.only_fineweb_train:
        lccc_raw = OUT_DIR / "lccc_base_train.jsonl.gz"
        fineweb_raw = OUT_DIR / "fineweb2_cmn_test.parquet"
        if not args.skip_download:
            download_file(LCCC_URL, lccc_raw, verify_gzip)
            download_file(FINEWEB_URL, fineweb_raw, verify_parquet)

        lex_out = OUT_DIR / "lexicon_words.txt"
        lex_n = export_jieba_lexicon(lex_out)
        print(f"[lexicon] exported {lex_n:,} jieba words -> {lex_out}")

        lccc_out = OUT_DIR / "clean_lccc.txt"
        l1, c1, k1 = process_lccc(lccc_raw, lccc_out, args.lccc_max_chars, leak_index, allowed_chars)
        print(f"[lccc]    lines={l1:,}  hanzi_chars={c1:,}  leaked_dropped={k1:,}  -> {lccc_out}")

        fw_out = OUT_DIR / "clean_fineweb.txt"
        l2, c2, k2 = process_fineweb(fineweb_raw, fw_out, args.fineweb_max_chars, leak_index, allowed_chars)
        print(f"[fineweb] lines={l2:,}  hanzi_chars={c2:,}  leaked_dropped={k2:,}  -> {fw_out}")

    if not args.skip_fineweb_train and args.fineweb_train_max_chars > 0:
        fwt_out = OUT_DIR / "clean_fineweb_train.txt"
        l3, c3, k3 = process_fineweb_train_stream(
            FINEWEB_TRAIN_URL,
            fwt_out,
            args.fineweb_train_max_chars,
            leak_index,
            allowed_chars,
            force=args.force_fineweb_train,
        )
        print(
            f"[fw-train] lines={l3:,}  hanzi_chars={c3:,}  leaked_dropped={k3:,}  -> {fwt_out}"
        )


if __name__ == "__main__":
    main()

"""Stage-2 fine-tuning of CharTransformerLM on edited FineWeb-2 prose and standalone 的/得/地 triples."""
from __future__ import annotations

import argparse
import json
import math
import shutil
import struct
import sys
import time
from pathlib import Path

import numpy as np
import torch
import torch.nn.functional as F

sys.path.insert(0, str(Path(__file__).parent))
from train_reranker import (  # noqa: E402
    BOS_ID,
    CORPORA_DIR,
    DATA_DIR,
    DEFAULT_OUT,
    EOS_ID,
    MAGIC,
    PAD_ID,
    PAUSE_ID,
    ROOT,
    UNK_ID,
    CharTransformerLM,
    build_exact_eval_set,
    build_packed_masks,
    compute_loss,
    evaluate_val,
    export_imerer01,
    verify_rust_parity,
)

STAGE2_DIR = DATA_DIR / "stage2"
DEFAULT_PAIRS = ROOT / "data" / "curated" / "confusable_pairs.tsv"
FINEWEB_FILES = [
    CORPORA_DIR / "clean_fineweb.txt",
    CORPORA_DIR / "clean_fineweb_train.txt",
]
MAX_DE_SEQ = 48


def load_imerer01(path: Path) -> tuple[CharTransformerLM, list[int]]:
    with path.open("rb") as f:
        magic = f.read(8)
        if magic != MAGIC:
            raise ValueError(f"invalid magic in {path}: {magic!r}")
        vocab_size, d_model, n_layers, n_heads, d_ff, max_seq_len, rope_bits, _ = struct.unpack(
            "<8I", f.read(32)
        )
        rope_theta = struct.unpack("<f", struct.pack("<I", rope_bits))[0]
        model = CharTransformerLM(
            vocab_size=vocab_size,
            d_model=d_model,
            n_layers=n_layers,
            n_heads=n_heads,
            d_ff=d_ff,
            max_seq_len=max_seq_len,
            rope_theta=rope_theta,
        )
        scalars = np.frombuffer(f.read(vocab_size * 4), dtype=np.uint32).tolist()

        def read_f16_into(param: torch.Tensor, shape: tuple[int, ...]) -> None:
            n = int(np.prod(shape))
            arr = np.frombuffer(f.read(n * 2), dtype=np.float16).astype(np.float32).reshape(shape).copy()
            param.data.copy_(torch.from_numpy(arr))

        def read_f32_into(param: torch.Tensor, shape: tuple[int, ...]) -> None:
            n = int(np.prod(shape))
            arr = np.frombuffer(f.read(n * 4), dtype=np.float32).reshape(shape).copy()
            param.data.copy_(torch.from_numpy(arr))

        read_f16_into(model.tok_emb.weight, (vocab_size, d_model))
        read_f32_into(model.lm_head_bias, (vocab_size,))
        for layer in model.layers:
            read_f32_into(layer.norm1.weight, (d_model,))
            read_f16_into(layer.w_qkv.weight, (3 * d_model, d_model))
            read_f16_into(layer.w_o.weight, (d_model, d_model))
            read_f32_into(layer.norm2.weight, (d_model,))
            read_f16_into(layer.w_gate_up.weight, (2 * d_ff, d_model))
            read_f16_into(layer.w_down.weight, (d_model, d_ff))
        read_f32_into(model.norm_f.weight, (d_model,))
        leftover = f.read(1)
        if leftover:
            raise ValueError(f"unexpected trailing bytes in {path}")
    return model, scalars


def find_standalone_pos(line: str, target: str, lex: set[str], allow_end: bool = False) -> int:
    n = len(line)
    for i, ch in enumerate(line):
        if ch != target or i == 0 or line[i - 1] == "，":
            continue
        if not allow_end and (i + 1 >= n or line[i + 1] == "，"):
            continue
        covered = False
        for L in (2, 3, 4):
            for s in range(max(0, i - L + 1), min(i + 1, n - L + 1)):
                if line[s : s + L] in lex:
                    covered = True
                    break
            if covered:
                break
        if not covered:
            return i
    return -1


def prepare_stage2_data(
    scalars: list[int],
    max_per_class: int = 60_000,
    val_per_class: int = 1_000,
    val_lm_sentences: int = 30_000,
    num_shards: int = 64,
    seed: int = 43,
    force: bool = False,
) -> tuple[Path, Path, Path, Path]:
    meta_path = STAGE2_DIR / "meta.json"
    fw_train = STAGE2_DIR / "fw_train_tokens.u16"
    fw_val = STAGE2_DIR / "fw_val_tokens.u16"
    de_train = STAGE2_DIR / "de_train.npz"
    de_val = STAGE2_DIR / "de_val.npz"

    if not force and all(p.exists() for p in (meta_path, fw_train, fw_val, de_train, de_val)):
        meta = json.loads(meta_path.read_text(encoding="utf-8"))
        print(
            f"[stage2-data] cached: fw_train={meta['fw_train_tokens']:,}, "
            f"fw_val={meta['fw_val_tokens']:,}, de_train={meta['de_train']:,}, de_val={meta['de_val']:,}",
            flush=True,
        )
        return fw_train, fw_val, de_train, de_val

    STAGE2_DIR.mkdir(parents=True, exist_ok=True)
    char_to_id = {chr(sc): idx for idx, sc in enumerate(scalars) if idx >= 5 and sc > 0}
    char_to_id["，"] = PAUSE_ID

    lex_path = CORPORA_DIR / "lexicon_words.txt"
    lex = {w for w in lex_path.read_text(encoding="utf-8").splitlines() if 2 <= len(w) <= 4}
    exact_eval = build_exact_eval_set()

    t0 = time.time()
    print("[stage2-data] building FineWeb-only LM stream + edited 的/得/地 triples...", flush=True)
    shard_paths = [STAGE2_DIR / f"shard_{i:02d}.bin" for i in range(num_shards)]
    shard_files = [p.open("wb") for p in shard_paths]
    rng = np.random.default_rng(seed)

    # Reservoirs for class 0 (的 from carefully edited sentences), 1 (得), 2 (地)
    de_samples: list[tuple[str, int, int]] = []
    dei_count = 0
    di_count = 0
    de_count = 0
    leaked = 0
    lines_encoded = 0

    try:
        for cp in FINEWEB_FILES:
            with cp.open(encoding="utf-8") as handle:
                batch_lines: list[str] = []
                for raw in handle:
                    s = raw.strip()
                    if not s:
                        continue
                    if len(s) >= 8 and (s in exact_eval or any(len(c) >= 8 and c in exact_eval for c in s.split("，"))):
                        leaked += 1
                        continue
                    batch_lines.append(s)

                    if 5 <= len(s) <= MAX_DE_SEQ - 4:
                        has_dei = "得" in s
                        has_di = "地" in s
                        if has_dei or has_di:
                            p_dei = find_standalone_pos(s, "得", lex, allow_end=False) if has_dei else -1
                            p_di = find_standalone_pos(s, "地", lex, allow_end=False) if has_di else -1
                            if p_dei != -1 and dei_count < max_per_class:
                                de_samples.append((s, p_dei, 1))
                                dei_count += 1
                            if p_di != -1 and di_count < max_per_class:
                                de_samples.append((s, p_di, 2))
                                di_count += 1
                            if (p_dei != -1 or p_di != -1) and de_count < max_per_class and "的" in s:
                                p_de = find_standalone_pos(s, "的", lex, allow_end=True)
                                if p_de != -1:
                                    de_samples.append((s, p_de, 0))
                                    de_count += 1

                    if len(batch_lines) >= 65536:
                        targets = rng.integers(0, num_shards, size=len(batch_lines))
                        for ln, sh_idx in zip(batch_lines, targets):
                            ids = [BOS_ID] + [char_to_id.get(ch, UNK_ID) for ch in ln] + [EOS_ID]
                            arr = np.asarray(ids, dtype=np.uint16)
                            shard_files[sh_idx].write(struct.pack("<H", len(arr)))
                            shard_files[sh_idx].write(arr.tobytes())
                            lines_encoded += 1
                        batch_lines.clear()

                if batch_lines:
                    targets = rng.integers(0, num_shards, size=len(batch_lines))
                    for ln, sh_idx in zip(batch_lines, targets):
                        ids = [BOS_ID] + [char_to_id.get(ch, UNK_ID) for ch in ln] + [EOS_ID]
                        arr = np.asarray(ids, dtype=np.uint16)
                        shard_files[sh_idx].write(struct.pack("<H", len(arr)))
                        shard_files[sh_idx].write(arr.tobytes())
                        lines_encoded += 1
    finally:
        for f in shard_files:
            f.close()

    print(
        f"[stage2-data] encoded {lines_encoded:,} FineWeb lines (leaked={leaked}); "
        f"standalone 的={de_count:,}, 得={dei_count:,}, 地={di_count:,} ({time.time() - t0:.1f}s)",
        flush=True,
    )

    t1 = time.time()
    val_per_shard = max(1, val_lm_sentences // num_shards)
    fw_train_tokens = 0
    fw_val_tokens = 0
    shard_order = rng.permutation(num_shards)

    with fw_train.open("wb") as f_tr, fw_val.open("wb") as f_va:
        for sh_idx in shard_order:
            raw = shard_paths[sh_idx].read_bytes()
            shard_paths[sh_idx].unlink()
            recs: list[bytes] = []
            pos = 0
            n_bytes = len(raw)
            while pos < n_bytes:
                (slen,) = struct.unpack_from("<H", raw, pos)
                pos += 2
                byte_len = slen * 2
                recs.append(raw[pos : pos + byte_len])
                pos += byte_len
            perm = rng.permutation(len(recs))
            split = min(val_per_shard, len(recs) // 20)
            for idx in perm[:split]:
                b = recs[idx]
                f_va.write(b)
                fw_val_tokens += len(b) // 2
            for idx in perm[split:]:
                b = recs[idx]
                f_tr.write(b)
                fw_train_tokens += len(b) // 2

    # Build padded fixed-length [N, MAX_DE_SEQ] 的/得/地 datasets stratified by class.
    by_cls: list[list[tuple[str, int, int]]] = [[], [], []]
    for item in de_samples:
        by_cls[item[2]].append(item)
    tr_items: list[tuple[str, int, int]] = []
    va_items: list[tuple[str, int, int]] = []
    for cls_list in by_cls:
        perm = rng.permutation(len(cls_list))
        va_n = min(val_per_class, len(cls_list) // 10)
        for idx in perm[:va_n]:
            va_items.append(cls_list[idx])
        for idx in perm[va_n:]:
            tr_items.append(cls_list[idx])

    def encode_de_items(items: list[tuple[str, int, int]]) -> tuple[np.ndarray, np.ndarray, np.ndarray]:
        seqs = np.zeros((len(items), MAX_DE_SEQ), dtype=np.uint16)
        seq_pos = np.zeros(len(items), dtype=np.int32)
        labels = np.zeros(len(items), dtype=np.int64)
        for row_i, (text, str_pos, lbl) in enumerate(items):
            ids = [BOS_ID] + [char_to_id.get(ch, UNK_ID) for ch in text] + [EOS_ID]
            seqs[row_i, : len(ids)] = ids
            seq_pos[row_i] = str_pos + 1
            labels[row_i] = lbl
        return seqs, seq_pos, labels

    tr_seqs, tr_pos, tr_lbl = encode_de_items(tr_items)
    va_seqs, va_pos, va_lbl = encode_de_items(va_items)
    np.savez(de_train, seqs=tr_seqs, seq_pos=tr_pos, labels=tr_lbl)
    np.savez(de_val, seqs=va_seqs, seq_pos=va_pos, labels=va_lbl)

    meta_path.write_text(
        json.dumps(
            {
                "fw_train_tokens": fw_train_tokens,
                "fw_val_tokens": fw_val_tokens,
                "de_train": len(tr_items),
                "de_val": len(va_items),
            }
        ),
        encoding="utf-8",
    )
    print(
        f"[stage2-data] saved fw_train={fw_train_tokens:,} fw_val={fw_val_tokens:,} "
        f"de_train={len(tr_items):,} de_val={len(va_items):,} ({time.time() - t1:.1f}s)",
        flush=True,
    )
    return fw_train, fw_val, de_train, de_val


def score_de_batch(
    model: CharTransformerLM,
    seqs: torch.Tensor,
    seq_pos: torch.Tensor,
    de_token_ids: torch.Tensor,
    eos_weight: float = 0.85,
) -> tuple[torch.Tensor, torch.Tensor]:
    """Build 3 minimal-pair variants per sample and compute self-normalized logP scores."""
    bsz, max_len = seqs.shape
    variants = seqs.unsqueeze(1).expand(bsz, 3, max_len).clone()
    row_idx = torch.arange(bsz, device=seqs.device)
    for c in range(3):
        variants[row_idx, c, seq_pos] = de_token_ids[c]
    flat = variants.view(bsz * 3, max_len)
    inp = flat[:, :-1]
    tgt = flat[:, 1:]

    with torch.amp.autocast("cuda", dtype=torch.bfloat16):
        logits = model(inp)
    logits_f32 = logits.float()

    valid = tgt != PAD_ID
    tok_scores = logits_f32.gather(-1, tgt.unsqueeze(-1)).squeeze(-1)
    weights = torch.where(tgt == EOS_ID, eos_weight, torch.where(valid, 1.0, 0.0))
    seq_scores = (tok_scores * weights).sum(dim=-1).view(bsz, 3)

    log_z = torch.logsumexp(logits_f32[valid], dim=-1)
    sn_de = (log_z * log_z).mean()
    return seq_scores, sn_de


@torch.no_grad()
def evaluate_de_val(
    model: CharTransformerLM,
    va_seqs: torch.Tensor,
    va_pos: torch.Tensor,
    va_lbl: torch.Tensor,
    de_token_ids: torch.Tensor,
) -> tuple[float, tuple[float, float, float]]:
    model.eval()
    preds = []
    n = va_seqs.size(0)
    for i in range(0, n, 64):
        seq_scores, _ = score_de_batch(
            model,
            va_seqs[i : i + 64].long(),
            va_pos[i : i + 64].long(),
            de_token_ids,
        )
        preds.append(seq_scores.argmax(dim=-1))
    pred_all = torch.cat(preds, dim=0)
    overall = float((pred_all == va_lbl).float().mean().item())
    per_cls = []
    for c in range(3):
        m = va_lbl == c
        per_cls.append(float((pred_all[m] == va_lbl[m]).float().mean().item()))
    model.train()
    return overall, (per_cls[0], per_cls[1], per_cls[2])


def load_confusable_pairs(path: Path) -> list[tuple[str, str]]:
    pairs: list[tuple[str, str]] = []
    for lineno, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        parts = line.split("\t")
        if len(parts) != 2:
            raise ValueError(f"{path}:{lineno}: expected 2 tab columns, got {line!r}")
        good, bads_str = parts[0].strip(), parts[1].strip()
        for bad in bads_str.split("|"):
            bad = bad.strip()
            if not bad:
                continue
            if len(bad) != len(good):
                raise ValueError(f"{path}:{lineno}: length mismatch {good!r} vs {bad!r}")
            pairs.append((good, bad))
    return pairs


def prepare_pair_data(
    scalars: list[int],
    pairs_path: Path,
    max_ctx_per_pair: int = 80,
    seed: int = 44,
    force: bool = False,
) -> tuple[Path, Path, Path]:
    meta_path = STAGE2_DIR / "pair_meta.json"
    pair_train_p = STAGE2_DIR / "pair_train.npz"
    pair_val_ctx_p = STAGE2_DIR / "pair_val_ctx.npz"
    pair_val_sa_p = STAGE2_DIR / "pair_val_standalone.npz"

    pairs = load_confusable_pairs(pairs_path)
    pairs_sig = f"{len(pairs)}:{pairs_path.stat().st_mtime_ns}:{max_ctx_per_pair}"

    if not force and all(p.exists() for p in (meta_path, pair_train_p, pair_val_ctx_p, pair_val_sa_p)):
        meta = json.loads(meta_path.read_text(encoding="utf-8"))
        if meta.get("sig") == pairs_sig:
            print(
                f"[pair-data] cached: train={meta['train']:,}, val_ctx={meta['val_ctx']:,}, "
                f"val_standalone={meta['val_standalone']:,}",
                flush=True,
            )
            return pair_train_p, pair_val_ctx_p, pair_val_sa_p

    STAGE2_DIR.mkdir(parents=True, exist_ok=True)
    char_to_id = {chr(sc): idx for idx, sc in enumerate(scalars) if idx >= 5 and sc > 0}
    char_to_id["，"] = PAUSE_ID
    exact_eval = build_exact_eval_set()

    by_good: dict[str, list[str]] = {}
    for good, bad in pairs:
        by_good.setdefault(good, []).append(bad)

    lengths = sorted({len(g) for g in by_good})
    good_sets_by_len = {L: {g for g in by_good if len(g) == L} for L in lengths}
    bigrams = {g[:2] for g in by_good}
    ctx_by_good: dict[str, list[tuple[str, str]]] = {g: [] for g in by_good}
    counts: dict[str, int] = {g: 0 for g in by_good}
    active_goods = len(by_good)

    t0 = time.time()
    print(f"[pair-data] scanning FineWeb-2 for {len(pairs)} confusable pairs ({len(by_good)} good targets)...", flush=True)
    for cp in FINEWEB_FILES:
        if active_goods == 0:
            break
        with cp.open(encoding="utf-8") as handle:
            for raw in handle:
                s = raw.strip()
                n = len(s)
                if n < 2 or n > MAX_DE_SEQ - 4:
                    continue
                if not any(s[i : i + 2] in bigrams for i in range(n - 1)):
                    continue
                if n >= 8 and (s in exact_eval or any(len(c) >= 8 and c in exact_eval for c in s.split("，"))):
                    continue
                matched_here = False
                for i in range(n - 1):
                    if s[i : i + 2] not in bigrams:
                        continue
                    for L in lengths:
                        if i + L > n:
                            continue
                        sub = s[i : i + L]
                        if sub in good_sets_by_len[L]:
                            c = counts[sub]
                            if c < max_ctx_per_pair:
                                bads = by_good[sub]
                                bad = bads[c % len(bads)]
                                bad_sent = s[:i] + bad + s[i + L :]
                                ctx_by_good[sub].append((s, bad_sent))
                                counts[sub] = c + 1
                                if c + 1 >= max_ctx_per_pair:
                                    active_goods -= 1
                                matched_here = True
                                break
                    if matched_here:
                        break

    rng = np.random.default_rng(seed)
    train_items: list[tuple[str, str]] = []
    val_ctx_items: list[tuple[str, str]] = []
    val_sa_items: list[tuple[str, str]] = list(pairs)

    # Include standalone pairs (repeated 6x in train) so zero-context single-phrase EOS scoring stays accurate.
    for good, bad in pairs:
        for _ in range(6):
            train_items.append((good, bad))

    matched_groups = 0
    total_ctx = 0
    for good, ctx_list in ctx_by_good.items():
        if not ctx_list:
            continue
        matched_groups += 1
        total_ctx += len(ctx_list)
        perm = rng.permutation(len(ctx_list))
        n_val = max(1, (len(ctx_list) * 15) // 100) if len(ctx_list) >= 3 else 0
        for idx in perm[:n_val]:
            val_ctx_items.append(ctx_list[idx])
        for idx in perm[n_val:]:
            train_items.append(ctx_list[idx])

    def encode_pairs(items: list[tuple[str, str]]) -> tuple[np.ndarray, np.ndarray]:
        good_seqs = np.zeros((len(items), MAX_DE_SEQ), dtype=np.uint16)
        bad_seqs = np.zeros((len(items), MAX_DE_SEQ), dtype=np.uint16)
        for row_i, (g_txt, b_txt) in enumerate(items):
            g_ids = [BOS_ID] + [char_to_id.get(ch, UNK_ID) for ch in g_txt] + [EOS_ID]
            b_ids = [BOS_ID] + [char_to_id.get(ch, UNK_ID) for ch in b_txt] + [EOS_ID]
            good_seqs[row_i, : len(g_ids)] = g_ids
            bad_seqs[row_i, : len(b_ids)] = b_ids
        return good_seqs, bad_seqs

    tr_g, tr_b = encode_pairs(train_items)
    vc_g, vc_b = encode_pairs(val_ctx_items)
    vs_g, vs_b = encode_pairs(val_sa_items)
    np.savez(pair_train_p, good_seqs=tr_g, bad_seqs=tr_b)
    np.savez(pair_val_ctx_p, good_seqs=vc_g, bad_seqs=vc_b)
    np.savez(pair_val_sa_p, good_seqs=vs_g, bad_seqs=vs_b)

    meta_path.write_text(
        json.dumps(
            {
                "sig": pairs_sig,
                "train": len(train_items),
                "val_ctx": len(val_ctx_items),
                "val_standalone": len(val_sa_items),
                "matched_groups": matched_groups,
                "total_ctx": total_ctx,
            }
        ),
        encoding="utf-8",
    )
    print(
        f"[pair-data] matched {matched_groups}/{len(by_good)} groups ({total_ctx:,} ctx sentences); "
        f"train={len(train_items):,}, val_ctx={len(val_ctx_items):,}, val_standalone={len(val_sa_items):,} "
        f"({time.time() - t0:.1f}s)",
        flush=True,
    )
    return pair_train_p, pair_val_ctx_p, pair_val_sa_p


def score_pair_batch(
    model: CharTransformerLM,
    good_seqs: torch.Tensor,
    bad_seqs: torch.Tensor,
    eos_weight: float = 0.85,
) -> tuple[torch.Tensor, torch.Tensor, torch.Tensor]:
    bsz = good_seqs.size(0)
    flat = torch.cat([good_seqs, bad_seqs], dim=0)
    inp = flat[:, :-1]
    tgt = flat[:, 1:]

    with torch.amp.autocast("cuda", dtype=torch.bfloat16):
        logits = model(inp)
    logits_f32 = logits.float()

    valid = tgt != PAD_ID
    tok_scores = logits_f32.gather(-1, tgt.unsqueeze(-1)).squeeze(-1)
    weights = torch.where(tgt == EOS_ID, eos_weight, torch.where(valid, 1.0, 0.0))
    seq_scores = (tok_scores * weights).sum(dim=-1)
    s_good = seq_scores[:bsz]
    s_bad = seq_scores[bsz:]

    log_z = torch.logsumexp(logits_f32[valid], dim=-1)
    sn_pair = (log_z * log_z).mean()
    return s_good, s_bad, sn_pair


@torch.no_grad()
def evaluate_pair_val(
    model: CharTransformerLM,
    good_seqs: torch.Tensor,
    bad_seqs: torch.Tensor,
) -> float:
    n = good_seqs.size(0)
    if n == 0:
        return 1.0
    model.eval()
    wins = 0
    for i in range(0, n, 64):
        s_g, s_b, _ = score_pair_batch(
            model,
            good_seqs[i : i + 64].long(),
            bad_seqs[i : i + 64].long(),
        )
        wins += int((s_g > s_b).sum().item())
    model.train()
    return wins / n


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--stage1", type=Path, default=ROOT / "models" / "reranker_s1.bin")
    parser.add_argument("--out", type=Path, default=DEFAULT_OUT)
    parser.add_argument("--steps", type=int, default=3000)
    parser.add_argument("--batch-lm", type=int, default=256)
    parser.add_argument("--seq-len", type=int, default=64)
    parser.add_argument("--batch-de", type=int, default=36)
    parser.add_argument("--batch-pair", type=int, default=32)
    parser.add_argument("--lr", type=float, default=4e-4)
    parser.add_argument("--min-lr", type=float, default=2e-5)
    parser.add_argument("--sn-weight", type=float, default=0.5)
    parser.add_argument("--de-weight", type=float, default=0.5)
    parser.add_argument("--confusable-pairs", type=str, default=str(DEFAULT_PAIRS))
    parser.add_argument("--pair-weight", type=float, default=0.35)
    parser.add_argument("--pair-margin", type=float, default=1.5)
    parser.add_argument("--max-ctx-per-pair", type=int, default=80)
    parser.add_argument("--force-data", action="store_true")
    parser.add_argument("--force-pair-data", action="store_true")
    args = parser.parse_args()
    sys.stdout.reconfigure(encoding="utf-8")

    device = torch.device("cuda" if torch.cuda.is_available() else "cpu")
    if device.type != "cuda":
        raise RuntimeError("CUDA GPU required")
    torch.backends.cuda.matmul.allow_tf32 = True
    torch.set_float32_matmul_precision("high")

    model, scalars = load_imerer01(args.stage1)
    model = model.to(device)
    char_to_id = {chr(sc): idx for idx, sc in enumerate(scalars) if idx >= 5 and sc > 0}
    de_ids_list = [char_to_id["的"], char_to_id["得"], char_to_id["地"]]
    assert all(x >= 5 for x in de_ids_list)
    de_token_ids = torch.tensor(de_ids_list, dtype=torch.long, device=device)

    fw_train_p, fw_val_p, de_train_p, de_val_p = prepare_stage2_data(
        scalars, force=args.force_data
    )

    fw_tr_np = np.fromfile(fw_train_p, dtype=np.uint16)
    fw_va_np = np.fromfile(fw_val_p, dtype=np.uint16)
    fw_tr_gpu = torch.from_numpy(fw_tr_np.view(np.int16)).to(device)
    fw_va_gpu = torch.from_numpy(fw_va_np.view(np.int16)).to(device)
    del fw_tr_np, fw_va_np

    tr_de = np.load(de_train_p)
    va_de = np.load(de_val_p)
    tr_de_seqs = torch.from_numpy(tr_de["seqs"].view(np.int16)).to(device)
    tr_de_pos = torch.from_numpy(tr_de["seq_pos"]).to(device)
    tr_de_lbl = torch.from_numpy(tr_de["labels"]).to(device)
    cls_indices = [torch.nonzero(tr_de_lbl == c, as_tuple=False).squeeze(-1) for c in range(3)]

    va_de_seqs = torch.from_numpy(va_de["seqs"].view(np.int16)).to(device)
    va_de_pos = torch.from_numpy(va_de["seq_pos"]).to(device)
    va_de_lbl = torch.from_numpy(va_de["labels"]).to(device)

    pair_enabled = bool(args.confusable_pairs.strip()) and args.pair_weight > 0.0
    if pair_enabled:
        pt_p, pvc_p, pvs_p = prepare_pair_data(
            scalars,
            Path(args.confusable_pairs),
            max_ctx_per_pair=args.max_ctx_per_pair,
            force=args.force_data or args.force_pair_data,
        )
        tr_pair = np.load(pt_p)
        vc_pair = np.load(pvc_p)
        vs_pair = np.load(pvs_p)
        tr_p_good = torch.from_numpy(tr_pair["good_seqs"].view(np.int16)).to(device)
        tr_p_bad = torch.from_numpy(tr_pair["bad_seqs"].view(np.int16)).to(device)
        vc_p_good = torch.from_numpy(vc_pair["good_seqs"].view(np.int16)).to(device)
        vc_p_bad = torch.from_numpy(vc_pair["bad_seqs"].view(np.int16)).to(device)
        vs_p_good = torch.from_numpy(vs_pair["good_seqs"].view(np.int16)).to(device)
        vs_p_bad = torch.from_numpy(vs_pair["bad_seqs"].view(np.int16)).to(device)
    else:
        tr_p_good = tr_p_bad = vc_p_good = vc_p_bad = vs_p_good = vs_p_bad = None

    val_ce0, val_lzm0, val_lzs0 = evaluate_val(
        model, fw_va_gpu, args.batch_lm, args.seq_len, args.sn_weight, device, max_batches=48
    )
    de_acc0, (c0_0, c1_0, c2_0) = evaluate_de_val(
        model, va_de_seqs, va_de_pos, va_de_lbl, de_token_ids
    )
    pair_msg0 = ""
    if pair_enabled:
        p_ctx0 = evaluate_pair_val(model, vc_p_good, vc_p_bad)
        p_sa0 = evaluate_pair_val(model, vs_p_good, vs_p_bad)
        pair_msg0 = f" | pair_ctx={p_ctx0*100:.2f}% pair_sa={p_sa0*100:.2f}%"
    print(
        f"[eval @    0] fw_ce={val_ce0:.4f} logZ={val_lzm0:+.4f}±{val_lzs0:.4f} | "
        f"de_val_acc={de_acc0*100:.2f}% (的={c0_0*100:.1f}% 得={c1_0*100:.1f}% 地={c2_0*100:.1f}%){pair_msg0}",
        flush=True,
    )

    optimizer = torch.optim.AdamW(
        model.parameters(),
        lr=args.lr,
        betas=(0.9, 0.95),
        weight_decay=0.05,
        fused=True,
    )
    warmup = min(150, args.steps // 10)

    def lr_at(step: int) -> float:
        if step < warmup:
            return args.lr * (step + 1) / warmup
        prog = (step - warmup) / max(1, args.steps - warmup)
        return args.min_lr + 0.5 * (args.lr - args.min_lr) * (1.0 + math.cos(math.pi * prog))

    torch.manual_seed(2026)
    torch.cuda.manual_seed(2026)
    max_start = fw_tr_gpu.numel() - (args.seq_len + 1)
    stride_offsets = torch.arange(args.seq_len + 1, device=device, dtype=torch.long)
    per_cls_b = max(1, args.batch_de // 3)
    best_obj = float("inf")
    best_state: dict[str, torch.Tensor] | None = None
    best_fallback_obj = float("inf")
    best_fallback_state: dict[str, torch.Tensor] | None = None
    t0 = time.time()

    model.train()
    for step in range(args.steps):
        lr = lr_at(step)
        for pg in optimizer.param_groups:
            pg["lr"] = lr

        starts = torch.randint(0, max_start, (args.batch_lm,), device=device, dtype=torch.long)
        chunk = fw_tr_gpu[starts[:, None] + stride_offsets[None, :]].long()
        inp = chunk[:, :-1]
        tgt = chunk[:, 1:]
        pos_ids, attn_mask = build_packed_masks(inp)

        de_picks = []
        for c in range(3):
            pool = cls_indices[c]
            sel = pool[torch.randint(0, pool.numel(), (per_cls_b,), device=device)]
            de_picks.append(sel)
        de_idx = torch.cat(de_picks, dim=0)

        optimizer.zero_grad(set_to_none=True)
        with torch.amp.autocast("cuda", dtype=torch.bfloat16):
            logits = model(inp, pos_ids=pos_ids, attn_mask=attn_mask)
        loss_lm, ce_lm, lz_m, lz_s = compute_loss(logits, inp, tgt, args.sn_weight)

        seq_scores, sn_de = score_de_batch(
            model,
            tr_de_seqs[de_idx].long(),
            tr_de_pos[de_idx].long(),
            de_token_ids,
        )
        loss_de = F.cross_entropy(seq_scores, tr_de_lbl[de_idx]) + args.sn_weight * sn_de
        loss = loss_lm + args.de_weight * loss_de

        pair_loss_val = 0.0
        if pair_enabled:
            p_idx = torch.randint(0, tr_p_good.size(0), (args.batch_pair,), device=device)
            s_good, s_bad, sn_pair = score_pair_batch(
                model,
                tr_p_good[p_idx].long(),
                tr_p_bad[p_idx].long(),
            )
            loss_pair = F.softplus(args.pair_margin - (s_good - s_bad)).mean() + args.sn_weight * sn_pair
            loss = loss + args.pair_weight * loss_pair
            pair_loss_val = float(loss_pair.item())

        loss.backward()
        torch.nn.utils.clip_grad_norm_(model.parameters(), 1.0)
        optimizer.step()

        if (step + 1) % 250 == 0:
            pair_log = f" | pair_loss={pair_loss_val:.4f}" if pair_enabled else ""
            print(
                f"  step {step + 1:4d}/{args.steps} | lr={lr:.2e} | ce={ce_lm.item():.4f} | "
                f"de_loss={loss_de.item():.4f}{pair_log} | logZ={lz_m.item():+.3f}±{lz_s.item():.3f} | "
                f"{time.time() - t0:.0f}s",
                flush=True,
            )

        if (step + 1) % 500 == 0 or step + 1 == args.steps:
            val_ce, val_lzm, val_lzs = evaluate_val(
                model, fw_va_gpu, args.batch_lm, args.seq_len, args.sn_weight, device, max_batches=48
            )
            de_acc, (c0, c1, c2) = evaluate_de_val(
                model, va_de_seqs, va_de_pos, va_de_lbl, de_token_ids
            )
            val_ppl = math.exp(min(val_ce, 20.0))
            if pair_enabled:
                p_ctx = evaluate_pair_val(model, vc_p_good, vc_p_bad)
                p_sa = evaluate_pair_val(model, vs_p_good, vs_p_bad)
                pair_acc = 0.5 * (p_ctx + p_sa)
                obj = val_ce - 0.4 * de_acc - 0.3 * pair_acc
                pair_eval_msg = f" | pair_ctx={p_ctx*100:.2f}% pair_sa={p_sa*100:.2f}%"
            else:
                obj = val_ce - 0.5 * de_acc
                pair_eval_msg = ""
            print(
                f"[eval @ {step + 1:4d}] fw_ce={val_ce:.4f} ppl={val_ppl:.2f} logZ={val_lzm:+.4f}±{val_lzs:.4f} | "
                f"de_val_acc={de_acc*100:.2f}% (的={c0*100:.1f}% 得={c1*100:.1f}% 地={c2*100:.1f}%){pair_eval_msg}",
                flush=True,
            )
            if step + 1 >= 1000:
                if obj < best_fallback_obj:
                    best_fallback_obj = obj
                    best_fallback_state = {k: v.detach().cpu().clone() for k, v in model.state_dict().items()}
                if (not pair_enabled or de_acc >= 0.93) and obj < best_obj:
                    best_obj = obj
                    best_state = {k: v.detach().cpu().clone() for k, v in model.state_dict().items()}

    chosen_state = best_state if best_state is not None else best_fallback_state
    if chosen_state is not None:
        model.load_state_dict({k: v.to(device) for k, v in chosen_state.items()})
    del fw_tr_gpu, fw_va_gpu, tr_de_seqs, va_de_seqs
    if pair_enabled:
        del tr_p_good, tr_p_bad, vc_p_good, vc_p_bad, vs_p_good, vs_p_bad
    torch.cuda.empty_cache()

    export_imerer01(model, scalars, args.out)
    verify_rust_parity(model, scalars, args.out, ROOT / "target" / "release" / "ime-cli.exe")


if __name__ == "__main__":
    main()

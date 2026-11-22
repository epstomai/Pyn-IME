"""Train a Self-Normalized Causal Character Transformer reranker on RTX GPU and export IMERER01."""
from __future__ import annotations

import argparse
import json
import math
import struct
import subprocess
import sys
import time
from collections import Counter
from pathlib import Path

import numpy as np
import torch
import torch.nn as nn
import torch.nn.functional as F

ROOT = Path(__file__).resolve().parents[2]
EVAL_SETS = ROOT / "eval" / "sets"
CORPORA_DIR = ROOT / ".external" / "corpora"
DATA_DIR = CORPORA_DIR / "reranker_data"
DEFAULT_OUT = ROOT / "models" / "reranker.bin"

CORPUS_FILES = [
    CORPORA_DIR / "clean_lccc.txt",
    CORPORA_DIR / "clean_fineweb.txt",
    CORPORA_DIR / "clean_fineweb_train.txt",
]

MAGIC = b"IMERER01"
PAD_ID = 0
BOS_ID = 1
EOS_ID = 2
PAUSE_ID = 3
UNK_ID = 4


def build_exact_eval_set() -> set[str]:
    clauses: set[str] = set()
    for p in sorted(EVAL_SETS.glob("*.jsonl")):
        with p.open(encoding="utf-8") as handle:
            for line in handle:
                row = json.loads(line)
                exp = row.get("expected")
                if exp and len(exp) >= 8:
                    clauses.add(exp)
    return clauses


def prepare_dataset(
    vocab_size: int = 6144,
    val_sentences: int = 40_000,
    num_shards: int = 64,
    seed: int = 42,
    force: bool = False,
) -> tuple[list[int], Path, Path]:
    vocab_path = DATA_DIR / "vocab.json"
    train_path = DATA_DIR / "train_tokens.u16"
    val_path = DATA_DIR / "val_tokens.u16"

    if not force and vocab_path.exists() and train_path.exists() and val_path.exists():
        meta = json.loads(vocab_path.read_text(encoding="utf-8"))
        if len(meta["scalars"]) == vocab_size:
            print(
                f"[data] cached dataset ready: vocab={vocab_size}, "
                f"train_tokens={meta['train_tokens']:,}, val_tokens={meta['val_tokens']:,}",
                flush=True,
            )
            return meta["scalars"], train_path, val_path

    DATA_DIR.mkdir(parents=True, exist_ok=True)
    t0 = time.time()
    print("[data] counting character frequencies across cleaned corpora...", flush=True)
    char_freq: Counter[str] = Counter()
    total_lines = 0
    for cp in CORPUS_FILES:
        with cp.open(encoding="utf-8") as handle:
            for line in handle:
                s = line.strip()
                if not s:
                    continue
                total_lines += 1
                char_freq.update(s)
    char_freq.pop("，", None)

    keep_chars = [ch for ch, _ in char_freq.most_common(vocab_size - 5)]
    scalars = [0, 1, 2, 3, 4] + [ord(ch) for ch in keep_chars]
    while len(scalars) < vocab_size:
        scalars.append(0)
    char_to_id = {ch: idx + 5 for idx, ch in enumerate(keep_chars)}
    char_to_id["，"] = PAUSE_ID

    total_hanzi = sum(char_freq.values())
    covered_hanzi = sum(char_freq[ch] for ch in keep_chars)
    print(
        f"[data] lines={total_lines:,}, unique_hanzi={len(char_freq):,}, "
        f"vocab={len(scalars)} coverage={covered_hanzi / max(total_hanzi, 1) * 100:.4f}% ({time.time() - t0:.1f}s)",
        flush=True,
    )

    exact_eval = build_exact_eval_set()
    t1 = time.time()
    print("[data] encoding & sharded shuffling...", flush=True)
    shard_paths = [DATA_DIR / f"shard_{i:02d}.bin" for i in range(num_shards)]
    shard_files = [p.open("wb") for p in shard_paths]
    shard_counts = [0] * num_shards

    rng = np.random.default_rng(seed)
    leaked_extra = 0
    encoded_lines = 0

    try:
        trans_miss = UNK_ID
        for cp in CORPUS_FILES:
            with cp.open(encoding="utf-8") as handle:
                batch_lines: list[str] = []
                for line in handle:
                    s = line.strip()
                    if not s:
                        continue
                    if len(s) >= 8 and (s in exact_eval or any(len(c) >= 8 and c in exact_eval for c in s.split("，"))):
                        leaked_extra += 1
                        continue
                    batch_lines.append(s)
                    if len(batch_lines) >= 65536:
                        targets = rng.integers(0, num_shards, size=len(batch_lines))
                        for ln, sh_idx in zip(batch_lines, targets):
                            ids = [BOS_ID] + [char_to_id.get(ch, trans_miss) for ch in ln] + [EOS_ID]
                            arr = np.asarray(ids, dtype=np.uint16)
                            shard_files[sh_idx].write(struct.pack("<H", len(arr)))
                            shard_files[sh_idx].write(arr.tobytes())
                            shard_counts[sh_idx] += 1
                            encoded_lines += 1
                        batch_lines.clear()
                if batch_lines:
                    targets = rng.integers(0, num_shards, size=len(batch_lines))
                    for ln, sh_idx in zip(batch_lines, targets):
                        ids = [BOS_ID] + [char_to_id.get(ch, trans_miss) for ch in ln] + [EOS_ID]
                        arr = np.asarray(ids, dtype=np.uint16)
                        shard_files[sh_idx].write(struct.pack("<H", len(arr)))
                        shard_files[sh_idx].write(arr.tobytes())
                        shard_counts[sh_idx] += 1
                        encoded_lines += 1
    finally:
        for f in shard_files:
            f.close()

    print(
        f"[data] sharded {encoded_lines:,} lines into {num_shards} shards (extra_leaked={leaked_extra}) in {time.time() - t1:.1f}s",
        flush=True,
    )

    t2 = time.time()
    val_per_shard = max(1, val_sentences // num_shards)
    train_tokens = 0
    val_tokens = 0
    shard_order = rng.permutation(num_shards)

    with train_path.open("wb") as f_train, val_path.open("wb") as f_val:
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
                f_val.write(b)
                val_tokens += len(b) // 2
            for idx in perm[split:]:
                b = recs[idx]
                f_train.write(b)
                train_tokens += len(b) // 2

    vocab_path.write_text(
        json.dumps(
            {
                "scalars": scalars,
                "train_tokens": train_tokens,
                "val_tokens": val_tokens,
            }
        ),
        encoding="utf-8",
    )
    print(
        f"[data] wrote train_tokens={train_tokens:,} val_tokens={val_tokens:,} ({time.time() - t2:.1f}s)",
        flush=True,
    )
    return scalars, train_path, val_path


class RMSNorm(nn.Module):
    def __init__(self, dim: int, eps: float = 1e-6) -> None:
        super().__init__()
        self.weight = nn.Parameter(torch.ones(dim))
        self.eps = eps

    def forward(self, x: torch.Tensor) -> torch.Tensor:
        x_f = x.float()
        normed = x_f * torch.rsqrt(x_f.pow(2).mean(dim=-1, keepdim=True) + self.eps)
        return (normed * self.weight.float()).to(x.dtype)


class TransformerBlock(nn.Module):
    def __init__(self, d_model: int, n_heads: int, d_ff: int) -> None:
        super().__init__()
        self.d_model = d_model
        self.n_heads = n_heads
        self.head_dim = d_model // n_heads
        self.d_ff = d_ff

        self.norm1 = RMSNorm(d_model)
        self.w_qkv = nn.Linear(d_model, 3 * d_model, bias=False)
        self.w_o = nn.Linear(d_model, d_model, bias=False)
        self.norm2 = RMSNorm(d_model)
        self.w_gate_up = nn.Linear(d_model, 2 * d_ff, bias=False)
        self.w_down = nn.Linear(d_ff, d_model, bias=False)

    def forward(
        self,
        x: torch.Tensor,
        cos: torch.Tensor,
        sin: torch.Tensor,
        attn_mask: torch.Tensor,
    ) -> torch.Tensor:
        bsz, seq_len, _ = x.shape
        h = self.norm1(x)
        qkv = self.w_qkv(h)
        q, k, v = qkv.split(self.d_model, dim=-1)

        q = q.view(bsz, seq_len, self.n_heads, self.head_dim).transpose(1, 2)
        k = k.view(bsz, seq_len, self.n_heads, self.head_dim).transpose(1, 2)
        v = v.view(bsz, seq_len, self.n_heads, self.head_dim).transpose(1, 2)

        half = self.head_dim // 2
        q1, q2 = q[..., :half], q[..., half:]
        k1, k2 = k[..., :half], k[..., half:]
        q = torch.cat([q1 * cos - q2 * sin, q1 * sin + q2 * cos], dim=-1)
        k = torch.cat([k1 * cos - k2 * sin, k1 * sin + k2 * cos], dim=-1)

        attn = F.scaled_dot_product_attention(q, k, v, attn_mask=attn_mask)
        attn = attn.transpose(1, 2).contiguous().view(bsz, seq_len, self.d_model)
        x = x + self.w_o(attn)

        ff_h = self.w_gate_up(self.norm2(x))
        gate, up = ff_h.split(self.d_ff, dim=-1)
        x = x + self.w_down(F.silu(gate) * up)
        return x


class CharTransformerLM(nn.Module):
    def __init__(
        self,
        vocab_size: int = 6144,
        d_model: int = 192,
        n_layers: int = 6,
        n_heads: int = 3,
        d_ff: int = 384,
        max_seq_len: int = 128,
        rope_theta: float = 10000.0,
    ) -> None:
        super().__init__()
        self.vocab_size = vocab_size
        self.d_model = d_model
        self.n_layers = n_layers
        self.n_heads = n_heads
        self.head_dim = d_model // n_heads
        self.d_ff = d_ff
        self.max_seq_len = max_seq_len
        self.rope_theta = rope_theta

        self.tok_emb = nn.Embedding(vocab_size, d_model)
        self.lm_head_bias = nn.Parameter(torch.full((vocab_size,), -math.log(vocab_size)))
        self.layers = nn.ModuleList(
            [TransformerBlock(d_model, n_heads, d_ff) for _ in range(n_layers)]
        )
        self.norm_f = RMSNorm(d_model)

        half = self.head_dim // 2
        inv_freq = 1.0 / (rope_theta ** (torch.arange(0, half, dtype=torch.float32) * 2.0 / self.head_dim))
        pos = torch.arange(max_seq_len, dtype=torch.float32)
        angles = torch.outer(pos, inv_freq)
        self.register_buffer("rope_cos", angles.cos()[None, None, :, :], persistent=False)
        self.register_buffer("rope_sin", angles.sin()[None, None, :, :], persistent=False)

        self.apply(self._init_weights)
        res_scale = 0.02 / math.sqrt(2 * n_layers)
        for layer in self.layers:
            nn.init.normal_(layer.w_o.weight, mean=0.0, std=res_scale)
            nn.init.normal_(layer.w_down.weight, mean=0.0, std=res_scale)

    def _init_weights(self, mod: nn.Module) -> None:
        if isinstance(mod, nn.Linear):
            nn.init.normal_(mod.weight, mean=0.0, std=0.02)
        elif isinstance(mod, nn.Embedding):
            nn.init.normal_(mod.weight, mean=0.0, std=0.02)

    def forward(
        self,
        input_ids: torch.Tensor,
        pos_ids: torch.Tensor | None = None,
        attn_mask: torch.Tensor | None = None,
    ) -> torch.Tensor:
        bsz, seq_len = input_ids.shape
        x = self.tok_emb(input_ids)
        if pos_ids is None:
            cos = self.rope_cos[:, :, :seq_len, :].to(x.dtype)
            sin = self.rope_sin[:, :, :seq_len, :].to(x.dtype)
        else:
            # pos_ids: [B, T] -> cos/sin [B, 1, T, half]
            cos = self.rope_cos[0, 0][pos_ids].unsqueeze(1).to(x.dtype)
            sin = self.rope_sin[0, 0][pos_ids].unsqueeze(1).to(x.dtype)

        if attn_mask is None:
            attn_mask = torch.tril(torch.ones((seq_len, seq_len), dtype=torch.bool, device=x.device))

        for layer in self.layers:
            x = layer(x, cos, sin, attn_mask)
        x = self.norm_f(x)
        return F.linear(x, self.tok_emb.weight, self.lm_head_bias)


def build_packed_masks(inp: torch.Tensor) -> tuple[torch.Tensor, torch.Tensor]:
    """Compute within-sentence positions and block-diagonal causal mask for packed batches."""
    bsz, seq_len = inp.shape
    is_bos = inp == BOS_ID
    doc_id = torch.cumsum(is_bos.to(torch.int32), dim=1)

    causal = torch.tril(torch.ones((seq_len, seq_len), dtype=torch.bool, device=inp.device))
    same_doc = doc_id.unsqueeze(2) == doc_id.unsqueeze(1)
    attn_mask = (same_doc & causal.unsqueeze(0)).unsqueeze(1)

    # Position within each sentence (matches inference depth starting at BOS = 0)
    idx = torch.arange(seq_len, device=inp.device, dtype=torch.int32).unsqueeze(0).expand(bsz, -1)
    bos_pos = torch.where(is_bos, idx, torch.zeros_like(idx))
    sent_start = torch.cummax(bos_pos, dim=1).values
    pos_ids = (idx - sent_start).clamp(max=seq_len - 1)
    return pos_ids, attn_mask


def compute_loss(
    logits: torch.Tensor,
    inp: torch.Tensor,
    targets: torch.Tensor,
    sn_weight: float,
) -> tuple[torch.Tensor, torch.Tensor, torch.Tensor, torch.Tensor]:
    valid = (inp != EOS_ID) & (inp != PAD_ID)
    valid_logits = logits[valid].float()
    valid_targets = targets[valid]
    ce = F.cross_entropy(valid_logits, valid_targets)
    log_z = torch.logsumexp(valid_logits, dim=-1)
    sn = (log_z * log_z).mean()
    loss = ce + sn_weight * sn
    return loss, ce, log_z.mean(), log_z.std()


def export_imerer01(
    model: CharTransformerLM,
    scalars: list[int],
    out_path: Path,
    unk_penalty: float = 5.0,
) -> None:
    out_path.parent.mkdir(parents=True, exist_ok=True)
    tmp_path = out_path.with_name(out_path.name + ".tmp")

    def write_f16(f, t: torch.Tensor) -> None:
        arr = t.detach().cpu().to(torch.float16).contiguous().numpy()
        f.write(arr.tobytes())

    def write_f32(f, t: torch.Tensor) -> None:
        arr = t.detach().cpu().to(torch.float32).contiguous().numpy()
        f.write(arr.tobytes())

    with tmp_path.open("wb") as f:
        f.write(MAGIC)
        f.write(
            struct.pack(
                "<8I",
                model.vocab_size,
                model.d_model,
                model.n_layers,
                model.n_heads,
                model.d_ff,
                model.max_seq_len,
                struct.unpack("<I", struct.pack("<f", model.rope_theta))[0],
                struct.unpack("<I", struct.pack("<f", unk_penalty))[0],
            )
        )
        f.write(np.asarray(scalars, dtype=np.uint32).tobytes())
        write_f16(f, model.tok_emb.weight)
        write_f32(f, model.lm_head_bias)
        for layer in model.layers:
            write_f32(f, layer.norm1.weight)
            write_f16(f, layer.w_qkv.weight)
            write_f16(f, layer.w_o.weight)
            write_f32(f, layer.norm2.weight)
            write_f16(f, layer.w_gate_up.weight)
            write_f16(f, layer.w_down.weight)
        write_f32(f, model.norm_f.weight)
    tmp_path.replace(out_path)
    print(f"[export] wrote {out_path} ({out_path.stat().st_size / 1e6:.2f} MB)", flush=True)


@torch.no_grad()
def evaluate_val(
    model: CharTransformerLM,
    val_gpu: torch.Tensor,
    batch_size: int,
    seq_len: int,
    sn_weight: float,
    device: torch.device,
    max_batches: int = 48,
) -> tuple[float, float, float]:
    model.eval()
    stride_offsets = torch.arange(seq_len + 1, device=device, dtype=torch.long)
    n_chunks = (val_gpu.numel() - 1) // seq_len
    n_batches = min(max_batches, max(1, n_chunks // batch_size))

    ce_sum = 0.0
    lz_mean_sum = 0.0
    lz_std_sum = 0.0
    for bi in range(n_batches):
        starts = (bi * batch_size + torch.arange(batch_size, device=device, dtype=torch.long)) * seq_len
        chunk_t = val_gpu[starts[:, None] + stride_offsets[None, :]].long()
        inp = chunk_t[:, :-1]
        tgt = chunk_t[:, 1:]
        pos_ids, attn_mask = build_packed_masks(inp)
        with torch.amp.autocast("cuda", dtype=torch.bfloat16):
            logits = model(inp, pos_ids=pos_ids, attn_mask=attn_mask)
        _, ce, lz_m, lz_s = compute_loss(logits, inp, tgt, sn_weight)
        ce_sum += float(ce.item())
        lz_mean_sum += float(lz_m.item())
        lz_std_sum += float(lz_s.item())
    model.train()
    return ce_sum / n_batches, lz_mean_sum / n_batches, lz_std_sum / n_batches


@torch.no_grad()
def verify_rust_parity(
    model: CharTransformerLM,
    scalars: list[int],
    reranker_path: Path,
    cli_exe: Path,
) -> None:
    if not cli_exe.exists():
        print(f"[parity] skipped: {cli_exe} not found", flush=True)
        return
    char_to_id = {chr(sc): idx for idx, sc in enumerate(scalars) if idx >= 5 and sc > 0}
    model_f32 = model.cpu().float().eval()

    test_cases = [
        ("", ["给我一杯水吧", "给我一杯睡吧", "今天天气不错"]),
        ("今天天气，", ["真好啊", "真好吗", "我喜欢骑马"]),
        ("你来得", ["正好", "很晚"]),
        ("他轻轻地", ["关上门", "闭上眼"]),
        ("你好", ["吗", "美"]),
    ]

    max_diff = 0.0
    for ctx, cands in test_cases:
        py_scores: list[float] = []
        ctx_toks = [BOS_ID]
        for ch in ctx:
            if ch == "，":
                ctx_toks.append(PAUSE_ID)
            elif "\u4e00" <= ch <= "\u9fff":
                ctx_toks.append(char_to_id.get(ch, UNK_ID))
        for cand in cands:
            c_toks = [PAUSE_ID if ch == "，" else char_to_id.get(ch, UNK_ID) for ch in cand]
            seq = ctx_toks + c_toks
            inp = torch.tensor([seq], dtype=torch.long)
            logits = model_f32(inp)[0]  # [T, V]
            start_idx = len(ctx_toks) - 1
            total = 0.0
            for j, tok in enumerate(c_toks):
                row_logits = logits[start_idx + j]
                s = float(row_logits[tok].item())
                if tok == UNK_ID:
                    s -= 5.0
                total += s
            eos_s = float(logits[len(seq) - 1, EOS_ID].item())
            total += 0.85 * eos_s
            py_scores.append(total)

        proc = subprocess.run(
            [
                str(cli_exe.resolve()),
                "--score-candidates",
                "--reranker",
                str(reranker_path.resolve()),
                "--ctx",
                ctx,
                "--neural-eos-weight",
                "0.85",
            ],
            input="\n".join(cands) + "\n",
            capture_output=True,
            text=True,
            encoding="utf-8",
            check=True,
        )
        rust_scores = json.loads(proc.stdout)["scores"]
        for cand, p_s, r_s in zip(cands, py_scores, rust_scores):
            diff = abs(p_s - r_s)
            max_diff = max(max_diff, diff)
            print(f"  [parity] ctx={ctx!r} cand={cand!r} py={p_s:.4f} rust={r_s:.4f} diff={diff:.5f}")

    print(f"[parity] max_abs_diff = {max_diff:.5f}", flush=True)
    if max_diff >= 0.05:
        raise AssertionError(f"Python vs Rust parity failed: max_diff={max_diff:.5f} >= 0.05")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--out", type=Path, default=DEFAULT_OUT)
    parser.add_argument("--vocab-size", type=int, default=6144)
    parser.add_argument("--d-model", type=int, default=192)
    parser.add_argument("--n-layers", type=int, default=6)
    parser.add_argument("--n-heads", type=int, default=3)
    parser.add_argument("--d-ff", type=int, default=384)
    parser.add_argument("--max-seq-len", type=int, default=128)
    parser.add_argument("--seq-len", type=int, default=64)
    parser.add_argument("--batch-size", type=int, default=256)
    parser.add_argument("--epochs", type=float, default=1.5)
    parser.add_argument("--max-steps", type=int, default=0)
    parser.add_argument("--lr", type=float, default=2.5e-3)
    parser.add_argument("--min-lr", type=float, default=1e-4)
    parser.add_argument("--sn-weight", type=float, default=0.5)
    parser.add_argument("--force-data", action="store_true")
    parser.add_argument("--verify-only", action="store_true")
    args = parser.parse_args()
    sys.stdout.reconfigure(encoding="utf-8")

    scalars, train_path, val_path = prepare_dataset(
        vocab_size=args.vocab_size,
        force=args.force_data,
    )

    device = torch.device("cuda" if torch.cuda.is_available() else "cpu")
    if device.type != "cuda":
        raise RuntimeError("CUDA GPU required for training")
    torch.backends.cuda.matmul.allow_tf32 = True
    torch.set_float32_matmul_precision("high")

    model = CharTransformerLM(
        vocab_size=args.vocab_size,
        d_model=args.d_model,
        n_layers=args.n_layers,
        n_heads=args.n_heads,
        d_ff=args.d_ff,
        max_seq_len=args.max_seq_len,
    ).to(device)
    n_params = sum(p.numel() for p in model.parameters())
    print(f"[model] {n_params:,} params on {torch.cuda.get_device_name(0)}", flush=True)

    train_np = np.fromfile(train_path, dtype=np.uint16)
    val_np = np.fromfile(val_path, dtype=np.uint16)
    assert int(train_np.max()) < 32768 and int(val_np.max()) < 32768
    train_gpu = torch.from_numpy(train_np.view(np.int16)).to(device)
    val_gpu = torch.from_numpy(val_np.view(np.int16)).to(device)
    del train_np, val_np

    tokens_per_step = args.batch_size * args.seq_len
    total_steps = (
        args.max_steps
        if args.max_steps > 0
        else max(1, int(train_gpu.numel() * args.epochs // tokens_per_step))
    )
    warmup_steps = min(400, max(10, total_steps // 20))
    print(
        f"[train] train_tokens={train_gpu.numel():,}, val_tokens={val_gpu.numel():,}, "
        f"seq_len={args.seq_len}, batch={args.batch_size}, sn_weight={args.sn_weight}, "
        f"tokens/step={tokens_per_step:,}, total_steps={total_steps:,}",
        flush=True,
    )

    optimizer = torch.optim.AdamW(
        model.parameters(),
        lr=args.lr,
        betas=(0.9, 0.95),
        weight_decay=0.05,
        fused=True,
    )

    def lr_at(step: int) -> float:
        if step < warmup_steps:
            return args.lr * (step + 1) / warmup_steps
        prog = (step - warmup_steps) / max(1, total_steps - warmup_steps)
        return args.min_lr + 0.5 * (args.lr - args.min_lr) * (1.0 + math.cos(math.pi * prog))

    torch.manual_seed(1234)
    torch.cuda.manual_seed(1234)
    max_start = train_gpu.numel() - (args.seq_len + 1)
    stride_offsets = torch.arange(args.seq_len + 1, device=device, dtype=torch.long)
    best_val_ce = float("inf")
    best_state: dict[str, torch.Tensor] | None = None
    t_start = time.time()
    t_log = time.time()
    ema_ce = 0.0

    model.train()
    for step in range(total_steps):
        lr = lr_at(step)
        for pg in optimizer.param_groups:
            pg["lr"] = lr

        starts = torch.randint(0, max_start, (args.batch_size,), device=device, dtype=torch.long)
        chunk = train_gpu[starts[:, None] + stride_offsets[None, :]].long()
        inp = chunk[:, :-1]
        tgt = chunk[:, 1:]
        pos_ids, attn_mask = build_packed_masks(inp)

        optimizer.zero_grad(set_to_none=True)
        with torch.amp.autocast("cuda", dtype=torch.bfloat16):
            logits = model(inp, pos_ids=pos_ids, attn_mask=attn_mask)
        loss, ce, lz_m, lz_s = compute_loss(logits, inp, tgt, args.sn_weight)
        loss.backward()
        torch.nn.utils.clip_grad_norm_(model.parameters(), 1.0)
        optimizer.step()

        ce_v = float(ce.item())
        ema_ce = ce_v if step == 0 else 0.95 * ema_ce + 0.05 * ce_v

        if (step + 1) % 500 == 0 or step == 0:
            torch.cuda.synchronize()
            dt = max(time.time() - t_log, 1e-6)
            n_steps = 1 if step == 0 else 500
            tok_s = (n_steps * tokens_per_step) / dt
            vram_gb = torch.cuda.max_memory_allocated(device) / (1024**3)
            print(
                f"  step {step + 1:5d}/{total_steps} | lr={lr:.2e} | ce={ema_ce:.4f} | "
                f"logZ={lz_m.item():+.3f}±{lz_s.item():.3f} | {tok_s/1e3:.0f}k tok/s | vram={vram_gb:.2f}GB",
                flush=True,
            )
            t_log = time.time()

        if (step + 1) % 2000 == 0 or step + 1 == total_steps:
            val_ce, val_lzm, val_lzs = evaluate_val(
                model, val_gpu, args.batch_size, args.seq_len, args.sn_weight, device
            )
            val_ppl = math.exp(min(val_ce, 20.0))
            print(
                f"[val @ {step + 1}] ce={val_ce:.4f} ppl={val_ppl:.2f} "
                f"logZ={val_lzm:+.4f}±{val_lzs:.4f} (elapsed {time.time() - t_start:.0f}s)",
                flush=True,
            )
            if val_ce < best_val_ce:
                best_val_ce = val_ce
                best_state = {k: v.detach().cpu().clone() for k, v in model.state_dict().items()}

    if best_state is not None:
        model.load_state_dict({k: v.to(device) for k, v in best_state.items()})
    del train_gpu, val_gpu
    torch.cuda.empty_cache()
    export_imerer01(model, scalars, args.out)
    verify_rust_parity(model, scalars, args.out, ROOT / "target" / "release" / "ime-cli.exe")


if __name__ == "__main__":
    main()

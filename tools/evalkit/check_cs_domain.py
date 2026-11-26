"""A/B check for optional --cs-ai domain boost vs default general mode."""
from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

CONFLICT_CASES = [
    ("xiecheng", "携程", "协程"),
    ("fanxing", "反省", "泛型"),
    ("zhanzhen", "战阵", "栈帧"),
    ("suanzi", "蒜子", "算子"),
    ("ciyuan", "次元", "词元"),
    ("xiancun", "现存", "显存"),
    ("tidu", "提督", "梯度"),
    ("zhengliu", "整流", "蒸馏"),
    ("bingfa", "病发", "并发"),
    ("shoulian", "熟练", "收敛"),
    ("bianji", "编辑", "变基"),
    ("tiaobiao", "调表", "跳表"),
]

DOMAIN_COMPOUNDS = [
    ("suanzironghe", "算子融合"),
    ("tuicejiema", "推测解码"),
    ("qianzhuishu", "前缀树"),
    ("zidianshu", "字典树"),
    ("shuangshuzuzidianshu", "双数组字典树"),
    ("midengxing", "幂等性"),
    ("lingkaobei", "零拷贝"),
    ("fenyezhuyili", "分页注意力"),
    ("qiangliantongfenliang", "强连通分量"),
    ("haxibiao", "哈希表"),
    ("hongheishu", "红黑树"),
    ("jieyongjiancha", "借用检查"),
    ("huancunmingzhong", "缓存命中"),
    ("chongpaiqi", "重排器"),
    ("dizhishipei", "低秩适配"),
]

# Everyday general phrases that must NOT be hijacked when --cs-ai is enabled.
SAFE_GENERAL = [
    ("jintianwenti", "今天问题"),
    ("zhegefangfa", "这个方法"),
    ("womengongzuo", "我们工作"),
    ("xuyaoshiyou", "需要石油"),
    ("xuexixiguan", "学习习惯"),
    ("woxiangquchifan", "我想去吃饭"),
    ("zhunbeikaishi", "准备开始"),
    ("shejishijian", "设计时间"),
    ("fuwuyuan", "服务员"),
    ("gongsiwenhua", "公司文化"),
    ("womenguanli", "我们管理"),
    ("tamenneirong", "他们内容"),
    ("mingtianzaishuo", "明天再说"),
    ("zheshiwodeshu", "这是我的书"),
    ("nihaoma", "你好吗"),
    ("xiexiedajia", "谢谢大家"),
    ("gongzuoshunli", "工作顺利"),
    ("diannaosijile", "电脑死机了"),
    ("shoujimeidianle", "手机没电了"),
    ("tiyanfeichanghao", "体验非常好"),
]


def query_cli(cli: Path, model: Path, reranker: Path | None, keys_list: list[str], cs_ai: bool) -> list[list[str]]:
    cmd = [str(cli), "--jsonl", "--model", str(model), "--no-learn"]
    if reranker and reranker.exists():
        cmd.extend(["--reranker", str(reranker)])
    else:
        cmd.append("--no-reranker")
    if cs_ai:
        cmd.append("--cs-ai")
    payload = "\n".join(json.dumps({"op": "convert", "keys": k, "n": 5}) for k in keys_list) + "\n"
    proc = subprocess.run(cmd, input=payload, capture_output=True, text=True, encoding="utf-8", check=True)
    return [json.loads(line)["candidates"] for line in proc.stdout.splitlines() if line.strip()]


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--cli", type=Path, default=ROOT / "target" / "release" / "ime-cli.exe")
    parser.add_argument("--model", type=Path, default=ROOT / "models" / "ime_model.bin")
    parser.add_argument("--reranker", type=Path, default=ROOT / "models" / "reranker.bin")
    args = parser.parse_args()
    sys.stdout.reconfigure(encoding="utf-8")

    all_keys = (
        [k for k, _, _ in CONFLICT_CASES]
        + [k for k, _ in DOMAIN_COMPOUNDS]
        + [k for k, _ in SAFE_GENERAL]
    )
    off_res = query_cli(args.cli, args.model, args.reranker, all_keys, cs_ai=False)
    on_res = query_cli(args.cli, args.model, args.reranker, all_keys, cs_ai=True)

    n_conf = len(CONFLICT_CASES)
    n_comp = len(DOMAIN_COMPOUNDS)

    print("=== Conflict Cases (default vs --cs-ai) ===")
    conf_off_general = 0
    conf_on_cs = 0
    for i, (k, exp_gen, exp_cs) in enumerate(CONFLICT_CASES):
        top_off = off_res[i][0] if off_res[i] else ""
        top_on = on_res[i][0] if on_res[i] else ""
        conf_off_general += int(top_off == exp_gen)
        conf_on_cs += int(top_on == exp_cs)
        print(f"  {k:12s} | off={top_off:6s} (exp {exp_gen}) | on={top_on:6s} (exp {exp_cs})")

    print("\n=== Domain Compounds (--cs-ai) ===")
    comp_off = 0
    comp_on = 0
    for j, (k, exp) in enumerate(DOMAIN_COMPOUNDS):
        idx = n_conf + j
        top_off = off_res[idx][0] if off_res[idx] else ""
        top_on = on_res[idx][0] if on_res[idx] else ""
        comp_off += int(top_off == exp)
        comp_on += int(top_on == exp)
        print(f"  {k:22s} | off={top_off:8s} | on={top_on:8s} (exp {exp})")

    print("\n=== Safe General Phrases (must remain identical) ===")
    safe_same = 0
    for m, (k, exp) in enumerate(SAFE_GENERAL):
        idx = n_conf + n_comp + m
        top_off = off_res[idx][0] if off_res[idx] else ""
        top_on = on_res[idx][0] if on_res[idx] else ""
        same = top_off == top_on
        safe_same += int(same)
        if not same:
            print(f"  [CHANGED] {k}: off={top_off} -> on={top_on} (ref {exp})")

    print(
        f"\nSummary: conflict_default_general={conf_off_general}/{n_conf}, "
        f"conflict_cs_ai={conf_on_cs}/{n_conf}, "
        f"compounds_off={comp_off}/{n_comp}, compounds_on={comp_on}/{n_comp}, "
        f"safe_unchanged={safe_same}/{len(SAFE_GENERAL)}"
    )


if __name__ == "__main__":
    main()

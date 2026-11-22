# 评测协议与评测集格式

评测器（`tools/evalkit/run_eval.py`）用同一套用例驱动任何引擎：Rime 基线、自研引擎，
以及用来自检评测器的两个假引擎。

## 引擎操作

| 操作 | 含义 | 是否算学习 |
|---|---|---|
| `reset` | 忘掉所有学到的东西，清空上文 | — |
| `context` | 光标前的文字现在是这些 | 否 |
| `convert` | 给出原始按键串的候选，最好的在前 | 否 |
| `commit` | 用户为上一次按键串选了这段文字；文字并入上文 | 是 |
| `undo` | 用户删掉了刚上屏的文字 | 是（负反馈） |

上文由若干段组成，每段是 `{"text": "…", "keys": "…" 或 null}`。`keys` 是用户打出这段
汉字时敲的按键；标点和从应用里读到的文字没有 `keys`。

## JSONL 协议

`ime-cli --jsonl` 从标准输入逐行读请求，向标准输出逐行写回复，编码为 UTF-8。

```
→ {"op":"hello"}
← {"ok":true,"name":"ime-core","version":"0.1.0","capabilities":{"context":true,"learn":true,"undo":true}}
→ {"op":"reset"}
← {"ok":true}
→ {"op":"context","segments":[{"text":"我们走","keys":"womenzou"}]}
← {"ok":true,"accepted":true}
→ {"op":"convert","keys":"ba","n":10}
← {"ok":true,"candidates":["吧","把","爸"]}
→ {"op":"commit","text":"吧"}
← {"ok":true,"accepted":true}
→ {"op":"undo"}
← {"ok":true,"accepted":true}
→ {"op":"bye"}
← {"ok":true}
```

出错时回复 `{"ok":false,"error":"…"}`。

## 用例格式

`eval/sets/<套件>.jsonl`，每行一条：

| 字段 | 含义 |
|---|---|
| `id` | 稳定编号，由套件、上文、按键、期望文字的哈希得出 |
| `suite` | 所属套件 |
| `split` | `dev`（可用来调参）或 `heldout`（只用来验收） |
| `gate` | 是否属于零失败门禁 |
| `check` | `text`：按期望文字判定；`reading`：首选必须能按所敲拼音逐字读出 |
| `context` | 上文各段 |
| `keys` | 原始按键串：音节直接相连，ü 写作 `v`，不含分隔符（`abbrev` 套件中为简拼按键串） |
| `syllables` | 音节切分，仅作参考；切分本身属于被测内容 |
| `expected` | 期望文字 |
| `focus` | 门禁字在期望文字里的位置；判定只看这些位置 |
| `tags` / `source` | 标签与来源 |

## 套件

| 套件 | 内容 | 门禁 |
|---|---|---|
| `function_words` | 手写虚词句：了、的、着、过、吗、呢、吧、语气词、么，以及同音节但答案不是虚词的对照句 | 是 |
| `incremental` | 由上一套派生的分段上屏用例：上文 + 尾字或后半句 | 含门禁字的是 |
| `abbrev` | 单键简拼与句尾虚词简拼用例 | 含门禁字的是 |
| `precedence` | 所有单音节与一批易混词；首选必须是所敲拼音的精确读法，不得被模糊音或纠错顶替 | 是 |
| `pollution` | 先按脚本误选若干次，再重跑前三套（`function_words`、`incremental`、`abbrev`）的门禁用例；干净时通过、污染后失败的记为回归，报告同时记录干净态通过数 `clean_passed` | 回归数须为零 |
| `de_grammar` | 「的 / 得 / 地」的语法区分 | 否，只计分 |
| `general` | Tatoeba 日常句子，按标点切成分句，前面的分句作上文 | 否，只计分 |

## 指标

- `exact`：首选与期望文字完全相同的比例。
- `nbest`：期望文字出现在前 10 个候选里的比例。
- `CER`：首选相对期望文字的字错误率（编辑距离 / 期望字数）。
- `focus`：门禁字位置全部正确的比例。
- `gate fail`：门禁用例的失败数，发布前必须为零。

## 命令

```
python tools/evalkit/build_sets.py --tatoeba .external/tatoeba-dl/cmn_sentences_detailed.tsv.bz2
python tools/evalkit/run_eval.py --engine oracle      # 自检：必须满分
python tools/evalkit/run_eval.py --engine echo        # 自检：必须全败
python tools/evalkit/run_eval.py --engine rime-gram   # Rime + 万象语法模型
python tools/evalkit/run_eval.py --engine cmd --cmd "target/release/ime-cli.exe --jsonl"
```

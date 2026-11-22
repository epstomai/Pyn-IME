# 数据来源与许可证

本项目的词库、训练语料与评测语料全部记录如下，所有商用/衍生分发受限或未经核实的语料均不采用。

## 1. 词库（CC BY 4.0 + MIT）

- **万象词库**：[amzxyz/rime-wanxiang](https://github.com/amzxyz/rime-wanxiang)（v18.2.0 Base 版）及 [amzxyz/rime-wanxiang-data](https://github.com/amzxyz/rime-wanxiang-data)，许可证 **CC BY 4.0**（要求署名与标明修改）。
  - 使用文件：`dicts/zi.dict.yaml`、`dicts/duoyin.dict.yaml`、`dicts/jichu.dict.yaml`、`dicts/diming.dict.yaml`、`dicts/mingren.dict.yaml`、`dicts/yiren.dict.yaml`。
- **词表规范化与简体过滤（`jieba` + OpenCC `t2s`）**：[fxsjy/jieba](https://github.com/fxsjy/jieba) 默认词典 `dict.txt`（**MIT License**）配合 OpenCC `t2s` 简体化检验。
  - 简体单字白名单：从 `zi.dict.yaml` 提取全部满足 `t2s(ch) == ch` 的纯汉字单字，共 `17,360` 字，剔除繁简异形干扰字（如 `麽`、`這`、`為`），确保评测集用字 `100%` 覆盖。
  - 多字词过滤：从 `jieba` 提取长度 2–6 字、词频 $\ge 3$ 且 `t2s(word) == word` 的纯汉字词条，并剔除会吞噬句尾共现的粘连词条（以 `吗呢吧啊呀啦嘛哦噢呐哒` 结尾的多字词，以及长度 $\ge 3$ 且以 `了` 结尾的词），共导出 `295,302` 个规范词条；用于过滤万象基础词库与多音词库中的整句/短语碎片（保留全部简体单字、`jieba` 词条及权重 $\ge 1000$ 的地名/名人/艺人专名，第 2 期最终模型词表 `225,404` 词）。
- **读音概率**：对同一词条的不同读音保留万象统计权重，计算拼音发射对数概率 `reading_logp`；标准 415 音节表剔除了独立鼻音 `hm/m/n/ng`。

## 2. 训练语料

清洗与零泄漏过滤脚本见 [prepare_corpus.py](file:///H:/Codex%20Projs/Input_Engine/tools/train/prepare_corpus.py)。第 2 期全部训练分句均经 OpenCC `t2s` 白名单过滤，共 **221,185,736** 汉字（口语与书面语约 `1 : 10`，切分后约 `1.664 亿`词 token）。

### 2.1 LCCC（Large-scale Cleaned Chinese Conversation）
- **来源**：[thu-coai/lccc](https://huggingface.co/datasets/thu-coai/lccc) / [silver/lccc](https://huggingface.co/datasets/silver/lccc)（`lccc_base_train.jsonl.gz`）
- **许可证**：MIT License
- **用途**：提供日常口语对话语境，覆盖句尾语气词（「了、吗、呢、吧、啊」）与口语搭配。
- **清洗结果（`--lccc-max-chars 20000000`）**：输出 `2,187,580` 行、`20,000,001` 个汉字，精确防泄漏拦截 `13` 句。

### 2.2 FineWeb-2 中文测试分片（`cmn_Hani/test`）
- **来源**：[HuggingFaceFW/fineweb-2](https://huggingface.co/datasets/HuggingFaceFW/fineweb-2)（`data/cmn_Hani/test/000_00000.parquet`）
- **许可证**：Open Data Commons Attribution License（ODC-By）v1.0
- **用途**：通用书面语与网页常识文本。
- **清洗结果**：剔除繁体文档、非白名单汉字及非纯汉字分句后，输出 `887,932` 行、`21,185,663` 个汉字，精确防泄漏拦截 `3` 句。

### 2.3 FineWeb-2 中文训练分片（`cmn_Hani/train` 流式拉取）
- **来源**：[HuggingFaceFW/fineweb-2](https://huggingface.co/datasets/HuggingFaceFW/fineweb-2)（`data/cmn_Hani/train/000_00000.parquet`）
- **许可证**：Open Data Commons Attribution License（ODC-By）v1.0
- **用途**：按 Parquet row group 经 HTTP Range 断点流式清洗高质量简体中文书面语（提取前 `375` 个 row group 中的 clean 中文段落），大幅降低低频词与常规搭配稀疏度。
- **清洗结果（`--fineweb-train-max-chars 180000000`）**：输出 `8,295,320` 行、`180,000,072` 个汉字。

### 2.4 第 3 期神经重排训练语料（Causal Character LM）
- **脚本与配方**：[train_reranker.py](../tools/train/train_reranker.py)（Stage-1）与 [finetune_reranker_s2.py](../tools/train/finetune_reranker_s2.py)（Stage-2），与第 2 期共用同一份 1,689 条评测黑名单并核验 `extra_leaked = 0`。
- **字符词表与 Stage-1 预训练**：从上述三份简体清洗语料（`11,370,832` 行、`2.21 亿` 汉字）统计前 `6,144` 词表（涵盖常用汉字与标点，`99.9907%` 汉字覆盖率），切分为 `train_tokens.u16`（`255,120,535` token）与 `val_tokens.u16`（`899,204` token），完成 `23,356` 步自归一化 Causal Character Transformer 预训练。
- **Stage-2 纯书面与的/得/地对比微调**：针对微博 LCCC 中大量「努力的/说的对/你好美」噪音会稀释虚词与的/得/地边界的问题，Stage-2 剔除 LCCC，仅保留 FineWeb-2 纯书面分片（`9,183,252` 行、`fw_train=230,227,486` token、`fw_val=750,855` token），并从中抽取 **`177,826` 条**独立「的/得/地」文法三元组（`的=60,000`、`得=57,826`、`地=60,000`；`174,826` 训练 / `3,000` 验证）联合训练 `3,000` 步，验证集三元组准确率从 `75.07%` 升至 **`94.57%`**（`的 96.0% / 得 93.5% / 地 94.2%`）。

## 3. 零泄漏原则与评测隔离

- `eval/sets/general.jsonl` 取自 [Tatoeba](https://tatoeba.org)（CC BY 2.0 FR），**仅用于评测，严禁进入任何训练或调参语料**。
- 从 `eval/sets/*.jsonl` 提取所有长度 $\ge 8$ 汉字的 `expected` 分句（共 `1,689` 条）作为精确防泄漏黑名单；仅当清洗后的整句或句内长分句精确等于其中任一条时整句丢弃（避免把「我们可以」「新年快乐」「这是我的」等 4–7 字高频日常短语误删成训练空洞）。
- 模型训练产出 `models/ime_model.bin` 与 `models/reranker.bin` 已加入 `.gitignore`，不直接提交至代码仓库；用户语料与临时文件保留在 `.external/` 下。

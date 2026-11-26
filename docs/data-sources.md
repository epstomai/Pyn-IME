# 数据来源与许可证

本项目的词库、训练语料与评测语料全部记录如下，所有商用/衍生分发受限或未经核实的语料均不采用。

## 1. 词库（CC BY 4.0 + MIT）

- **万象词库**：[amzxyz/rime-wanxiang](https://github.com/amzxyz/rime-wanxiang)（v18.2.0 Base 版）及 [amzxyz/rime-wanxiang-data](https://github.com/amzxyz/rime-wanxiang-data)，许可证 **CC BY 4.0**（要求署名与标明修改）。
  - 使用文件：`dicts/zi.dict.yaml`、`dicts/duoyin.dict.yaml`、`dicts/jichu.dict.yaml`、`dicts/diming.dict.yaml`、`dicts/mingren.dict.yaml`、`dicts/yiren.dict.yaml`。
- **词表规范化、简体过滤与精选净化（`jieba` + OpenCC `t2s` + `data/curated/`）**：[fxsjy/jieba](https://github.com/fxsjy/jieba) 默认词典 `dict.txt`（**MIT License**）配合 OpenCC `t2s` 简体化检验，并在 `ime-v5` 接入 [curate_lexicon.py](../tools/train/curate_lexicon.py) 与仓库自建 `data/curated/` 四份独立审核数据表：
  - 简体单字白名单：从 `zi.dict.yaml` 提取全部满足 `t2s(ch) == ch` 的纯汉字单字，共 `17,360` 字，剔除繁简异形干扰字（如 `麽`、`這`、`為`），并在 `ime-train` 导入 `zi.dict.yaml` 时额外过滤非简体常用异读 `著\tzhe`（仅保留 `zhù/zhuó`），确保评测集用字 `100%` 覆盖且不误出繁体异读。
  - 多字词过滤与净化：从 `jieba` 提取长度 2–6 字、词频 $\ge 3$ 且 `t2s(word) == word` 的纯汉字词条，并剔除会吞噬句尾共现的粘连词条，得基础词表 `295,302` 词；再剔除 [typo_banlist.txt](../data/curated/typo_banlist.txt)（人工审核 `151` 条无歧义错别字词，实际从基础词表清除 `97` 词），合并 [modern_extra.tsv](../data/curated/modern_extra.tsv)（`555` 条现代高频词、品牌与 CS/AI 术语，手工核验音节与权重）及 [cs_ai_boost.tsv](../data/curated/cs_ai_boost.tsv) 词条（`312` 条纯 CS/AI 术语），导出 `lexicon_words_v2.txt`（`295,670` 词）。训练时经 `--ban-words` 与 `--extra-dict`（保护精选多音词不因词表截断丢失，保留简体单字、`jieba` 词条及权重 $\ge 1000$ 的地名/名人/艺人专名），产出第 5 期模型词表 `225,812` 词。
- **读音概率与可选领域先验**：对同一词条的不同读音保留万象统计权重与 `--extra-dict` 校准权重，计算拼音发射对数概率 `reading_logp`；标准 415 音节表剔除了独立鼻音 `hm/m/n/ng`。仅当用户显式开启 `--cs-ai` 或设置界面的「CS / AI 术语优先」开关时，解码器才加载 [cs_ai_boost.tsv](../data/curated/cs_ai_boost.tsv) 的受控对数先验加成（仅作用于无简拼多字词，默认关闭）。

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
- **Stage-2 纯书面与错字/语法对比微调**：针对微博 LCCC 中大量「努力的/说的对/你好美」噪音会稀释虚词与的/得/地边界的问题，Stage-2 剔除 LCCC，仅保留 FineWeb-2 纯书面分片（`9,183,252` 行、`fw_train=230,227,486` token、`fw_val=750,855` token），从中抽取 **`177,826` 条**独立「的/得/地」文法三元组（`的=60,000`、`得=57,826`、`地=60,000`；`174,826` 训练 / `3,000` 验证），并在 `ime-v5` 进一步接入 [confusable_pairs.tsv](../data/curated/confusable_pairs.tsv)（**`166` 组 / `201` 对**严格无歧义同音错字与成语误写最小对）：在过滤评测黑名单后从 FineWeb-2 匹配出 `9,056` 条含目标词的真实书面语境句（覆盖 `152/166` 组）并与 `6×` 独立短语一起组成 `8,930` 条对比训练样本（验证集 `val_ctx=1,332`、`val_standalone=201`），按与线上一致的 `eos_weight=0.85`、`pair_weight=0.35`、`pair_margin=1.5` 联合微调 `2,000` 步，最终 `de_val_acc` 保持 **`94.70%`**，`pair_ctx` 由 `96.85%` 升至 **`99.92%`**、`pair_sa` 由 `83.58%` 升至 **`100.00%`**。

## 3. 零泄漏原则与评测隔离

- `eval/sets/general.jsonl` 取自 [Tatoeba](https://tatoeba.org)（CC BY 2.0 FR），**仅用于评测，严禁进入任何训练或调参语料**。
- `eval/sets/modern_orthography.jsonl`（`94` 条：覆盖现代高频词汇、常见成语正字法纠错与高冲突 CS/AI 术语对比）同样纳入零泄漏精确黑名单检查，并在训练前验证与所有 `eval/sets/*.jsonl` 训练语料零重叠。
- 从 `eval/sets/*.jsonl` 提取所有长度 $\ge 8$ 汉字的 `expected` 分句（共 `1,689` 条）作为精确防泄漏黑名单；仅当清洗后的整句或句内长分句精确等于其中任一条时整句丢弃（避免把「我们可以」「新年快乐」「这是我的」等 4–7 字高频日常短语误删成训练空洞）。
- 模型训练产出 `models/ime_model.bin` 与 `models/reranker.bin` 已加入 `.gitignore`，不直接提交至代码仓库；用户语料与临时文件保留在 `.external/` 下。

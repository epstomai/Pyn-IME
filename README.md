# Input Engine

一个以「少猜错」为目标的中文拼音输入法：Windows 平台、Rust 自研引擎、
n-gram 实时解码加小型神经网络重排。

当前处于第 5 期增强状态：在 **`ime-v4`** 核心解码器（`2.21 亿`汉字 KN 三元模型 + 混合精度 `W8A8 AVX-VNNI + F32 w_down` 神经重排器 + 受护增量自学习 `IMEUSR01`）之上，已完成 **Windows TSF 双架构输入法外壳（`ime-v5`）**：
- 双进程与双架构分离：**`ime_tsf.dll`**（`534.0 KiB`, x64）与 **`ime_tsf_x86.dll`**（`439.0 KiB`, x86 WOW64，兼容 32 位 QQ 与旧 Win32/CUAS 应用）作为轻量 COM 进程内文本服务，提供 DPI 自适应深浅色圆角**横排**悬浮候选框、**打字中途单击 `Shift` 切换中/英候选**（保留编码并生成英文前缀补全）、**`Shift` 系统双向状态同步**与 850 ms 光标浮标、以及跨应用 32 字上下文读取；通过 `Low-IL + AppContainer` 兼容 Named Pipe `\\.\pipe\InputEngineTSF_v1` 连接共享守护进程 **`ime-server.exe`**（`768.0 KiB`），全系统仅加载一份约 180 MB 模型并统一安全自学习；
- Pure-CPU 极致延迟：`run_eval.py` 全量评测单次 `convert` 延迟 **`p50 1.19 ms`、`p95 2.74 ms`、`max 6.33 ms`**；实测 Named Pipe 40 次实时往返采样 **`p50 1.111 ms`、`p95 2.257 ms`、`max 2.885 ms`**；
- 在开启受护自学习下保持全部 `13` 个污染场景 **`0` 回归**、门禁干净通过 **`828 / 830`**、`general` `heldout` **`76.80%` / `CER 4.55%`**、`de_grammar` `heldout` **`85.71%`**。

## 目录

| 路径 | 内容 |
|---|---|
| `crates/ime-core` | 零依赖纯 Rust 引擎核心（无分支查表、DAG + arena Beam Search、混合精度 `IMERER01` AVX2/AVX-VNNI + Prefix KV 缓存重排、`IMEUSR01` 受护用户自学习） |
| `crates/ime-ipc` | 长度前缀 JSON IPC 协议、AppContainer/Low-IL 权限 Win32 Named Pipe 传输层、英文词表前缀补全（`english.rs`）与状态机 `SessionController` |
| `crates/ime-server` | Windows 单实例本地推理与共享自学习守护进程（`ime-server.exe`，原子持久化 `%LOCALAPPDATA%\InputEngine\user_dict.tsv`） |
| `crates/ime-tsf` | Windows TSF x64/x86 双架构 COM 进程内 DLL（`ITfTextInputProcessorEx`、`ITfCompartmentEventSink` 双向同步与 DPI 自适应横排 GDI 悬浮候选窗） |
| `crates/ime-train` | 离线训练工具：导入万象词库与清洗语料，训练并导出 `models/ime_model.bin` |
| `crates/ime-cli` | 命令行前端：`--jsonl` / `--interactive` 交互试打，支持 `--reranker`、`--user-dict`、`--no-learn` 与 `:learn` / `:commit` |
| `tools/windows` | Windows TSF x64/x86 双架构部署、ACL 授权与 `System32`/`SysWOW64` `regsvr32` 注册/卸载脚本（`install_tsf.py`） |
| `tools/train` | 开放语料清洗脚本与神经重排两阶段 GPU 训练脚本（`prepare_corpus.py` / `train_reranker.py` / `finetune_reranker_s2.py`） |
| `tools/evalkit` | 评测集构建脚本、评测器、Rime 基线适配 |
| `eval/src` | 手写的评测源文件 |
| `eval/sets` | 生成的评测集 |
| `docs/eval-protocol.md` | 评测协议、用例格式与指标 |
| `docs/data-sources.md` | 词库与训练语料来源、许可证及零泄漏清洗说明 |
| `docs/baseline.md` | Rime 基线与自研引擎实测对比（含 CPU 混合精度优化与双架构进展） |
| `docs/windows-tsf.md` | Windows TSF 外壳架构、快捷键、AppContainer/WOW64 兼容与安装排障指南 |
| `.external/` | 下载的第三方引擎、模型与语料，不入库 |
| `models/` | 本地训练生成的二进制语言模型与重排权重，不入库 |

## 环境与使用

```
python -m venv .venv
.venv/Scripts/python -m pip install pypinyin opencc-python-reimplemented jieba pyarrow numpy torch
.venv/Scripts/python tools/train/prepare_corpus.py
cargo build --release
target/release/ime-train.exe --dicts .external/wanxiang/dicts --allowed-chars .external/corpora/allowed_chars.txt --lexicon .external/corpora/lexicon_words.txt --corpus .external/corpora/clean_lccc.txt --corpus .external/corpora/clean_fineweb.txt --corpus .external/corpora/clean_fineweb_train.txt --out models/ime_model.bin --max-bigrams 7000000 --max-trigrams 7000000 --shards 8
.venv/Scripts/python tools/train/train_reranker.py --steps 23356 --out models/reranker_s1.bin
.venv/Scripts/python tools/train/finetune_reranker_s2.py --init-pt .external/corpora/reranker_data/stage1_best.pt --out models/reranker.bin --steps 3000
```

试打、评测与 Windows TSF x64/x86 双架构系统安装（管理员终端）：

```
target/release/ime-cli.exe --interactive --model models/ime_model.bin --reranker models/reranker.bin --user-dict models/user_dict.tsv
.venv/Scripts/python tools/evalkit/run_eval.py --engine cmd --cmd "target/release/ime-cli.exe --jsonl --model models/ime_model.bin --reranker models/reranker.bin" --name ime-v4

# 构建 32 位 WOW64 + 64 位 TSF 产物（首次需安装 i686 目标）
rustup target add i686-pc-windows-msvc
cargo build --release
cargo build -p ime-tsf --release --target i686-pc-windows-msvc

# 部署并双架构注册 Windows TSF 输入法（详见 docs/windows-tsf.md）
.venv/Scripts/python tools/windows/install_tsf.py deploy
.venv/Scripts/python tools/windows/install_tsf.py register
.venv/Scripts/python tools/windows/install_tsf.py status
# 卸载：.venv/Scripts/python tools/windows/install_tsf.py unregister
```

## 数据来源与许可

- 词库、训练语料与评测集隔离规则详见 [docs/data-sources.md](docs/data-sources.md)。
- `eval/sets/general.jsonl` 的句子取自 [Tatoeba](https://tatoeba.org)（CC BY 2.0 FR），仅用于评测，严禁进入训练语料。
- librime（BSD-3-Clause）与万象拼音配置（CC BY 4.0）只作为对比基线运行，不随本项目分发。

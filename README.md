# Input Engine

一个以「少猜错」为目标的中文拼音输入法：Windows 平台、Rust 自研引擎、
n-gram 实时解码加小型神经网络重排。

当前处于第 5 期增强状态：在 **`ime-v4`** 核心解码器（`2.21 亿`汉字 KN 三元模型 + 混合精度 `W8A8 AVX-VNNI + F32 w_down` 神经重排器 + 受护增量自学习 `IMEUSR01`）之上，已完成 **Windows TSF 双架构输入法外壳与配置界面（`ime-v5`）**：
- 双进程与双架构分离：**`ime_tsf_gNNNN.dll`**（`643.0 KiB`, x64）与 **`ime_tsf_x86_gNNNN.dll`**（`533.5 KiB`, x86 WOW64，兼容 32 位 QQ 与旧 Win32/CUAS 应用）作为轻量 COM 进程内文本服务，提供 **Vercel / Geist 极简黑白风格** DPI 自适应深浅色圆角悬浮候选框（支持**横排 / 竖排**布局、SDF 抗锯齿胶囊、Windows 11 原生 `DWMWA_BORDER_COLOR` 细边、现代中文字体自动优选 `MiSans` / `OPPO Sans 4.0` / `HarmonyOS Sans SC` / `Noto Sans SC` / `思源黑体` 等，以及默认 `Regular 400` 字重 + `Subtle` 极简微抬升边框高亮）、**代际版本化零重启热切换**（`install_tsf.py deploy` 自动递增代际并派生独立 CLSID/Profile/窗口类，通过 `InstallLayoutOrTip` 切换激活并卸载清理旧代际，已打开应用无需重启即可加载新版 DLL）、**首字母大写 `Shift+A–Z` 直接入窗组合**、**打字中途或空闲单击 `Shift` 切换中/英**（通过 TSF `PreserveKey(VK_SHIFT, TF_MOD_ON_KEYUP)` + `OnPreservedKey`/`OnTestKeyUp`/`OnKeyUp` 兼容 Windows 11 / Chromium / Electron）、**`Ctrl + .` 切换中/英文标点**（`中.` 状态直出半角英文符号且无需切出中文输入法）、以及跨应用 32 字上下文读取；通过 `Low-IL + AppContainer` 兼容 Named Pipe `\\.\pipe\InputEngineTSF_v1` 连接共享守护进程 **`ime-server.exe`**（`878.0 KiB`），全系统仅加载一份约 180 MB 模型并统一安全自学习；
- 独立配置界面 **`ime-settings.exe`**（`452.0 KiB`）：同样采用原生 Vercel / Geist 界面风格，提供实时预览舞台、主题/布局/高亮样式/字体/字号/首选字重/候选数量与输入行为开关；修改自动原子持久化至 `%LOCALAPPDATA%\InputEngine\config.json` 并经 Named Pipe（`GetConfig` / `SetConfig` + `config_revision`）实时热同步到所有运行中的 `ime_tsf*.dll` 实例，部署时自动创建开始菜单快捷方式 `Input Engine 设置`；
- Pure-CPU 极致延迟：`run_eval.py` 全量评测单次 `convert` 延迟 **`p50 1.19 ms`、`p95 2.74 ms`、`max 6.33 ms`**；实测 Named Pipe 40 次实时往返采样 **`p50 1.111 ms`、`p95 2.257 ms`、`max 2.885 ms`**；
- 在开启受护自学习下保持全部 `13` 个污染场景 **`0` 回归**、门禁干净通过 **`828 / 830`**、`general` `heldout` **`76.80%` / `CER 4.55%`**、`de_grammar` `heldout` **`85.71%`**。

## 目录

| 路径 | 内容 |
|---|---|
| `crates/ime-core` | 零依赖纯 Rust 引擎核心（无分支查表、DAG + arena Beam Search、混合精度 `IMERER01` AVX2/AVX-VNNI + Prefix KV 缓存重排、`IMEUSR01` 受护用户自学习） |
| `crates/ime-ipc` | 长度前缀 JSON IPC 协议（含 `GetConfig`/`SetConfig` 热同步）、配置模型 `config.rs`、Win32 Named Pipe 传输层、英文词表前缀补全与状态机 `SessionController` |
| `crates/ime-server` | Windows 单实例本地推理、共享自学习与配置同步守护进程（`ime-server.exe`，原子持久化 `%LOCALAPPDATA%\InputEngine\user_dict.tsv` 与 `config.json`） |
| `crates/ime-tsf` | Windows TSF x64/x86 双架构 COM 进程内 DLL（`ITfTextInputProcessorEx`、`ITfCompartmentEventSink` 双向同步与 DPI 自适应横排/竖排 GDI 悬浮候选窗） |
| `crates/ime-settings` | 原生 Win32 DPI 自适应 Vercel/Geist 风格 GUI 设置程序（`ime-settings.exe`，1:1 候选窗实时预览、现代字体选择与热生效配置下发） |
| `crates/ime-train` | 离线训练工具：导入万象词库与清洗语料，训练并导出 `models/ime_model.bin` |
| `crates/ime-cli` | 命令行前端：`--jsonl` / `--interactive` 交互试打，支持 `--reranker`、`--user-dict`、`--no-learn` 与 `:learn` / `:commit` |
| `tools/windows` | Windows TSF x64/x86 双架构部署、代际版本化零重启热切换、ACL 授权与 `System32`/`SysWOW64` `regsvr32` 注册/卸载脚本（`install_tsf.py`） |
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
.venv/Scripts/python tools/windows/install_tsf.py settings
# 卸载：.venv/Scripts/python tools/windows/install_tsf.py unregister
```

## 数据来源与许可

- 词库、训练语料与评测集隔离规则详见 [docs/data-sources.md](docs/data-sources.md)。
- `eval/sets/general.jsonl` 的句子取自 [Tatoeba](https://tatoeba.org)（CC BY 2.0 FR），仅用于评测，严禁进入训练语料。
- librime（BSD-3-Clause）与万象拼音配置（CC BY 4.0）只作为对比基线运行，不随本项目分发。

# Pyn

一个以「少猜错」为目标的中文拼音输入法：Windows 平台、Rust 自研引擎、
n-gram 实时解码加小型神经网络重排。

当前处于第 5 期增强状态：在自研核心解码器（`2.21 亿`汉字 KN 三元模型 + 混合精度 `W8A8 AVX-VNNI + F32 w_down` 神经重排器 + 受护增量自学习 `IMEUSR01`）之上，已完成 **词库净化与重排对比微调、可选 CS/AI 术语开关、英文频次分层补全及 Windows TSF 双架构输入法外壳与配置界面（`ime-v5`）**：
- 双进程与双架构分离：**`ime_tsf_gNNNN.dll`**（`789.5 KiB`, x64）与 **`ime_tsf_x86_gNNNN.dll`**（`666.5 KiB`, x86 WOW64，兼容 32 位 QQ 与旧 Win32/CUAS 应用）作为轻量 COM 进程内文本服务，提供 **Vercel / Geist 极简黑白风格** DPI 自适应深浅色圆角悬浮候选框（支持**横排 / 竖排**布局、SDF 抗锯齿胶囊、Windows 11 原生 `DWMWA_BORDER_COLOR` 细边、现代中文字体自动优选 `MiSans` / `OPPO Sans 4.0` / `HarmonyOS Sans SC` / `Noto Sans SC` / `思源黑体` 等，以及默认 `Regular 400` 字重 + `Subtle` 极简微抬升边框高亮）、**代际版本化零重启热切换**（`install_tsf.py deploy` 自动递增代际并派生独立 CLSID/Profile/窗口类，通过 `InstallLayoutOrTip` 切换激活并卸载清理旧代际，已打开应用无需重启即可加载新版 DLL）、**首字母大写 `Shift+A–Z` 直接入窗组合**、**打字中途按 `Shift` 立即上屏当前英文并切英文（按使用频次分层排序的英文词表前缀补全，覆盖常用日常与开发/AI 术语）、空闲单击 `Shift` 切换中/英**（通过 TSF `PreserveKey(VK_SHIFT, TF_MOD_ON_KEYUP)` + `OnPreservedKey`/`OnTestKeyUp`/`OnKeyUp` 兼容 Windows 11 / Chromium / Electron）、**`Ctrl + .` 切换中/英文标点**（`中.` 状态直出半角英文符号且无需切出中文输入法）、以及跨应用 32 字上下文读取；通过 `Low-IL + AppContainer` 兼容 Named Pipe `\\.\pipe\PynTSF_v1` 连接共享守护进程 **`ime-server.exe`**（`1022.0 KiB`），全系统仅加载一份约 180 MB 模型并统一安全自学习；
- 可选领域开关与配置界面 **`ime-settings.exe`**（`666.5 KiB`）：同样采用原生 Vercel / Geist 界面风格，提供实时预览舞台、主题/布局/高亮样式/字体/字号/首选字重/候选数量，以及输入行为与 **「CS / AI 术语优先」** 可选开关（默认关闭以保持日常词先验如 `xiecheng → 携程`，开启后即对 `协程`/`泛型`/`栈帧`/`算子`/`词元`/`显存`/`梯度`/`蒸馏` 等纯 CS/AI 术语精准置顶，命令行亦可用 `--cs-ai` 切换）；修改自动原子持久化至 `%LOCALAPPDATA%\Pyn\config.json` 并经 Named Pipe（`GetConfig` / `SetConfig` + `config_revision`）实时热同步到所有运行中的 `ime_tsf*.dll` 实例，部署时自动创建开始菜单快捷方式 `Pyn 设置`；
- Pure-CPU 极致延迟：`run_eval.py` 全量评测单次 `convert` 延迟 **`p50 1.15 ms`、`p95 2.70 ms`、`max 6.51 ms`**；实测 Named Pipe 40 次实时往返采样 **`p50 1.111 ms`、`p95 2.257 ms`、`max 2.885 ms`**；
- 在开启受护自学习下保持全部 `13` 个污染场景 **`0` 回归**、门禁干净通过 **`830 / 830`**（虚词 `0/355`、分段上屏 `0/423`、简拼尾字 `0/52`、优先级 `0/456`）、`general` `heldout` **`76.89%` / `CER 4.53%`**、`de_grammar` `heldout` **`92.86%`**、现代用字与正字法集 `modern_orthography` `heldout` **`97.30%`**（较 `ime-v4` `81.08%` 提升 `16.22` 个百分点）。

## 目录

| 路径 | 内容 |
|---|---|
| `crates/ime-core` | 零依赖纯 Rust 引擎核心（无分支查表、DAG + arena Beam Search、混合精度 `IMERER01` AVX2/AVX-VNNI + Prefix KV 缓存重排、`IMEUSR01` 受护用户自学习） |
| `crates/ime-ipc` | 长度前缀 JSON IPC 协议（含 `GetConfig`/`SetConfig` 热同步）、配置模型 `config.rs`、Win32 Named Pipe 传输层、频次分层英文前缀补全与状态机 `SessionController` |
| `crates/ime-server` | Windows 单实例本地推理、共享自学习与配置同步守护进程（`ime-server.exe`，原子持久化 `%LOCALAPPDATA%\Pyn\user_dict.tsv` 与 `config.json`） |
| `crates/ime-tsf` | Windows TSF x64/x86 双架构 COM 进程内 DLL（`ITfTextInputProcessorEx`、`ITfCompartmentEventSink` 双向同步与 DPI 自适应横排/竖排 GDI 悬浮候选窗） |
| `crates/ime-settings` | 原生 Win32 DPI 自适应 Vercel/Geist 风格 GUI 设置程序（`ime-settings.exe`，1:1 候选窗实时预览、现代字体选择、`CS / AI 术语优先` 开关与热生效配置下发） |
| `crates/ime-train` | 离线训练工具：导入万象词库、错词过滤表（`--ban-words`）与补充词典（`--extra-dict`）及清洗语料，训练并导出 `models/ime_model.bin` |
| `crates/ime-cli` | 命令行前端：`--jsonl` / `--interactive` 交互试打，支持 `--reranker`、`--cs-ai`、`--user-dict`、`--no-learn` 与 `:learn` / `:commit` |
| `data/curated` | 人工审核的错词屏蔽表（`typo_banlist.txt`）、现代高频/CS 补词表（`modern_extra.tsv`）、可选 CS/AI 提权表（`cs_ai_boost.tsv`）与错字最小对（`confusable_pairs.tsv`） |
| `tools/windows` | Windows TSF x64/x86 双架构部署、代际版本化零重启热切换、ACL 授权与 `System32`/`SysWOW64` `regsvr32` 注册/卸载脚本（`install_tsf.py`） |
| `crates/ime-android` | Android JNI 引擎封装（`libpyn_ime.so`，arm64 NEON / `sdot` 重排内核，连续按键延迟解码） |
| `android` | Android 26 键输入法 App（`com.pyn.ime`，后台引擎线程、候选栏 / 全屏候选、上滑数字符号） |
| `tools/android` | 精简模型生成、`cargo ndk` 构建与 APK 安装脚本（`build_install.py`） |
| `tools/release` | GitHub 发布包打包脚本（`package_release.py`：Windows zip + Android APK） |
| `tools/train` | 开放语料清洗脚本、`curate_lexicon.py` 词表净化及神经重排两阶段 GPU 训练脚本（`prepare_corpus.py` / `train_reranker.py` / `finetune_reranker_s2.py`） |
| `tools/evalkit` | 评测集构建脚本、评测器、Rime 基线适配与 `check_cs_domain.py` 领域 A/B 检查 |
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
.venv/Scripts/python tools/train/curate_lexicon.py
cargo build --release
target/release/ime-train.exe --dicts .external/wanxiang/dicts --allowed-chars .external/corpora/allowed_chars.txt --ban-words data/curated/typo_banlist.txt --extra-dict data/curated/modern_extra.tsv --lexicon .external/corpora/lexicon_words_v2.txt --corpus .external/corpora/clean_lccc.txt --corpus .external/corpora/clean_fineweb.txt --corpus .external/corpora/clean_fineweb_train.txt --out models/ime_model.bin --max-bigrams 7000000 --max-trigrams 7000000 --shards 8
.venv/Scripts/python tools/train/train_reranker.py --steps 23356 --out models/reranker_s1.bin
.venv/Scripts/python tools/train/finetune_reranker_s2.py --stage1 models/reranker_s1.bin --out models/reranker.bin --steps 2000
```

试打、评测与 Windows TSF x64/x86 双架构系统安装（管理员终端）：

```
target/release/ime-cli.exe --interactive --model models/ime_model.bin --reranker models/reranker.bin --user-dict models/user_dict.tsv
.venv/Scripts/python tools/evalkit/run_eval.py --engine cmd --cmd "target/release/ime-cli.exe --jsonl --model models/ime_model.bin --reranker models/reranker.bin" --name ime-v5
.venv/Scripts/python tools/evalkit/run_eval.py --engine cmd --cmd "target/release/ime-cli.exe --jsonl --model models/ime_model.bin --reranker models/reranker.bin" --suites modern_orthography --name ime-v5-mod
.venv/Scripts/python tools/evalkit/check_cs_domain.py

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

## 移动端精简模型

Android 版不打包 173 MB 的全量模型，而是由 `ime-slim` 从全量模型离线生成 `models/ime_model_mobile.bin`（`IMEMOD02`，约 48 MB），APK 内仍以 `ime_model.bin` 名义分发：

- **相对熵（Stolcke）剪枝**：二元 / 三元各保留 250 万（全量各 700 万），按删除代价最小的顺序裁剪，回退权重随剪枝同步重算；
- **虚词 / 句尾保护**：预测 `</s>` 或受保护虚词（`的/地/得/了/着/过/吗/呢/吧/啊/呀/啦/嘛/哦/么`）的 n-gram 删除代价 ×5，守住语气词与结构助词门禁；
- **8 位码本量化**：二元 / 三元概率与回退权重以 256 级 Lloyd-Max 码本存盘（精确 0 值单独编码），加载时还原为 `f32`，解码热路径与桌面端不变。

与全量模型对比（同一 `reranker.bin`）：`general` heldout `76.89%`（持平）、`function_words` all `93.87%`（全量 `93.31%`）、`incremental` all `91.99%`（全量 `91.88%`），全部门禁 `0` 失败、污染基线 `830 / 830`、`13` 个场景 `0` 回归。真机（arm64）冷加载约 `0.17 s`，Native Heap 约 `104 MB`（全量 `208 MB`），APK `57.6 MB`（原 `182 MB`）。

```
cargo build --release -p ime-train --bin ime-slim
target/release/ime-slim.exe --in models/ime_model.bin --out models/ime_model_mobile.bin --max-bigrams 2.5M --max-trigrams 2.5M --protect-weight 5
# 或直接：.venv/Scripts/python tools/android/build_install.py --install（全量模型更新后自动重新生成）
```

## 发布包

```bash
# 生成 dist/Pyn-<版本>-windows.zip、dist/Pyn-<版本>-android-arm64.apk 与 SHA256SUMS.txt
.venv/Scripts/python tools/release/package_release.py
```

- **Windows**：TSF（x64 + x86）、`ime-server`、`ime-settings` 以静态 CRT 链接（无需 VC++ 运行库），构建于独立的 `target/dist`；压缩包附带 `install.cmd` / `uninstall.cmd`（`tools/windows/release/`），无需 Python，自动请求管理员权限完成注册、AppContainer 授权、开机自启与开始菜单“Pyn 设置”快捷方式；
- **Android**：非 debuggable 的 release APK（arm64-v8a，内置精简模型），签名密钥取自 `PYN_KEYSTORE` / `PYN_KEYSTORE_PASS` / `PYN_KEY_ALIAS` / `PYN_KEY_PASS`，未设置时使用本机 Android debug 密钥（可直接覆盖 `build_install.py` 安装的开发版）。

## 数据来源与许可

- 词库、训练语料与评测集隔离规则详见 [docs/data-sources.md](docs/data-sources.md)。
- `eval/sets/general.jsonl` 的句子取自 [Tatoeba](https://tatoeba.org)（CC BY 2.0 FR），仅用于评测，严禁进入训练语料。
- librime（BSD-3-Clause）与万象拼音配置（CC BY 4.0）只作为对比基线运行，不随本项目分发。

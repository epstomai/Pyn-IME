# Windows TSF 输入法外壳架构与安装指南（`ime-v5`）

本文档说明 Pyn 第 5 期 Windows Text Services Framework（TSF）输入法集成的架构设计、安全边界、按键交互及部署排障方法。

## 1. 双进程分离架构

为保证全系统（含多进程桌面应用与 UWP/AppContainer 应用）共享一份 ~188 MB 模型、统一安全自学习状态，并使核心 crate [ime-core](../crates/ime-core/Cargo.toml) 保持零依赖，TSF 外壳采用瘦客户端 + 本地守护服务架构：

```text
[目标应用 (记事本 / VS Code / Terminal / Edge / QQ)]       [配置界面 ime-settings.exe (452.0 KiB)]
        │  COM In-Process (ITfTextInputProcessorEx)                 │
        ▼                                                           │ GetConfig / SetConfig
[ime_tsf_gNNNN.dll (643.0 KiB) / ime_tsf_x86_gNNNN.dll (533.5 KiB)] │ (实时推送 + 修订号同步)
        │  轻量 TSF 外壳 + Vercel/Geist 横排/竖排悬浮候选窗           │
        └──────────────────────┬────────────────────────────────────┘
                               │ Win32 Named Pipe (\\.\pipe\PynTSF_v1)
                               │ 长度前缀 JSON 帧 (u32 LE + UTF-8)
                               ▼
                      [ime-server.exe (878.0 KiB)]
                               │ 单实例本地守护进程 (Local\PynServerMutex_v1)
                               │ 共享 Arc<Model> + Option<Arc<Reranker>> + UserMemory + ImeConfig
                               ▼
    [%LOCALAPPDATA%\Pyn\user_dict.tsv  &  config.json]  <-- MoveFileExW 原子落盘
```

- **实测 Named Pipe 端到端延迟**（`ime-server.exe` 挂载 `181.1 MB` KN 三元模型 + 混合精度 `AVX-VNNI + F32 w_down` 神经重排器，40 轮冷启动与连续击键实时查询）：**p50 `1.111 ms`、p95 `2.257 ms`、max `2.885 ms`**（较初期 Phase 5 基准 `p50 2.74 ms / p95 4.21 ms / max 4.90 ms` 延迟减半以上）。

## 2. COM 身份、双架构注册、AppContainer/CUAS 兼容与 Loader Lock 安全

| 项目 | 标识 / 策略 |
|---|---|
| `CLSID_PYN_TIP` | 基准 `{A94E2C1B-7D3F-4E8A-9B12-6C5F8A3D0101}`；代际版本化 `ime_tsf_gNNNN.dll` 自动派生 `{A94E2C1B-7D3F-4E8A-9B20-6C5F8A3DNNNN}` |
| `GUID_PYN_PROFILE` | 基准 `{A94E2C1B-7D3F-4E8A-9B12-6C5F8A3D0102}`；代际版本化自动派生 `{A94E2C1B-7D3F-4E8A-9B21-6C5F8A3DNNNN}` |
| `GUID_DISPLAY_ATTR_INPUT` | 基准 `{A94E2C1B-7D3F-4E8A-9B12-6C5F8A3D0103}`；代际版本化自动派生 `{A94E2C1B-7D3F-4E8A-9B22-6C5F8A3DNNNN}`（热键与窗口类同理独立） |
| 零重启代际热切换 | `install_tsf.py deploy` 自动递增 `gNNNN`、`regsvr32` + `InstallLayoutOrTip` 启用新代际并卸载清理旧代际注册表键，仍被旧进程映射的过期 DLL 登记 `MOVEFILE_DELAY_UNTIL_REBOOT` 延迟删除，已打开应用无需重启即可加载新版 DLL |
| 语言标识 / 名称 | `0x0804` (`zh-CN`) / `Pyn 拼音` |
| 注册类别（8 项） | `GUID_TFCAT_TIP_KEYBOARD`、`IMMERSIVESUPPORT`、`SYSTRAYSUPPORT`、`UIELEMENTENABLED`、`SECUREMODE`、`COMLESS`、`INPUTMODECOMPARTMENT`、`DISPLAYATTRIBUTEPROVIDER` |
| 双架构 COM / WOW64 | 先以 `SysWOW64\regsvr32.exe` 注册 `ime_tsf_x86.dll` 写入 `WOW6432Node\...\CLSID`，再以 `System32\regsvr32.exe` 注册 `ime_tsf.dll`；兼容 32 位 QQ 与旧 Win32/CUAS 应用 |
| Named Pipe SDDL | `D:(A;;GA;;;BA)(A;;GA;;;SY)(A;;GA;;;AU)(A;;GRGW;;;AC)S:(ML;;NW;;;LW)`（允许标准用户、UWP AppContainer 与 Low Mandatory Level 沙箱/低权限客户端连接高权限守护进程） |
| 安装目录 ACL | `icacls <dir> /grant "*S-1-15-2-1:(OI)(CI)(RX)" /T /Q`（授予 `ALL APPLICATION PACKAGES` 读取与加载 `ime_tsf*.dll` 权限） |
| 导出符号封装 | 通过 `ime_tsf.def` 将 `DllCanUnloadNow` / `DllGetClassObject` / `DllInstall` / `DllRegisterServer` / `DllUnregisterServer` 标记为 `PRIVATE`（x86 stdcall 亦保持无修饰精确导出） |
| `DllMain` 约束 | `DLL_PROCESS_ATTACH` 仅保存 `HINSTANCE`，严禁初始化 COM、创建窗口、启动子进程或连接 Named Pipe |
| EditSession 与光标兼容 | 读上文使用 `TF_ES_READ`；同步锁失败时自动回退 `TF_ES_ASYNCDONTCARE`；无现有组合时以瞬时组合包裹提交以兼容 CUAS/QQ/Electron；TSF `GetTextExt` 缺失时回退 `GetGUIThreadInfo` + `ClientToScreen` |

## 3. 按键与交互映射（`SessionController` 与中途 `Shift` 切英文候选）

状态机实现在无 Win32 依赖的 [session.rs](../crates/ime-ipc/src/session.rs) 与英文前缀补全模块 [english.rs](../crates/ime-ipc/src/english.rs)：

| 场景 | 按键 | 行为说明 |
|---|---|---|
| 空闲（中文模式） | `a–z` | 启动拼音组合并读取光标前最多 `32` 字上文发起转换 |
| 空闲（中文模式） | `Shift+A–Z`（`CapsLock` 关闭时） | 从首个大写字母直接进入输入法英文组合（如 `NiloToon` 完整保留在候选栏内，`Space`/`Enter` 上屏后仍保持中文模式，`Backspace` 删空自动退出，不误切中英；误按首字大写时单击 `Shift` 可原地转小写切回中文候选） |
| 空闲（中文模式） | `, . ? ! ; : ( ) < > \ " ' ^ _` | 当处于中文标点模式（默认，`，。`）时直接上屏对应中文全角标点（`"`/`'` 交替配对 `“”`/`‘’`）；按 **`Ctrl + .`** 切为英文标点（`,.`）后全部直出半角英文符号（无需切换英文键盘） |
| 任意时刻（空闲 / 组合中） | **`Ctrl + .`**（`PreserveKey(VK_OEM_PERIOD, TF_MOD_CONTROL)` + 键sink回退） | 切换「中文标点（`，。` / 徽章 `中`） ↔ 英文标点（`,.` / 徽章 `中.`）」并同步 `TF_CONVERSIONMODE_SYMBOL`；保持中文打字模式不变，组合中途输入标点时自动提交「候选词 + 半角英文符号」 |
| 空闲 | 单击 `Shift`（`PreserveKey(VK_SHIFT, TF_MOD_ON_KEYUP)` -> `OnPreservedKey` + `OnTestKeyUp`/`OnKeyUp` + 丢 `KeyUp` 恢复） | 在 Windows 11 / Chromium / Electron / QQ 中可靠切换「中 ↔ 英」模式并通过 `ITfCompartmentEventSink` 与任务栏指示器双向同步，光标处弹出 850 ms `中` / `英` 状态浮标；带组合键（如 `Shift+N`/`Shift+(`/`Ctrl+Shift`）与长按 repeat 保护 |
| 空闲（刚上屏中文候选后） | `Backspace` | 键事件放行让宿主应用删除误选文字，同时向 `ime-server` 发送 `Undo` 触发回滚与惩罚门槛 |
| 组合态（中途切换中/英候选） | 单击 `Shift`（`OnPreservedKey` + `OnTestKeyUp`/`OnKeyUp` + 丢 `KeyUp` 恢复） | **保留当前已输入编码不清空**并切换「中 ↔ 英」：切英立即以原编码置顶 + 内置 1,410 词表前缀补全直接生成英文候选（不锁定后续中文输入）；切中转小写并重算中文候选；无任何自动英文识别以免长拼音/错拼被误锁 |
| 组合态（大写驼峰 / 符号 / 小键盘 / CapsLock） | `Shift+A–Z` / `CapsLock A–Z` / `0` / `/` / `RawChar` 符号与小键盘键 | `UpperChar`（如 `NiloToon` 途中按 `Shift+T`）追加进当前编码并临时展示英文候选，退格删回纯小写立即恢复中文（不再放行造成前插错位）；`CapsLock` 字母与 `0`、`/`、`RawChar` 同步提交「已输入编码+符号」结束组合且不锁定英文模式 |
| 组合态 | `a–z` / `'` / `Backspace` | 追加拼音（或英文字符）/ 音节分隔符 `'` / 回退编码；退空自动取消组合并隐藏候选窗 |
| 组合态 | `Space` / `1–5` | 提交高亮候选或本页第 1–5 项（英文组合态下提交英文且不写入中文学习；首字母大写进入的英文组合提交后自动保持中文模式） |
| 组合态 | `Enter` / `Escape` | `Enter` 提交原始编码（不记录学习）；`Escape` 清空组合并关闭候选窗 |
| 组合态（横排候选窗） | `Left` / `Right` / `Up` / `Down` / `Tab` / `-` / `=` / `PageUp` / `PageDown` | 横排高亮光标移动或每页 `5` 项翻页 |
| 组合态 | 中文可映射标点（如 `, . ? !`） | 中文组合时自动上屏「当前高亮候选 + 中文全角标点」并仅学习纯汉字；英文组合时直接提交「选中英文 + 半角英文标点」（不学习） |
| 组合 / 空闲 | `Ctrl+*` / `Alt+*` / `Win+*` / 空闲 `CapsLock A–Z` | 不拦截，直通宿主应用（`CapsLock` 空闲直接直出英文，组合态安全顺序提交） |

## 4. 图形化配置 GUI 与热重载 (`ime-settings.exe`)

- **原生配置界面**：新增 [crates/ime-settings](../crates/ime-settings/Cargo.toml)（编译产物 `ime-settings.exe`, `452.0 KiB`），采用相同 Vercel / Geist 黑白极简设计，内置 **1:1 实时预览舞台**，可配置：
  - **界面主题**：自动跟随系统（`AppsUseLightTheme`）/ 深色 / 浅色；
  - **候选布局与高亮**：横排 / 竖排、极简微抬升边框高亮（`Subtle`，默认）/ 黑白反色高亮（`Inverted`）；
  - **字体与排版**：启动时通过 `EnumFontFamiliesExW` 自动优选已安装的现代中文字体（`MiSans` → `OPPO Sans 4.0` → `HarmonyOS Sans SC` → `Noto Sans SC` → `Source Han Sans` → `思源黑体` → `Microsoft YaHei` → `Microsoft YaHei UI`），支持从本机全部字体列表中手动指定、调整字号（`12–22 px`，默认 `15 px`）、首选词字重（默认 `常规 400`，消除 GDI `700` 粗体发糊）以及每页候选数（`3–9` 项，默认 `5` 项）；
  - **输入与行为开关**：默认中文标点、单击 `Shift` 切换中英文、首字母大写进入英文组合、拼音光标、模式切换浮标与本地词频自学习开关。
- **配置持久化与跨进程热同步**：配置由 [config.rs](../crates/ime-ipc/src/config.rs) 通过 `MoveFileExW` 原子落盘至 `%LOCALAPPDATA%\Pyn\config.json`；修改时立即通过 Named Pipe 发送 `SetConfig`，守护进程递增 `config_revision` 并在每次 `Convert` 响应及焦点切换时将新配置热同步给所有运行中的 `ime_tsf*.dll` 实例，无需重启已打开应用。
- **打开设置方式**：开始菜单搜索 **`Pyn 设置`**（由 `install_tsf.py deploy` 创建快捷方式），运行 `C:\Program Files\Pyn\ime-settings.exe`，或执行 `.\.venv\Scripts\python tools\windows\install_tsf.py settings`。

## 5. 部署、注册与卸载命令（管理员权限）

```powershell
# 0. 安装 32 位目标（仅首次）
rustup target add i686-pc-windows-msvc

# 1. 编译 x64 release 产物（ime_tsf.dll + ime-server.exe + ime-settings.exe）与 x86 32 位 ime_tsf.dll
cargo build --release
cargo build -p ime-tsf --release --target i686-pc-windows-msvc

# 2. 部署 x64 + x86 产物到 C:\Program Files\Pyn（已注册时自动执行代际热切换，无需重启应用）、创建设置快捷方式并赋予 AppContainer ACL
.\.venv\Scripts\python tools\windows\install_tsf.py deploy

# 3. 双架构注册 COM（WOW64 + x64）与 TSF 中文输入法配置
.\.venv\Scripts\python tools\windows\install_tsf.py register

# 4. 打开设置界面或启停/查看守护进程状态
.\.venv\Scripts\python tools\windows\install_tsf.py settings
.\.venv\Scripts\python tools\windows\install_tsf.py start
.\.venv\Scripts\python tools\windows\install_tsf.py status
.\.venv\Scripts\python tools\windows\install_tsf.py stop

# 5. 彻底卸载（关闭服务 + 双架构 regsvr32 /u 反注册，注意如浏览器/QQ已加载 DLL 需重启对应应用释放文件锁）
.\.venv\Scripts\python tools\windows\install_tsf.py unregister
```

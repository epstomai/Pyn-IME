# Windows TSF 输入法外壳架构与安装指南（`ime-v5`）

本文档说明 Input Engine 第 5 期 Windows Text Services Framework（TSF）输入法集成的架构设计、安全边界、按键交互及部署排障方法。

## 1. 双进程分离架构

为保证全系统（含多进程桌面应用与 UWP/AppContainer 应用）共享一份 ~188 MB 模型、统一安全自学习状态，并使核心 crate [ime-core](../crates/ime-core/Cargo.toml) 保持零依赖，TSF 外壳采用瘦客户端 + 本地守护服务架构：

```text
[目标应用 (记事本 / VS Code / Terminal / Edge / QQ)]
        │  COM In-Process (ITfTextInputProcessorEx, x64 / x86 WOW64)
        ▼
[ime_tsf.dll (534.0 KiB) / ime_tsf_x86.dll (439.0 KiB)] <-- 轻量 TSF 外壳 + 横排悬浮候选窗
        │  Win32 Named Pipe (\\.\pipe\InputEngineTSF_v1)
        │  长度前缀 JSON 帧 (u32 LE + UTF-8)
        ▼
[ime-server.exe (768.0 KiB)] <-- 单实例本地引擎守护进程 (Local\InputEngineServerMutex_v1)
        │  共享 Arc<Model> + Option<Arc<Reranker>> + 单份 UserMemory
        │  按 session_id 隔离上下文与 undo 回滚槽位
        ▼
[%LOCALAPPDATA%\InputEngine\user_dict.tsv]  <-- 防抖（10s）与退出时 MoveFileExW 原子落盘
```

- **实测 Named Pipe 端到端延迟**（`ime-server.exe` 挂载 `181.1 MB` KN 三元模型 + 混合精度 `AVX-VNNI + F32 w_down` 神经重排器，40 轮冷启动与连续击键实时查询）：**p50 `1.111 ms`、p95 `2.257 ms`、max `2.885 ms`**（较初期 Phase 5 基准 `p50 2.74 ms / p95 4.21 ms / max 4.90 ms` 延迟减半以上）。

## 2. COM 身份、双架构注册、AppContainer/CUAS 兼容与 Loader Lock 安全

| 项目 | 标识 / 策略 |
|---|---|
| `CLSID_INPUT_ENGINE_TIP` | `{A94E2C1B-7D3F-4E8A-9B12-6C5F8A3D0101}` |
| `GUID_INPUT_ENGINE_PROFILE` | `{A94E2C1B-7D3F-4E8A-9B12-6C5F8A3D0102}` |
| `GUID_DISPLAY_ATTR_INPUT` | `{A94E2C1B-7D3F-4E8A-9B12-6C5F8A3D0103}` |
| 语言标识 / 名称 | `0x0804` (`zh-CN`) / `Input Engine 拼音` |
| 注册类别（8 项） | `GUID_TFCAT_TIP_KEYBOARD`、`IMMERSIVESUPPORT`、`SYSTRAYSUPPORT`、`UIELEMENTENABLED`、`SECUREMODE`、`COMLESS`、`INPUTMODECOMPARTMENT`、`DISPLAYATTRIBUTEPROVIDER` |
| 双架构 COM / WOW64 | 先以 `SysWOW64\regsvr32.exe` 注册 `ime_tsf_x86.dll` 写入 `WOW6432Node\...\CLSID`，再以 `System32\regsvr32.exe` 注册 `ime_tsf.dll`；兼容 32 位 QQ 与旧 Win32/CUAS 应用 |
| Named Pipe SDDL | `D:(A;;GA;;;BA)(A;;GA;;;SY)(A;;GA;;;AU)(A;;GRGW;;;AC)S:(ML;;NW;;;LW)`（允许标准用户、UWP AppContainer 与 Low Mandatory Level 沙箱/低权限客户端连接高权限守护进程） |
| 安装目录 ACL | `icacls <dir> /grant "*S-1-15-2-1:(OI)(CI)(RX)" /T /Q`（授予 `ALL APPLICATION PACKAGES` 读取与加载 `ime_tsf*.dll` 权限） |
| 导出符号封装 | 通过 `ime_tsf.def` 将 `DllCanUnloadNow` / `DllGetClassObject` / `DllRegisterServer` / `DllUnregisterServer` 标记为 `PRIVATE`（x86 stdcall 亦保持无修饰精确导出） |
| `DllMain` 约束 | `DLL_PROCESS_ATTACH` 仅保存 `HINSTANCE`，严禁初始化 COM、创建窗口、启动子进程或连接 Named Pipe |
| EditSession 与光标兼容 | 读上文使用 `TF_ES_READ`；同步锁失败时自动回退 `TF_ES_ASYNCDONTCARE`；无现有组合时以瞬时组合包裹提交以兼容 CUAS/QQ/Electron；TSF `GetTextExt` 缺失时回退 `GetGUIThreadInfo` + `ClientToScreen` |

## 3. 按键与交互映射（`SessionController` 与中途 `Shift` 切英文候选）

状态机实现在无 Win32 依赖的 [session.rs](../crates/ime-ipc/src/session.rs) 与英文前缀补全模块 [english.rs](../crates/ime-ipc/src/english.rs)：

| 场景 | 按键 | 行为说明 |
|---|---|---|
| 空闲（中文模式） | `a–z` | 启动拼音组合并读取光标前最多 `32` 字上文发起转换 |
| 空闲（中文模式） | `, . ? ! ; : ( ) < > \ " ' ^ _` | 直接上屏对应中文全角标点；`"` 与 `'` 自动交替配对 `“”` 与 `‘’` |
| 空闲 | 单击 `Shift`（含 Chromium/Electron 丢 `KeyUp` 恢复） | 切换「中 ↔ 英」模式并通过 `ITfCompartmentEventSink` 与任务栏指示器双向同步，光标处弹出 850 ms `中` / `英` 状态浮标 |
| 空闲（刚上屏中文候选后） | `Backspace` | 键事件放行让宿主应用删除误选文字，同时向 `ime-server` 发送 `Undo` 触发回滚与惩罚门槛 |
| 组合态（中途切换中/英候选） | 单击 `Shift`（含丢 `KeyUp` 恢复） | **保留当前已输入编码不清空**并切换「中 ↔ 英」：切英立即以原编码置顶 + 内置 1,410 词表前缀补全直接生成英文候选（不锁定后续中文输入）；切中转小写并重算中文候选；无任何自动英文识别以免长拼音/错拼被误锁 |
| 组合态（大写驼峰 / 符号 / 小键盘 / CapsLock） | `Shift+A–Z` / `CapsLock A–Z` / `0` / `/` / `RawChar` 符号与小键盘键 | `UpperChar`（如 `NiloToon` 途中按 `Shift+T`）追加进当前编码并临时展示英文候选，退格删回纯小写立即恢复中文（不再放行造成前插错位）；`0`、`/` 与 `RawChar` 直接同步提交「已输入编码+符号」结束组合且不锁定英文模式 |
| 组合态 | `a–z` / `'` / `Backspace` | 追加拼音（或英文字符）/ 音节分隔符 `'` / 回退编码；退空自动取消组合并隐藏候选窗 |
| 组合态 | `Space` / `1–5` | 提交高亮候选或本页第 1–5 项（英文组合态下提交英文且保持英文模式，不写入中文学习） |
| 组合态 | `Enter` / `Escape` | `Enter` 提交原始编码（不记录学习）；`Escape` 清空组合并关闭候选窗 |
| 组合态（横排候选窗） | `Left` / `Right` / `Up` / `Down` / `Tab` / `-` / `=` / `PageUp` / `PageDown` | 横排高亮光标移动或每页 `5` 项翻页 |
| 组合态 | 中文可映射标点（如 `, . ? !`） | 中文组合时自动上屏「当前高亮候选 + 中文全角标点」并仅学习纯汉字；英文组合时直接提交「选中英文 + 半角英文标点」（不学习） |
| 组合 / 空闲 | `Ctrl+*` / `Alt+*` / `Win+*` | 不拦截，直通宿主应用（`Shift+A–Z` 与 `CapsLock` 大写空闲直通，组合态安全追加） |

## 4. 部署、注册与卸载命令（管理员权限）

```powershell
# 0. 安装 32 位目标（仅首次）
rustup target add i686-pc-windows-msvc

# 1. 编译 x64 release 产物（ime_tsf.dll + ime-server.exe）与 x86 32 位 ime_tsf.dll
cargo build --release
cargo build -p ime-tsf --release --target i686-pc-windows-msvc

# 2. 部署 x64 + x86 产物到 C:\Program Files\InputEngine 并赋予 AppContainer 只读/执行 ACL
.\.venv\Scripts\python tools\windows\install_tsf.py deploy

# 3. 双架构注册 COM（WOW64 + x64）与 TSF 中文输入法配置
.\.venv\Scripts\python tools\windows\install_tsf.py register

# 4. 启停或查看守护进程状态（首次打字时 ime_tsf.dll 也会自动无窗口拉起）
.\.venv\Scripts\python tools\windows\install_tsf.py start
.\.venv\Scripts\python tools\windows\install_tsf.py status
.\.venv\Scripts\python tools\windows\install_tsf.py stop

# 5. 彻底卸载（关闭服务 + 双架构 regsvr32 /u 反注册，注意如浏览器/QQ已加载 DLL 需重启对应应用释放文件锁）
.\.venv\Scripts\python tools\windows\install_tsf.py unregister
```

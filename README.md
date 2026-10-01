# Wisp

一个 **2 MB** 的 Windows 划词助手：在任意软件里选中文字，浮出操作条，执行翻译 / 总结 /
解释 / 润色等动作（复制与搜索为可选动作）。

选词能力复用 [Cherry Studio](https://github.com/CherryHQ/cherry-studio) 所用的
`selection-hook` C++ 引擎，**行为与之一致**；外壳与界面为 Tauri 重写。

---

## 为什么这么小

| | Electron 版（本项目的第一阶段） | **本版（Tauri）** |
|---|---|---|
| 安装包 | 97.4 MB | **2.15 MB** |
| 应用 exe | 301 MB（解压后） | ~5 MB |
| 选词引擎 | 内置 Node 原生插件 | **同一个 C++ 引擎**，编成 DLL（0.9 MB） |
| 运行时 | 打包整个 Chromium（~96 MB） | 复用系统 WebView2（Windows 自带） |

体积差 45 倍，**功能与选词效果不变**——关键在于没有重写选词，而是把原 C++ 引擎
编成 DLL 复用（见 [ARCHITECTURE.md](docs/ARCHITECTURE.md)）。

---

## 功能

**划词**
- 全局划词，跨任意程序（基于低层鼠标钩子 + UIAutomation + MSAA + 剪贴板兜底）
- 两种触发：选中即弹出 / 按住 Ctrl
- 应用过滤：黑名单 / 白名单（默认已排除资源管理器、截图与图像视频编辑类软件）

**动作**
- 内置：翻译、总结、解释、润色
- 可选内置（默认关闭，到「动作」页开启）：**复制**（本地，不需模型）、**搜索**（用配置的搜索引擎）
- 自定义动作：任意提示词，可指定模型与思考强度
- 可增删改、排序、启用/停用、各自配色（彩虹色板）

**模型**
- 任意 OpenAI 兼容接口；内置预设的 Base URL **取自 Cherry Studio 官方厂商表**
  （OpenAI / DeepSeek / Moonshot / 智谱 / 通义百炼 / 硅基流动 / MiniMax / OpenRouter /
  OpenCode Go·Zen / Ollama / LM Studio）
- **思考控制**：全局 + 按动作覆盖（关闭 / 轻度 / 标准 / 深度），自动适配
  `reasoning_effort`、`thinking`、`enable_thinking`、`thinking_config`；思考过程可折叠查看
- 流式输出；可复制 / 重试 / 停止
- **自定义请求头**（每行一个 `Name: Value`）；OpenCode 系自动附加 `x-opencode-session`

**结果窗口**
- **Markdown 渲染**：AI 回复按 Markdown 排版（标题 / 列表 / 表格 / 代码块 / 引用）
- **追问**：可继续对话，完整历史随请求发送，多轮上下文不丢
- 思考过程、**原文**均为折叠面板（默认收起，原文带字数提示）
- 流式输出；可停止 / 重试 / 复制结果

**界面**
- 悬浮条：纯文字扁平风格，每个动作独立配色，尺寸精确贴合内容
- 单层毛玻璃（窗口即表面，无内外分层）
- 设置窗口：Windows 原生标题栏，四页（通用 / 动作 / 模型 / 关于）
- 保存为**自动保存**：改完即存，右下角短暂提示「已保存」；回车或关窗也会立即落盘
- 托盘常驻；首次启动（未配置模型时）自动打开设置

---

## 截图

> 待补充：悬浮条 / 结果窗口 / 设置窗口。

---

## 使用

1. 运行安装包（当前用户级安装，免管理员），或直接运行 `selection-assistant.exe`。
2. 首次启动会自动打开设置 →「模型」添加提供商（填 Base URL / API Key / 模型名）→ 点「测试连接」。
3. 在任意软件中选中文字 → 悬浮条出现 → 点击动作查看结果。
4. 想用「复制 / 搜索」→ 设置 →「动作」→ 打开对应开关。
5. 结果窗口底部可**继续追问**（Enter 发送，Shift+Enter 换行）；公式化的回复会自动排版。

配置文件：`%APPDATA%\wisp\settings.json`（界面里改完自动保存，底部状态栏会显示保存状态）。

---

## 构建

### 依赖

| 需要 | 说明 |
|---|---|
| **Rust**（GNU 工具链 `x86_64-pc-windows-gnu`） | 应用本体 |
| **MinGW-w64 gcc** | 编译 C++ 选词 DLL |
| **tauri-cli** | `cargo install tauri-cli --version "^2"` |
| **WebView2** | 系统自带（Win10/11 一般都有） |

> 本项目全程使用**免管理员的便携工具链**（rustup 用户级安装 + MSYS2 便携解压），
> 无需 Visual Studio。详见 [DEVELOPMENT.md](docs/DEVELOPMENT.md)。

### 步骤

```bash
# 1) 编译原生选词 DLL（产物 native/SelectionHook.dll）
cd native
g++ -std=c++17 -O2 -shared -o SelectionHook.dll -Wl,--out-implib,libSelectionHook.a \
    shim.cpp core/engine.cc lib/clipboard.cc lib/keyboard.cc lib/string_pool.cc lib/utils.cc \
    -lole32 -loleaut32 -luiautomationcore -loleacc -luser32 -lshell32 -lpsapi -lgdi32 \
    -static-libgcc -static-libstdc++ -static

# 2) 打包安装程序（产物 src-tauri/target/release/bundle/nsis/Wisp_1.0.0_x64-setup.exe）
cd ../src-tauri
cp ../native/SelectionHook.dll .
cargo tauri build --bundles nsis
```

### 测试

```bash
cd src-tauri
cargo test --test actions_default --test roundtrip   # 纯逻辑测试，不需要 DLL
```

验证「多轮对话 + 流式解析」（在应用进程内运行，绕开测试二进制的 DLL 加载问题）：

```bash
# 终端 A：启动模拟接口（会打印每个请求的 roles）
node scripts/mock-openai.js 8799
# 终端 B
cd src-tauri && WISP_DEBUG=1 ./target/release/selection-assistant.exe --selftest-chat http://127.0.0.1:8799/v1
# 期望：第一条 roles=system,user ；第二条 roles=system,user,assistant,user
```

> `cargo test --lib` 会因测试二进制旁缺少 `SelectionHook.dll` 而报
> `STATUS_ENTRYPOINT_NOT_FOUND`——这是测试环境的链接问题，不是产品缺陷。

### 启动参数

| 参数 | 作用 |
|---|---|
| `--settings` | 启动即打开设置窗口 |
| `--preview` | 在屏幕中部直接显示悬浮条（带示例文字），无需真实划词——用于静态查看 UI |
| `--selftest-chat <baseUrl>` | 在应用进程内跑两轮对话并写入日志，用于验证多轮请求与流式解析 |

### 调试

GUI 子系统下 stdout 不可见。设 `WISP_DEBUG=1` 启动，诊断会写入 `%TEMP%\wisp-debug.log`。

---

## 仓库结构

```
native/            C++ 选词引擎（原版）+ C ABI shim + 验证脚本
  core/ lib/       ← selection-hook 原代码（MIT，0xfullex），未修改
  shim.cpp/.h      ← 新增：N-API 绑定 → 普通 C 接口
src-tauri/         Rust 后端
  src/lib.rs       窗口、托盘、IPC、生命周期
  src/selection.rs 选词 DLL 的 FFI 绑定
  src/ai.rs        OpenAI 兼容客户端 + 思考参数
  src/store.rs     配置持久化 + 迁移
  tests/           单元测试
ui/                前端（无框架，原生 HTML/CSS/JS）
  index.html+*     设置窗口
  toolbar/         悬浮条
  action/          结果窗口
docs/              架构 / 开发过程 / 缺陷记录
```

## 验证工具（native/）

| 工具 | 用途 |
|---|---|
| `test_target.cpp` → `test_target.exe` | 无弹窗的测试文本窗口（记事本会弹保存提示，干扰测试） |
| `test_shim.cpp` → `test_shim.exe` | 直接驱动 DLL：`--inject` 自注入拖拽、`--inject-only` 只注入（让被测应用独占钩子）、`--poll` 轮询同步取词 |
| `drag-select.ps1` | 对测试窗口做真实鼠标拖选 |
| `click-toolbar.ps1` | 点击悬浮条按钮（已声明 DPI 感知） |

---

## 第三方组件

| 组件 | 用途 | 许可 |
|---|---|---|
| `selection-hook`（`native/core`、`native/lib`） | Windows 选词引擎 | MIT © 2025 0xfullex |
| [marked](https://github.com/markedjs/marked)（`ui/action/marked.umd.js`，内置） | 结果窗口的 Markdown 渲染 | MIT |

## 许可

本项目采用 **MIT** 许可，详见 [LICENSE](LICENSE)。

内置的第三方组件保留各自的许可：

| 组件 | 许可 |
|---|---|
| `native/core`、`native/lib`（selection-hook 原版代码） | MIT © 2025 0xfullex |
| `ui/action/marked.umd.js`（marked） | MIT © 2011-2025 Christopher Jeffrey |

## 相关文档

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — 技术栈、架构与关键技术决策

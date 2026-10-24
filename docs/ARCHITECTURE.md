# 架构与技术栈

## 1. 技术栈总览

| 层 | 技术 | 说明 |
|---|---|---|
| **外壳** | **Tauri 2.12** | Rust 宿主 + 系统 WebView2 渲染前端；不打包浏览器内核 |
| **UI** | **原生 HTML / CSS / JS（无框架）** | 三个页面共约 700 行；不引入构建工具与依赖 |
| **后端** | **Rust**（GNU 工具链 `x86_64-pc-windows-gnu`） | 窗口、托盘、IPC、配置、HTTP |
| **选词引擎** | **C++17**（selection-hook 原版）+ **C ABI shim** | 编成 `SelectionHook.dll`，Rust 经 FFI 调用 |
| **HTTP** | `reqwest`（rustls） | OpenAI 兼容接口，含 SSE 流式解析 |
| **窗口材质** | `window-vibrancy` + DWM | Mica / Mica Alt / Acrylic / 无 |
| **打包** | **`cargo tauri build`** | 输出 NSIS 安装包 |

**关键取舍**：选词用 C++（不重写）、前端不用框架（省体积与构建链）、运行时用系统
WebView2 而不是随包分发浏览器内核——这三点决定了体积量级。

---

## 2. 体积为什么能这么小

| 项 | 值 |
|---|---|
| 安装包 | 2.2 MB |
| 应用 exe | ~5 MB（Rust 静态链接，无额外运行时 DLL） |
| 选词 DLL | 0.9 MB |
| 运行时 | 系统 WebView2（Windows 自带） |

两点原因：

1. **不打包浏览器内核**。浏览器引擎是桌面应用体积的主要来源；Tauri 复用系统自带的
   WebView2，因此省下了这一整块。
2. **不重写选词**。选词引擎来自既有的 C++ 实现，编成 0.9 MB 的 DLL 直接复用
   （见下一节的取舍说明）。

---

## 3. 选词引擎：为什么复用而不是重写

Windows 上跨程序取"当前选中的文字"是一组脏活：

1. **UIAutomation**（现代应用，如 Chromium、Office）
2. **MSAA / IAccessible**（传统应用）
3. **剪贴板兜底**（发送 Ctrl+C / Ctrl+Insert 再读回，需备份/恢复原剪贴板）

再加上手势识别：区分"拖动选择 / 双击选词 / Shift+点击"，要处理窗口是否移动、
鼠标是否 I-beam 光标（防止把拖拽窗口误判为选词）等。这些**兼容性细节**是靠长期打磨得到的，
重写必然引入回归。

因此 `native/core`、`native/lib` **原样保留** selection-hook 的源码（MIT），
只新增一个 C ABI 层替换掉原来的 N-API 绑定：

```
原来（Node 原生模块）              现在（普通 DLL）
─────────────────────            ─────────────────────
selection_hook.cc                shim.cpp        ← 替换 N-API 部分
  ├─ Napi::ObjectWrap               ├─ C 回调函数指针
  ├─ Napi::ThreadSafeFunction       ├─ std::deque + 派发线程
  └─ Napi worker                    └─ MTA worker（同样复用 SelectionEngine）
core/engine.cc  ← 未改动，取词管线
lib/*.cc        ← 未改动，键盘映射 / 剪贴板 / 字符串池
```

**移植的关键点**：

| 原实现 | 替代方案 |
|---|---|
| `Napi::ObjectWrap` 类 | 普通 C++ 类 + 不透明句柄 |
| `Napi::ThreadSafeFunction` | `std::deque<QueuedEvent>` + 派发线程 + 回调 |
| `Napi::Object` 结果对象 | `SSelectionOut` POD 结构（字符串为 UTF-8 指针） |
| worker 的结果 TSFN | 直接回调（同一 MTA 线程内） |

**线程模型（保持一致）**：

```
低层钩子线程 (WH_MOUSE_LL / WH_KEYBOARD_LL)
    │  只做快照，绝不阻塞（钩子回调超时会触发 Windows 静默卸载钩子）
    ▼
std::deque 事件队列
    ▼
派发线程 ── 手势判定（拖动 / 双击 / Shift）→ 决定是否取词
    ▼
MTA worker 线程 ── SelectionEngine::GetSelectedText
    │   UIA → MSAA → 剪贴板兜底
    ▼
C 回调 → Rust 侧 on_selection → Tauri 事件 → 前端
```

---

## 4. 进程内模块划分

```
src-tauri/src/
  main.rs        入口（GUI 子系统 + 早期 DPI 感知）
  lib.rs         窗口 / 托盘 / IPC / 生命周期
  selection.rs   选词 DLL 的运行时加载与事件桥
  ai.rs          OpenAI 兼容客户端 + 思考参数 + 请求头
  chat.rs        ChatMessage（无依赖，供面板状态机与测试使用）
  panels.rs      每个结果窗口的对话状态机（世代号 / 回滚）
  sse.rs         SSE 分块解析（字节级，保护多字节字符）
  store.rs       配置持久化、默认值、迁移
```

### 结果窗口的多开与对话状态

每个结果窗口对应 `panels::Panels` 里的一份状态。两个约束决定了它的设计：

1. **每个请求带世代号**。停止、重试、新提问都会推进世代；HTTP 响应抵达时若世代不匹配
   就丢弃。没有这一条，"停止"之后迟到的完整回复仍会被贴到界面上。
2. **每轮开始记录快照**。失败或停止回滚到该快照，而不是"弹出最后一条 user 消息"——
   首轮的提问就是选中文本本身，弹出它会丢掉上下文。

这两点都由 `tests/panels.rs` 覆盖（纯逻辑，无 Tauri 依赖）。

### 原生 DLL 的加载方式

`SelectionHook.dll` 通过 `LoadLibraryW` **运行时加载**（`selection.rs` 的 `Api`），
不在链接期导入。原因：以导入库方式链接时，`cargo test --lib` 的测试二进制位于
`target/<profile>/deps/`，那里没有 DLL，Windows 会在执行任何测试之前终止进程
（`STATUS_ENTRYPOINT_NOT_FOUND`），看起来像代码缺陷。运行时加载还顺带避免了
导入库把 static-libgcc 的 `_Unwind_Resume` 带进链接、需要 `--allow-multiple-definition`
的问题，并让"DLL 缺失/版本不符"变成一条可读错误。

### 窗口模型

| 窗口 | label | 置顶 | 尺寸 |
|---|---|---|---|
| 悬浮条 | `toolbar` | ✅ | 由内容决定（宽度 = 内容宽，高度固定） |
| 结果窗口 | `action-1`、`action-2`… | ❌ | 可缩放，记住上次尺寸 |
| 设置窗口 | `settings` | ❌ | 固定 |

只有悬浮条置顶——它必须浮在用户选词的软件之上。**结果窗口不置顶**：它是被阅读的内容，
置顶会遮住其他结果窗口和用户正在用的软件，也让多开变得没有意义（这是实际踩过的坑）。

结果窗口按需创建、关闭即销毁（`Destroyed` 时释放该面板的对话与 provider 快照），
因此可以同时打开任意多个结果，互不影响。窗口 label 是面板的身份，所有面板命令
都从 `WebviewWindow` 参数取**调用者自己的** label。

### 窗口尺寸：三种比例空间

这是最容易出错的地方，必须区分：

| 关系 | 来源 | 实测值 |
|---|---|---|
| 逻辑 → 物理 | Tauri `scale_factor` | 1.5 |
| 物理 → CSS | webview `devicePixelRatio` | 1.8 |
| **逻辑 → CSS** | 推导值 | **0.833** |

**因为逻辑单位 ≠ CSS 单位**，把 CSS 尺寸直接当 `LogicalSize` 传进去，视口只有预期的 83%
（请求 304 CSS 实际只得 254），内容被压缩、文字被裁。

**正确公式**（`lib.rs::css_to_logical`）：

```rust
logical = css * devicePixelRatio / scale_factor
```

并且**尺寸与位置都从公式推导，绝不回读窗口**——`set_size` 是异步的，紧接着
`outer_size()` 拿到的是旧值（曾把悬浮条定位到屏幕外几百像素处）。

### 材质（DWM）

三种材质采样对象不同，这是观感差异的根源：

| 材质 | DWM 常量 | 采样 | 观感 |
|---|---|---|---|
| Mica | `DWMSBT_MAINWINDOW` | **桌面壁纸** | 几乎不模糊，浮在别的程序上偏"平" |
| Mica Alt | `DWMSBT_TABBEDWINDOW` | 同上，对比度略高 | — |
| Acrylic | `DWMSBT_TRANSIENTWINDOW` | **窗口背后的一切** | 实时模糊，真毛玻璃 |
| 无 | `DWMSBT_DISABLE` | — | 纯色 |

设计约束：**内容必须铺满窗口，且只画一层表面**。CSS 的 `backdrop-filter` 无法模糊窗口背后的
桌面，所以窗外阴影和外边框都会被窗口矩形裁掉或露出裸材质——这曾导致"透明大框里套白卡"的畸形观感。

---

## 5. 数据流

```
[用户在任意程序选中文字]
        │
        ▼
C++ 钩子线程 ──快照──▶ 队列 ──▶ 派发线程（手势判定）
        │                              │
        │                              ▼
        │                     MTA worker 取词
        │                              │
        ▼                              ▼
   鼠标/键盘事件              C 回调 on_selection
   （用于"点外部隐藏"）                │
        │                              ▼
        │                    Rust: 转成 SelectionPayload
        │                              │
        ▼                              ▼
   Tauri 事件 global-pointer     Tauri 事件 selection
        │                              │
        ▼                              ▼
   悬浮条隐藏逻辑              悬浮条定位 + 显示
                                       │
                              （用户点击动作）
                                       ▼
                            run_action → 结果窗口 → chat_stream
                                       │
                                       ▼
                            SSE 增量 → action:chunk 事件 → 前端渲染
```

---

## 6. 配置与迁移

`%APPDATA%\wisp\settings.json`，由 `store.rs` 管理：

- 内存缓存（`Mutex<Option<Settings>>`）+ 原子写盘
- **`merge()` 绝不静默重置**：反序列化失败时保留原配置并记日志
  （早期用 `unwrap_or_default()`，一旦形状不符就会把用户所有配置清空）
- **`schemaVersion` 一次性迁移**：`copy` / `search` 从"默认开启"改为"可选（默认关闭）"时，
  用版本号保证只执行一次——否则用户手动开启后会被每次启动改回去
- 写盘函数 `write_to_disk()` 不加锁，供持锁者（迁移路径）安全调用
  （`std::sync::Mutex` 不可重入，早期在 `load()` 里调 `save()` 造成**启动死锁**）

---

## 7. 打包与体积

```
cargo tauri build --bundles nsis
```

| 项 | 值 |
|---|---|
| 安装包 | 2.15 MB |
| exe | ~5 MB（Rust 静态链接，无额外运行时 DLL） |
| SelectionHook.dll | 0.9 MB |
| WebView2Loader.dll | 0.16 MB |

**不做便携版**（用户明确要求）。安装为**当前用户级**，免管理员。

### 为什么没有控制台窗口

GNU 工具链默认把二进制链成 console 子系统（会弹黑窗）。修法：

```rust
// main.rs
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
```

并在 `build.rs` 追加 `-Wl,--subsystem,windows`。副作用：GUI 子系统下 stdout 不可见，
因此诊断改为写文件（`WISP_DEBUG=1` → `%TEMP%\wisp-debug.log`）。

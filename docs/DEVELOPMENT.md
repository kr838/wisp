# 开发过程记录

本文按时间顺序记录实际做过什么、遇到什么问题、怎么解决的。踩坑部分写得很直白，
因为那些坑才是这个项目真正的难点。

---

## 阶段 0：需求确认

用户初始需求（原话在对话中被截断，逐步澄清后）：
1. 只保留 Cherry Studio 的**划词助手**功能，其余全部不要
2. 打包成 Windows 可安装版本
3. 安装包尽可能小
4. 能控制模型"思考"（thinking）开关与强度
5. 提示词可添加与管理
6. UI 要扁平简约，不要可爱风图标

---

## 阶段 1：摸清 Cherry Studio 的划词实现

**做法**：克隆 Cherry Studio 源码（`git clone` 首次失败 → 改用 `codeload` tarball）。

**发现**：划词不是一个独立模块，而是深度嵌入整个 Electron 客户端：

```
src/main/services/selection/SelectionService.ts   4000+ 行
src/main/services/selection/selectionConfig.ts
src/renderer/windows/selection/toolbar/          悬浮条
src/renderer/windows/selection/action/           结果窗口
src/renderer/pages/settings/SelectionAssistantSettings/
src/main/core/window/windowRegistry.ts            两个窗口的配置
```

**关键发现**：它依赖 `selection-hook`（npm 包，`package.json` 里有），
底层是 C++ 写的 Windows 原生模块。**这是整个项目最重要的一个发现**——
意味着选词能力可以被独立复用，不必重写。

从源码里抄到的关键行为约束（后来都照做了）：
- 工具栏窗口 **`hasShadow: false`**（OS 阴影会带来隐形边框吃掉内容宽度）
- 工具栏窗口 **`focusable: false`**（Windows 上防止抢键盘焦点）
- 选中即弹出模式下，会自动合并内置黑名单（`SELECTION_PREDEFINED_BLACKLIST`）
- 结果窗口 `frame: false` + `transparent: true` + 不透明 popover 卡片

---

## 阶段 2：Electron 版（第一阶段产物）

用 Cherry 的同款 `selection-hook` 独立重建了一个 Electron 应用。

**验证方法（当时的做法）**：
1. 写 `test_target.cpp` —— 一个无弹窗的测试文本窗口
   （起初用记事本，但它关窗时弹"是否保存"，既烦人又抢焦点）
2. 写 `e2e-select.ps1` 合成鼠标拖拽
3. 读 `%TEMP%` 下的 JSONL 事件日志

**踩坑 1：合成拖拽起点的光标不是 I-beam**

拖拽起点落在编辑框边框上时，鼠标光标是箭头而非 I-beam，剪贴板兜底路径
（`ShouldProcessViaClipboard` 的光标启发式）会拒绝它。

这不是 bug，是 Cherry 刻意的防误触发设计。修法是把起点移进文本区。

**踩坑 2：在钩子回调里打日志 → 钩子被静默卸载**

调试时在 `WH_MOUSE_LL` 回调里 `fprintf`，结果只能收到第一个事件。
原因是 Windows 有 `LowLevelHooksTimeout`（默认约 300 ms），
**回调超时会被系统静默卸载钩子**，连错误都不报。

修法：钩子回调里**零 I/O**，只做原子计数；事件快照存环形缓冲区，
由宿主线程定期读取（`sh_stats()` / `sh_dump_events()`）。

**踩坑 3：DPI 坐标空间**

`selection-hook` 返回**物理像素**，而合成输入的 PowerShell 脚本若不是 DPI 感知，
`SetCursorPos` 的坐标会被系统虚拟化。修法是给测试脚本加
`SetProcessDpiAwarenessContext(-4)`。

**成果**：97.4 MB 安装包，功能完整（含思考控制）。

---

## 阶段 3：为什么 100 MB —— 量化分析

用户问"为什么这么小的功能有 100MB"，于是做了分解：

```
234.3 MB  SelectionAssistant.exe   ← 整个 Chromium + V8 + Blink
 24.6 MB  dxcompiler.dll
 11.9 MB  resources.pak
 10.4 MB  icudtl.dat
 49.0 MB  locales/（55 个语言包，Windows 上完全用不到）
 20.0 MB  LICENSES.chromium.html
────────────────────────────
~301 MB 解压后      应用自身代码：860 KB（0.3%）
```

**结论**：98 MB 是"用网页技术写桌面应用"的门票钱，与功能多少无关。

**第一轮瘦身**（仍留在 Electron 版）：
- `electronLanguages: [zh-CN, en-US]` → locales 49 MB → 1.2 MB
- 删除 `LICENSES.chromium.html`（20 MB）
- 只保留 `selection-hook` 的 win32-x64 预编译二进制
- `compression: maximum`
- 结果：97.4 MB（不能再降，剩下的都是 Chromium 本体）

---

## 阶段 4：工具链 —— 全程免管理员

**这是最耗时的一个阶段。** 机器上没有 Rust、没有 MSVC、没有 gcc，且无管理员权限。

| 组件 | 方案 | 遇到的问题 |
|---|---|---|
| **Rust** | rustup 用户级安装（`--no-modify-path`） | 见下 |
| **C++ 编译器** | MSYS2 便携解压（`.sfx.exe -y -o`） | 见下 |
| **cargo 镜像** | USTC sparse 索引 | crates.io 直连超时 |

**踩坑 4：GitHub 被墙，官方源不可用**

`static.rust-lang.org` / `github.com` 直连超时或 302 后失败。
改用国内镜像：

```bash
export RUSTUP_DIST_SERVER=https://mirrors.ustc.edu.cn/rust-static
export RUSTUP_UPDATE_ROOT=https://mirrors.ustc.edu.cn/rust-static/rustup
```
实测 USTC 约 600 KB/s，可用。

**踩坑 5：rustup 参数写错导致工具链损坏**

`rustup default stable --profile minimal` 报 `unexpected argument '--profile'`，
但已经装了一半 → 工具链缺 `manifest` → `rustc --version` 报错。
修法：`rustup toolchain uninstall` 后重装，`--profile` 只用于 `toolchain install`。

**踩坑 6：MSYS2 的 GPG keyring 不可写（无管理员）**

`pacman -Sy` 报 `keyring is not writable` / `invalid or corrupted database (PGP signature)`。
修法：把 `pacman.conf` 的 `SigLevel` 改为 `Never`（个人开发机上可接受）。

**踩坑 7：MSYS2 解压后多一层嵌套目录**

`msys2.sfx.exe -y -o...` 解出 `msys64/msys64/...`，需要手工拍平。

**踩坑 8：MinGW 链接 UIAutomation 时 GUID 未定义**

`undefined reference to CLSID_CUIAutomation`。原因是 `UIAutomation.h` 里是
`DEFINE_GUID`（仅声明）。修法：在包含顺序最前面加

```cpp
#include <initguid.h>
```

**踩坑 9：libgcc 符号冲突**

DLL 的导入库内嵌静态 libgcc 的 `_Unwind_Resume`，与 Rust 自己链接的那份冲突。
修法：`-Wl,--allow-multiple-definition`（两者实现等价，重复无害）。

**踩坑 10：cdylib 导致符号导出爆炸**

`crate-type = ["staticlib", "cdylib", "rlib"]` 触发
`export ordinal too large: 69630`。这是桌面二进制，只需要 `rlib`。

**最终工具链**：Rust 1.98.1 (GNU) + MinGW-w64 gcc 16.2.0，全部便携，无需管理员。

---

## 阶段 5：C ABI 移植

把 `selection_hook.cc`（1819 行，N-API 绑定）替换为 `shim.cpp`（约 1070 行，普通 C 接口）。

**移植映射**：

| 原实现 | 替代 |
|---|---|
| `Napi::ObjectWrap<SelectionHook>` | 普通类 + `void*` 句柄 |
| `Napi::ThreadSafeFunction`（3 个） | `std::deque` + 派发线程 + 函数指针回调 |
| `Napi::Object`（结果） | `SSelectionOut` POD（UTF-8 指针，仅回调期间有效） |
| worker 结果走 TSFN | 直接调用回调（同线程） |

**未改动的部分**：`core/engine.cc`（取词管线）、`lib/*`（键盘映射、剪贴板、字符串池）。
`core/worker.cc` 被替换（原版含 N-API），但 `SelectionEngine` 的调用方式不变。

**验证**：
- `test_shim.exe --inject`：自注入拖拽，确认拿到 47 字符
- 后来又从一个真实第三方应用（Chromium）选中 170 字符（走 UIA，`method=1`）

---

## 阶段 6：Rust FFI → Tauri 应用

**顺序**：先写最小验证（`ffi_test`，Rust 直接调 DLL 拿到选中文本），
确认 FFI 边界无误后，才铺 Tauri 应用。

**验证**：`RUST_SYNC textLen=68` —— Rust 侧成功拿到选中文本。

**Tauri 部分**：
- `src/selection.rs`：FFI + 事件桥
- `src/store.rs`：配置
- `src/ai.rs`：OpenAI 兼容客户端 + 思考参数
- `src/lib.rs`：窗口 / 托盘 / IPC
- `ui/`：三个页面

**踩坑 11：Tauri 2 的 API 细节**
- 需要 `use tauri::Listener` 才能调 `.listen()`
- `SelectionPayload` 需要 `Deserialize`（因为要从事件载荷反序列化）
- 托盘菜单项需要 `&dyn IsMenuItem<Wry>` 数组

**踩坑 12：单实例缺失 → 托盘图标重复**

用户报告"点应用图标没反应，一看侧边栏开了一堆隐藏图标"。
原因是**没有单实例保护**，每点一次就启动一个新进程。
修法：`tauri-plugin-single-instance`（必须是第一个注册的插件）。

**踩坑 13：悬浮条抢键盘焦点**

用户报告"只要这个窗口出现，我就无法打字，也无法 Ctrl+C / Ctrl+V"。
根因是我调用了 `set_focus()`。修法：
- `focused(false)` 创建（`WS_EX_NOACTIVATE`，永不夺焦点）
- 取消 `set_focus()`
- 关闭改用全局鼠标钩子判断"点击在条外"（非激活窗口不会触发 blur）

**踩坑 14：设置窗口置顶导致切不出去**

用户报告"设置窗口总是浮在最上面，必须最小化才能看别的应用"。
根因是我为了"防止被结果窗口挡住"给它设了 `always_on_top(true)`。
修法：设置窗口改普通窗口；打开设置时先隐藏浮动窗口。

---

## 阶段 7：UI 反复打磨（耗时最长）

这一阶段的教训最深：**我在一个不稳定的桌面环境里反复跑 GUI，多次基于噪声下结论。**

### 悬浮条：三个真实缺陷

**缺陷 A：CSS 尺寸 ≠ 窗口单位（最根本）**

实测出三个互不相等的比例：

```
逻辑 → 物理 = 1.5   (scale_factor)
物理 → CSS  = 1.8   (devicePixelRatio)
逻辑 → CSS  = 0.833 ← 把 CSS 当逻辑尺寸传进去，视口只有 83%
```

表现：请求 304 CSS 宽，实际视口 254，内容被压缩、最后一个按钮被裁。

**诊断走过的弯路**：我先怀疑是"测量→resize→再测量"的反馈环，试了
自适应校正（会振荡）、一次性标定（存盘但没生效）、死区（解决了 +1px 漂移，
但不是主因）。最后靠**一次完整日志**（而不是逐条 grep）才看清
`inner=750 / outer=1344 / scale=1.5` 三者不自洽。

**缺陷 B：回读过期尺寸 → 定位错位**

`set_size` 后立刻 `outer_size()`，但 resize 是异步的，读到的是**上一次**的值
（日志里明确看到：请求 304 时读到 1620），导致定位偏差数百像素。

**缺陷 C：窗口比胶囊宽 → 露出裸材质**

窗口 = 内容 + 8px，而胶囊是 `max-content` 并居中 → 左右各露约 4px 原始材质。
修法：胶囊改为 `width/height: 100%` 铺满窗口。

**顺带修掉的**：
- 外层阴影画在窗口外 → 全被裁掉（改用内嵌描边 + 顶部高光表达层次）
- 圆角不一致（DWM 约 8px vs CSS 10px）→ 统一 8px
- `scrollWidth` 会跟随窗口 → `+1px` 自增长循环（加 2px 死区）

### 结果窗口：毛玻璃"分层"

用户描述非常准确："下面是一层透明毛玻璃但没内容，上面是很普通的白色圆角框，
毛玻璃比白框大一圈，像一个很大的透明白边。"

**根因（代码级）**：
```rust
// build_action
.transparent(true)   // 整个窗口透明
.shadow(true)        // Windows 在透明矩形外围画阴影 ← 用户看到的"外层"
// 且从未调用 apply_frosted_glass → 根本没有毛玻璃
```
```css
/* 可见的是这一层 */
.card { margin: 7px; background: #fff; }
```
即：透明窗口 + OS 阴影 + 内缩白卡 = 三层叠加的畸形结果。

**修法**：窗口本身铺材质（Mica/亚克力 + DWM 圆角），`.card` 去掉边距/背景/边框/阴影，
**只剩一层**。

### 设置窗口：两套标题栏

用户截图显示顶部有**两个**"划词助手"标题。
根因：`show_settings` 没设 `decorations(false)`（Windows 原生标题栏在），
而 `ui/index.html` 里又画了一套自绘的。

**修法**：改用 Windows 原生标题栏，删掉自绘的。

### 材质做成可调

用户："虽然不是亚克力，但是也可以了，这是什么材质啊"
→ 答案是 **Mica**（我的代码是"先试 Mica，失败才退 Acrylic"，Win11 必然成功）。

按用户要求做成可调：材质（Mica / Mica Alt / Acrylic / 无）+ 明暗 + 不透明度，
**改完立即生效**。验证方式是读 `DWMWA_SYSTEMBACKDROP_TYPE`：
`mica=2`、`acrylic=3`、`none=1`，与预期一致。

---

## 阶段 8：模型配置相关

### 坑：输入被静默丢弃

用户："模型根本设置不了"。

**根因：陈旧对象引用。**

```js
;(S.providers || []).forEach((p) => {
  i.addEventListener('input', () => { p[key] = i.value; saveNow({ providers: S.providers }) })
})
listen('settings-changed', (e) => { S = e.payload })   // ← S 被替换成全新对象
```

保存触发广播 → `S` 变成新对象 → 但闭包里的 `p` 仍是旧对象 →
后续输入改了旧对象、发出的却是新数组 → **输入被覆盖**。

表现正是："第一个字段能存，后面填的全丢"（用户配置里 `apiBase` 有值、`name/apiKey/model` 全空）。

**修法**：每次编辑按 id 取当前对象：

```js
const cur = () => (S.providers || []).find((x) => x.id === p.id) || p
```

**验证**：用 Node 精确复现旧/新逻辑
（修复前 `{"apiKey":"sk-1","model":""}`，修复后 `{"apiKey":"sk-1","model":"gpt-5"}`）。

### 坑：`merge()` 静默重置

```rust
let next: Settings = serde_json::from_value(cur).unwrap_or_default();  // 危险
```
一旦形状不符，**整个配置被重置为默认值**（所有模型消失）。
改为：失败时保留原配置并记日志。

### 坑：启动死锁

写迁移时在 `load()` 持锁的情况下调用 `save()`，而 `save()` 又要拿同一把锁。
`std::sync::Mutex` 不可重入 → **首次读取配置就死锁**，应用卡死。

现象很有迷惑性：进程在跑、无日志、无迁移生效。

**修法**：拆出不加锁的 `write_to_disk()` 供持锁者调用。

### OpenCode 的 `x-opencode-session`

用户报 `HTTP 400 MissingSessionID`。

**用用户提供的 Key 直连验证**：
- 不带头 → `400 MissingSessionID`
- 带 `x-opencode-session: wisp-test-...` → **`200`**，正常返回

从 Cherry 源码找到它的做法（`config.ts:439`，`conversationHeader: 'x-opencode-session'`），
于是照做：地址含 `opencode.ai` 时自动附加该头（值为每次安装固定的会话 ID）。

### 厂商配置取自 Cherry 官方表

用户提醒："Cherry Studio 对各个厂家的请求头都是配置好的，你直接使用就好了"。
于是从 `packages/provider-registry/data/providers.json`（56 个厂商）提取官方 Base URL，
并顺带修正了两处手写错误：
- DeepSeek 官方是 `https://api.deepseek.com`（我原来写成 `/v1`）
- 通义是 `dashscope.aliyuncs.com/compatible-mode/v1`（原来没有）

---

## 阶段 9：收尾（文档 / 注释 / 版本控制）

- 补齐模块级文档注释（尤其 `selection.rs` 的 FFI 线程契约）
- 写 README / ARCHITECTURE / DEVELOPMENT / BUGS
- 初始化 git，按阶段提交

---

## 复盘：如果重做一次

1. **逻辑改动先写测试，再跑 GUI。** 后期我改用单元测试（6 个）后，
   验证速度和可靠性都明显提升；早期在 GUI 上反复试错浪费了大量轮次。
2. **日志要"一次读全"，不要逐条 grep。** 悬浮条尺寸问题的三个数字
   （inner / outer / scale）分开看都是"正常"的，放一起才暴露不自洽。
3. **不要在自己也无法稳定观测的环境里下结论。** 那些"抓不到窗口/事件"
   很多时候是环境噪声（残留进程、截图坐标空间），不是产品缺陷。
4. **优先复用经过验证的复杂子系统。** 选词复用 C++ 引擎是最大的一次正确决策；
   反过来，我自己重写 UI 的几个周边（尺寸/材质/焦点）恰恰是出问题最多的地方。

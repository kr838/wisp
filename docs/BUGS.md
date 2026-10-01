# 缺陷清单与根因分析

按类别整理。每条都记录了**现象 → 根因 → 修法 → 验证**，便于以后回归。

---

## A. 选词引擎（C++ / DLL）

### A1. 添加调试日志后钩子失效

- **现象**：加了 `fprintf` 调试后，只能收到第一个鼠标事件，之后完全静默
- **根因**：在 `WH_MOUSE_LL` 回调里做同步 I/O，超过 Windows 的
  `LowLevelHooksTimeout`（默认约 300 ms），**系统静默卸载钩子**且不报错
- **修法**：钩子回调内**零 I/O**，只做原子计数并把快照写入环形缓冲区；
  由宿主线程通过 `sh_stats()` / `sh_dump_events()` 读取
- **验证**：无调试时正常取词；`SH_DEBUG=1` 也不再影响钩子存活

### A2. 合成拖拽取不到词（光标非 I-beam）

- **现象**：拖动后没有取词事件
- **根因**：拖拽起点落在编辑框边框上，鼠标光标是箭头而非 I-beam，
  剪贴板兜底路径的光标启发式拒绝处理（**这是刻意的防误触发设计，不是 bug**）
- **修法**：测试时把起点移入文本区（`r.left + 50`，`r.top + 91`）
- **验证**：`cursors down=0x10005 up=0x10005`（I-beam）→ 取词成功

### A3. 移到了错误的窗口取词

- **现象**：自动模式下 `GetSelectedText ok=0`
- **根因**：一度怀疑是前台窗口不对；实测日志显示
  `foreground=... class=Notepad same=1`，排除；真正原因见 A2
- **经验**：日志要打出窗口类名与 `same` 标志，否则只能猜

### A4. 误用记事本做测试目标

- **现象**：测试过程中频繁弹"是否保存"对话框，且抢焦点
- **根因**：测试目标选得不合适（记事本关窗会提示保存）
- **修法**：写 `test_target.cpp`，一个无弹窗、预填文本的 Win32 编辑框窗口

---

## B. 工具链与构建

### B1. GitHub 直连失败

- **现象**：`git clone` 报 `RPC failed; curl 56 Recv failure: Connection was reset`
- **分析**：`github.com` / `static.rust-lang.org` 在本网络下不可直连
- **修法**：`codeload.github.com` 取 tarball；Rust 用 USTC 镜像；cargo 用 USTC sparse 索引

### B2. rustup 工具链损坏

- **现象**：`rustup default stable --profile minimal` 报 `unexpected argument '--profile'`，
  之后 `rustc --version` 报 `missing manifest`；连 GNU 工具链也装不完整
- **根因**：参数用错（`--profile` 只属于 `toolchain install`），且中断导致半装状态
- **修法**：`toolchain uninstall` 后重装

### B3. MSYS2 pacman 签名校验失败（无管理员）

- **现象**：`keyring is not writable` / `invalid or corrupted database (PGP signature)`
- **根因**：无管理员权限，无法更新 keyring
- **修法**：`pacman.conf` 里 `SigLevel = Never`（个人开发机可接受）
- **风险**：降低了包完整性校验，不应在需要严格安全的环境照搬

### B4. MinGW 链接 UIAutomation：GUID 未定义

- **现象**：`undefined reference to 'CLSID_CUIAutomation'`
- **根因**：`UIAutomation.h` 用 `DEFINE_GUID`（只声明，不定义）
- **修法**：在包含顺序最前加 `#include <initguid.h>`

### B5. libgcc 符号重复定义

- **现象**：`multiple definition of '_Unwind_Resume'`
- **根因**：DLL 导入库内嵌静态 libgcc，与 Rust 链接的那份重复
- **修法**：`-Wl,--allow-multiple-definition`（实现等价，重复无害）

### B6. 打包失败：export ordinal too large

- **现象**：`error: export ordinal too large: 69630`
- **根因**：`crate-type` 含 `cdylib`，触发海量符号导出
- **修法**：只保留 `rlib`（这是桌面可执行文件，不需要 cdylib）

### B7. 启动弹出黑色控制台窗口

- **现象**：运行 exe 会带一个命令行窗口
- **根因**：GNU 工具链默认链成 **console 子系统**（`Subsystem 00000003 CUI`）
- **修法**：`main.rs` 加 `#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]`，
  并在 `build.rs` 追加 `-Wl,--subsystem,windows`
- **验证**：`objdump -p` 显示 `Subsystem 00000002 (Windows GUI)`
- **副作用**：GUI 子系统下 stdout 不可见 → 诊断改为写文件

### B8. 打包时文件被占用

- **现象**：`failed to remove file ...exe` / `Device or resource busy`
- **根因**：应用实例仍在运行；后期还遇到**火绒**实时防护扫描刚生成的 exe
- **修法**：先杀进程；火绒的情况无法编程解决，需用户加信任目录或等待扫描结束
  （**不要擅自关闭用户的杀毒软件**）

---

## C. 窗口与生命周期

### C1. 托盘图标重复堆积

- **现象**：点应用图标没反应，托盘/侧边栏出现多个隐藏图标
- **根因**：**没有单实例保护**，每次点击都启动新进程
- **修法**：`tauri-plugin-single-instance`（且必须最先注册）

### C2. 悬浮条抢走键盘焦点

- **现象**：悬浮条出现后无法打字，Ctrl+C / Ctrl+V 失效
- **根因**：我调用了 `set_focus()`
- **修法**：
  - `focused(false)` 创建窗口（`WS_EX_NOACTIVATE`，能收鼠标点击但不夺焦点）
  - 去掉 `set_focus()`
  - 关闭改用全局鼠标钩子判断"点击在条外"（非激活窗口不会产生 blur 事件）

### C3. 设置窗口永远置顶

- **现象**：设置窗口总压在其他应用之上，必须最小化才能切走
- **根因**：我为了"防止被结果窗口挡住"设了 `always_on_top(true)`
- **修法**：设置窗口改为**普通窗口**；打开设置时先隐藏浮动窗口来避免遮挡
- **验证**：读 `WS_EX_TOPMOST` 标志 —— 悬浮条/结果窗口 `True`，设置窗口 `False`

### C4. 设置窗口出现两套标题栏

- **现象**：窗口顶部有两个标题（一个原生、一个自绘）
- **根因**：`show_settings` 未设 `decorations(false)`（原生标题栏在），
  同时 `ui/index.html` 又画了一套自绘的
- **修法**：改用 Windows 原生标题栏，删除自绘 titlebar（HTML/CSS/JS 三处）

### C5. 设置页出现 512px 巨型图标

- **现象**：设置窗口被一个巨大的紫色图标撑满
- **根因**：上一次改动的 `ui/index.html` **写入静默失败**（工具报
  "File has been modified since read"，即未落盘），于是自绘 `<img class="tb-logo">`
  仍在，而配套 CSS（`width:20px`）已被删除 → 原图按 512px 渲染
- **修法**：重写 `index.html`，并做静态校验（引用资源、DOM id 对应、无残留按钮）
- **教训**：**文件写入失败必须复验内容**，不能只看工具返回

---

## D. 悬浮条尺寸与视觉

### D1.（根本）CSS 尺寸被当逻辑单位使用

- **现象**：悬浮条比内容窄约 17%，文字被压缩、末尾按钮被裁
- **根因**：三个比例互不相等，且我混用了它们

  | 关系 | 值 |
  |---|---|
  | 逻辑 → 物理（`scale_factor`） | 1.5 |
  | 物理 → CSS（`devicePixelRatio`） | 1.8 |
  | 逻辑 → CSS | **0.833** |

  把 CSS 尺寸直接传给 `LogicalSize`，视口只剩 83%（请求 304 → 实际 254）
- **修法**：`physical = CSS * devicePixelRatio`；集中到 `css_to_logical()`
- **验证**：请求 304 CSS → 窗口 547 物理 → 视口 304 CSS（三者自洽）

### D2. 回读窗口尺寸拿到过期值

- **现象**：悬浮条水平定位偏差数百像素
- **根因**：`set_size` 之后立刻 `outer_size()`；resize 异步生效，
  读到的是**上一次**的尺寸（日志：请求 304 时读到 1620）
- **修法**：尺寸与位置**全部由公式推导**，绝不回读

### D3. 悬浮条四周露出裸材质（"透明外框"观感）

- **现象**：胶囊外面有一圈半透明条
- **根因**：窗口宽 = 内容 + 8px，而胶囊 `max-content` 居中 → 左右各露约 4px 材质
- **修法**：胶囊 `width/height: 100%` **铺满窗口**，窗口尺寸精确等于内容

### D4. 外阴影被窗口裁掉

- **现象**：没有立体感，边缘发脏
- **根因**：窗口高度 == 胶囊高度，而阴影画在窗口外 → 完全不可见
- **修法**：改用**内嵌**发丝描边 + 顶部高光表达层次（能在窗口内画出来）

### D5. 圆角不一致

- **现象**：四角有月牙状裸材质
- **根因**：DWM 窗口圆角约 8px，CSS 圆角 10px
- **修法**：统一为 8px

### D6. `+1px` 自增长循环

- **现象**：宽度每次上报都比上次大 1px，持续增长
- **根因**：`scrollWidth` 会跟随窗口宽度 → 形成正反馈
- **修法**：改用纯内容宽度求和；主进程加 **2px 死区**忽略亚像素差异

### D7. 视觉效果"像线框图"

- **现象**：用户评价"看起来很简陋、像指定像素而不是设计出来的"
- **根因**：
  - 每个按钮之间都画 1px 分隔线（表格/线框画法）
  - 阴影 `0 10px 30px` 过重（Cherry 实际只用 `0 2px 3px rgba(50,50,50,.1)`）
- **修法**：改为独立圆角 chip + 仅在末尾分组前留一条细线；阴影降到很轻；
  参考 Cherry 与 pot-desktop（NextUI）的色板（中性灰阶 + 单一强调色）

### D8. 图标风格不符要求

- **现象**：emoji 图标 + 紫色渐变，用户要"扁平、纯文字、不要可爱风"
- **修法**：**去掉全部图标**；用**每动作独立配色**（彩虹色板）保证可区分性

---

## E. 结果窗口毛玻璃

### E1. "透明大框里套白卡"的分层结构

- **现象**（用户描述）：下面一层透明毛玻璃但没内容，上面一个很普通的白色圆角框，
  毛玻璃比白框大一圈，像一个很大的透明白边
- **根因**（代码级）：
  ```rust
  .transparent(true)   // 整个窗口透明
  .shadow(true)        // OS 在透明矩形外围画阴影 ← 用户看到的"外层"
  // 且从未调用 apply_frosted_glass → 根本没有毛玻璃
  ```
  ```css
  .card { margin: 7px; background: #fff; }  // ← 可见的是这一层
  ```
  = 透明窗口 + OS 阴影 + 内缩白卡，三层叠加
- **修法**：窗口本身铺材质（Mica/Acrylic + DWM 圆角）；`.card` 去掉
  边距/背景/边框/阴影，**只剩一层**；`shadow(false)` 去掉那个外框
- **验证**：`DWMWA_SYSTEMBACKDROP_TYPE` 读出 `3`（Acrylic）

### E2. 深色主题下材质不变深

- **现象**：切到深色主题，材质仍是浅色
- **根因**：`apply_mica(w, Some(false))` 硬编码了"浅色"
- **修法**：改为可配置（跟随系统 / 始终浅色 / 始终深色）

---

## F. 配置与保存

### F1. 模型配置填不进去（输入被静默丢弃）

- **现象**：用户"模型根本设置不了"；配置里只有 `apiBase` 有值，
  `name` / `apiKey` / `model` 全空
- **根因**：**陈旧对象引用**

  ```js
  ;(S.providers || []).forEach((p) => {
    i.addEventListener('input', () => { p[key] = i.value; saveNow({ providers: S.providers }) })
  })
  listen('settings-changed', (e) => { S = e.payload })   // S 被替换为全新对象
  ```

  保存广播把 `S` 换成新对象，但闭包里的 `p` 仍是旧对象 →
  后续输入改的是死对象，发出的却是新数组 → **输入被覆盖**
  （所以"第一个字段能存，后面的全丢"）
- **修法**：每次编辑按 id 取当前对象 `cur()`（共 13 处，动作页同样问题一并修）
- **验证**：Node 复现旧/新逻辑，旧 `{"apiKey":"sk-1","model":""}`，
  新 `{"apiKey":"sk-1","model":"gpt-5"}`

### F2. `merge()` 静默重置全部配置

- **现象**：潜在风险（未爆发但机制危险）
- **根因**：`serde_json::from_value(cur).unwrap_or_default()` —— 形状不符即重置为默认
- **修法**：失败时保留原配置并记日志

### F3. 启动死锁（最严重）

- **现象**：进程在运行，但无日志、无迁移生效、界面行为异常
- **根因**：在 `load()` **持有 `STORE` 锁**时调用 `save()`，而 `save()` 又要获取同一把锁；
  `std::sync::Mutex` **不可重入** → 首次读取配置即死锁
- **修法**：拆出 `write_to_disk()`（不加锁），供持锁者调用
- **教训**：这个 bug 现象很像"应用没启动"，实际是卡在锁上；排查时
  "进程存活但零日志"是一个强信号

### F4. 迁移反复覆盖用户选择

- **现象**：把 `copy` 改为默认关闭后，用户手动开启会被"改回去"
- **根因**：若每次启动都无条件执行"关掉 copy"，会与用户意图冲突
- **修法**：引入 `schemaVersion`，迁移**只执行一次**
- **验证**：`schema` 由 `undefined` → `2`，`copy` 由 `true` → `false`，仅一次

### F5. 保存没有反馈，用户不知道存没存

- **现象**：用户"光标还在闪，似乎没保存，也没有保存按键，不知道怎么保存"
- **根因**：保存是自动的（350ms 防抖），但**无按钮、无提示、回车无效** —— UX 缺失
- **修法**：
  - 底部常驻状态栏（有未保存的更改 / 正在保存 / 已保存 / 保存失败）
  - 明确的「保存」按钮
  - 任一输入框**回车即保存**
  - 窗口失焦 / 关闭前强制落盘
- **验证**：Node 实跑保存状态机（pending → 合并输入 → flush → clean，
  且两次输入合并为一次写入，无丢键）

---

## G. 模型接口

### G1. OpenCode `HTTP 400 MissingSessionID`

- **现象**：`{"type":"MissingSessionID","message":"Request is missing x-opencode-session..."}`
- **根因**：OpenCode Go/Zen 服务端要求该请求头
- **修法**：地址含 `opencode.ai` 时自动附加（值为每次安装固定的会话 ID），
  同时发送规范的 `User-Agent`；用户自定义同名头优先
- **验证**（直连实测）：不带头 → `400`；带头 → **`200`** 且正常返回内容

### G2. 厂商 Base URL 手写有误

- **现象**：DeepSeek 需要 `/v1` 后缀（我原来的写法）
- **根因**：凭记忆手写，未核对官方
- **修法**：改为从 Cherry 官方厂商表
  （`packages/provider-registry/data/providers.json`）提取，顺带补上通义等缺失项

---

## H. 遗留 / 已知限制

| 项 | 说明 |
|---|---|
| 无代码签名 | 首次运行会触发 SmartScreen，需手动允许 |
| 依赖 WebView2 | Win10/11 一般自带；极老系统需安装 |
| `cargo test --lib` | 因测试二进制旁缺 `SelectionHook.dll` 而失败（测试环境问题，非产品缺陷） |
| 截图缺失 | README 中截图待补 |
| 剪贴板兜底会覆盖剪贴板 | 这是该方案的固有代价，可在设置中关闭 |
| 未做代码签名与自动更新 | 未在本次范围内 |

---

## 回归检查清单

改动后建议按此清单验证（顺序即依赖关系）：

1. `cargo test --test actions_default --test roundtrip`（逻辑层）
2. `SelectionHook.dll` 编译通过，`test_shim.exe --inject` 能取到词
3. `--preview` 能显示悬浮条；日志 `reveal pos=... size=...` 中
   `size / devicePixelRatio` 等于内容 CSS 宽度
4. 设置窗口：无重复标题栏、非置顶（`WS_EX_TOPMOST=False`）
5. 悬浮条：打字与 Ctrl+C/V 正常（不抢焦点）
6. 模型：填字段→底部状态栏显示"已保存"→关窗重开内容仍在→「测试连接」成功
7. 托盘：反复启动只有**一个**图标

/**
 * C ABI shim over the selection-hook Windows engine.
 *
 * Ported from selection_hook.cc (MIT, 0xfullex): the N-API class is replaced by a
 * plain C handle, the Node ThreadSafeFunctions by an internal dispatch thread, and
 * the N-API worker (core/worker.cc) by a compact MTA worker that reuses the same
 * SelectionEngine. Gesture detection and hook callbacks preserve the original logic.
 */
#define WIN32_LEAN_AND_MEAN 1

#include <initguid.h>  // instantiate DEFINE_GUIDs (CLSID_CUIAutomation, IID_IUIAutomation)

#include <ole2.h>
#include <UIAutomation.h>
#include <process.h>
#include <windows.h>

#include <atomic>
#include <chrono>
#include <cmath>
#include <condition_variable>
#include <cstdio>
#include <cstring>
#include <deque>
#include <memory>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

#include "core/engine.h"
#include "core/types.h"
#include "lib/keyboard.h"
#include "lib/utils.h"
#include "shim.h"

#pragma comment(lib, "UIAutomationCore.lib")
#pragma comment(lib, "User32.lib")
#pragma comment(lib, "Shell32.lib")

// ---- gesture constants (mirror the original) ----
static constexpr int MIN_DRAG_DISTANCE = 8;
static constexpr DWORD MAX_DRAG_TIME_MS = 8000;
static constexpr int DOUBLE_CLICK_MAX_DISTANCE = 3;
static constexpr DWORD DOUBLE_CLICK_TIME_MS = 500;

static bool DebugEnabled()
{
    static int v = -1;
    if (v < 0)
    {
        char buf[8] = {0};
        v = (GetEnvironmentVariableA("SH_DEBUG", buf, sizeof(buf)) > 0 && buf[0] == '1') ? 1 : 0;
    }
    return v == 1;
}
#define SHDBG(...)                       \
    do                                   \
    {                                    \
        if (DebugEnabled())              \
        {                                \
            fprintf(stderr, __VA_ARGS__); \
            fprintf(stderr, "\n");       \
            fflush(stderr);              \
        }                                \
    } while (0)

static const char* StageName(RetrievalStage s)
{
    switch (s)
    {
        case RetrievalStage::Idle: return "Idle";
        case RetrievalStage::ProgramName: return "ProgramName";
        case RetrievalStage::UIAutomation: return "UIAutomation";
        case RetrievalStage::Accessible: return "Accessible";
        case RetrievalStage::Clipboard: return "Clipboard";
        case RetrievalStage::ClipboardPoll: return "ClipboardPoll";
        case RetrievalStage::Complete: return "Complete";
    }
    return "?";
}

//============================================================================
// UTF-8 helpers
//============================================================================
static std::string WideToUtf8(const std::wstring& w)
{
    if (w.empty())
        return std::string();
    int need = WideCharToMultiByte(CP_UTF8, 0, w.c_str(), (int)w.size(), nullptr, 0, nullptr, nullptr);
    if (need <= 0)
        return std::string();
    std::string out((size_t)need, '\0');
    WideCharToMultiByte(CP_UTF8, 0, w.c_str(), (int)w.size(), &out[0], need, nullptr, nullptr);
    return out;
}

static std::wstring Utf8ToWide(const char* s)
{
    if (!s || !*s)
        return std::wstring();
    int need = MultiByteToWideChar(CP_UTF8, 0, s, -1, nullptr, 0);
    if (need <= 0)
        return std::wstring();
    std::wstring out((size_t)(need - 1), L'\0');
    MultiByteToWideChar(CP_UTF8, 0, s, -1, &out[0], need);
    return out;
}

//============================================================================
// Captured hook events (value-only)
//============================================================================
struct MouseCtx
{
    WPARAM event;
    LONG ptX, ptY;
    DWORD mouseData;
    DWORD timestampMs;
    DWORD buttonMask;
    DWORD modifierMask;
    HCURSOR cursorHandle;
    HWND windowHandler;
    RECT windowRect;
    DWORD clipboardSequence;
    bool hasWindowRect;
};

struct KeyCtx
{
    WPARAM event;
    DWORD vkCode;
    DWORD scanCode;
    DWORD flags;
};

struct QueuedEvent
{
    bool isMouse;
    MouseCtx mouse;
    KeyCtx key;
};

//============================================================================
// Shim instance
//============================================================================
class SelHook
{
  public:
    SelHook() = default;
    ~SelHook() { Shutdown(); }

    int Start(SSelectionCb onSel, SMouseCb onMouse, SKeyCb onKey, void* user)
    {
        if (running_.load())
            return 1;
        onSel_ = onSel;
        onMouse_ = onMouse;
        onKey_ = onKey;
        user_ = user;
        lastError_.clear();

        EnableDpiAwareness();

        // Calling-thread (STA) UIA client for the synchronous path.
        HRESULT hr = CoInitializeEx(nullptr, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        comInitialized_ = SUCCEEDED(hr);
        if (SUCCEEDED(hr) || hr == RPC_E_CHANGED_MODE)
            CoCreateInstance(__uuidof(CUIAutomation), nullptr, CLSCTX_INPROC_SERVER, __uuidof(IUIAutomation),
                             (void**)&pUIAutomation_);

        if (!worker_.Start(this))
        {
            lastError_ = "failed to start retrieval worker";
            return 0;
        }

        stopEvent_ = CreateEventW(nullptr, TRUE, FALSE, nullptr);
        readyEvent_ = CreateEventW(nullptr, TRUE, FALSE, nullptr);
        if (!stopEvent_ || !readyEvent_)
        {
            lastError_ = "failed to create events";
            return 0;
        }
        running_.store(true);
        dispatchStop_.store(false);
        dispatchThread_ = std::thread(&SelHook::DispatchLoop, this);
        hookThread_ = std::thread(&SelHook::HookThreadProc, this);
        WaitForSingleObject(readyEvent_, 3000);
        return 1;
    }

    void Stop()
    {
        if (!running_.exchange(false))
            return;
        // Stop hooks first so no new events arrive.
        if (stopEvent_)
            SetEvent(stopEvent_);
        if (hookThread_.joinable())
            hookThread_.join();
        dispatchStop_.store(true);
        dispatchCv_.notify_all();
        if (dispatchThread_.joinable())
            dispatchThread_.join();
        worker_.Stop();
        if (stopEvent_)
        {
            CloseHandle(stopEvent_);
            stopEvent_ = nullptr;
        }
        if (readyEvent_)
        {
            CloseHandle(readyEvent_);
            readyEvent_ = nullptr;
        }
        if (pUIAutomation_)
        {
            pUIAutomation_->Release();
            pUIAutomation_ = nullptr;
        }
        if (comInitialized_)
            CoUninitialize();
        comInitialized_ = false;
    }

    void Shutdown() { Stop(); }

    bool IsRunning() const { return running_.load(); }

    // ---- config ----
    void SetPassive(bool p) { passiveMode_.store(p); }
    void SetClipboardEnabled(bool e) { clipboardEnabled_.store(e); }
    void EnableMouseMove(bool e) { mouseMoveEnabled_.store(e); }
    void SetGlobalFilter(int mode, const std::vector<std::string>& list)
    {
        std::lock_guard<std::mutex> lk(configMutex_);
        globalFilterMode_ = (FilterMode)mode;
        globalFilterList_ = list;
    }
    void SetClipboardFilter(int mode, const std::vector<std::string>& list)
    {
        std::lock_guard<std::mutex> lk(configMutex_);
        clipboardFilterMode_ = (FilterMode)mode;
        clipboardFilterList_ = list;
    }
    void SetFineTuned(int type, const std::vector<std::string>& list)
    {
        std::lock_guard<std::mutex> lk(configMutex_);
        if (type == SH_FTL_EXCLUDE_CLIPBOARD_CURSOR_DETECT)
            ftlExcludeCursor_ = list;
        else if (type == SH_FTL_INCLUDE_CLIPBOARD_DELAY_READ)
            ftlIncludeDelay_ = list;
    }

    SelectionConfigSnapshot BuildConfig(bool triggeredByUser) const
    {
        SelectionConfigSnapshot c;
        std::lock_guard<std::mutex> lk(configMutex_);
        c.clipboardEnabled = clipboardEnabled_.load();
        c.triggeredByUser = triggeredByUser;
        c.clipboardFilterMode = clipboardFilterMode_;
        c.clipboardFilterList = clipboardFilterList_;
        c.globalFilterMode = globalFilterMode_;
        c.globalFilterList = globalFilterList_;
        c.excludeClipboardCursorDetect = ftlExcludeCursor_;
        c.includeClipboardDelayRead = ftlIncludeDelay_;
        c.mouseDownCursor = mouseDownCursor_;
        c.mouseUpCursor = mouseUpCursor_;
        c.clipboardSequence = clipboardSequence_;
        return c;
    }

    bool GetCurrentSelection(TextSelectionInfo& info)
    {
        HWND hwnd = GetForegroundWindow();
        if (!hwnd)
            return false;
        bool expected = false;
        if (!syncInFlight_.compare_exchange_strong(expected, true))
            return false;
        struct Guard
        {
            std::atomic<bool>* f;
            ~Guard() { f->store(false); }
        } g{&syncInFlight_};

        SelectionConfigSnapshot config = BuildConfig(true);
        SelectionEngine engine(pUIAutomation_);
        DWORD endSeq = config.clipboardSequence;
        CONTROLTYPEID endType = uiaControlType_;
        std::atomic<RetrievalStage> stage{RetrievalStage::Idle};
        bool ok = engine.GetSelectedText(hwnd, info, config, ClipboardLockMode::TryOnly, nullptr, &stage, &endSeq,
                                         &endType);
        SHDBG("sync: hwnd=%p automation=%p ok=%d textLen=%d stage=%s", (void*)hwnd, (void*)pUIAutomation_, (int)ok,
              (int)info.text.size(), StageName(stage.load()));
        clipboardSequence_ = endSeq;
        uiaControlType_ = endType;
        return ok;
    }

    /**
     * Queue a retrieval on the worker thread and deliver the result through the normal
     * selection callback.
     *
     * The host uses this instead of `GetCurrentSelection` so no COM/UIA call ever runs on
     * its UI thread: a busy or hung foreground application makes a synchronous retrieval
     * block for an unbounded time, which is what froze the whole app when the tray menu
     * was used.
     */
    bool RequestSelection(bool triggeredByUser)
    {
        if (!running_.load())
            return false;
        HWND fg = GetForegroundWindow();
        if (!fg)
            return false;
        SelectionRequest req;
        req.sequence = ++sequence_;
        req.retrievalHwnd = fg;
        req.detectionType = SelectionDetectType::None;
        req.mouseDownPos = lastMouseDownPos_;
        req.mouseUpPos = lastMouseUpPos_;
        req.previousMouseUpPos = lastLastMouseUpPos_;
        req.config = BuildConfig(triggeredByUser);
        bool ok = worker_.Enqueue(req);
        SHDBG("request-selection: hwnd=%p user=%d ok=%d", (void*)fg, (int)triggeredByUser, (int)ok);
        return ok;
    }

    bool WriteClipboard(const std::wstring& text) { return SelectionEngine::TryWriteClipboard(text); }
    bool ReadClipboard(std::wstring& out) { return SelectionEngine::TryReadClipboard(out); }

    const std::string& LastError() const { return lastError_; }

    // ---- called from worker thread ----
    void EmitSelection(const TextSelectionInfo& info)
    {
        if (!onSel_)
            return;
        std::string text = WideToUtf8(info.text);
        std::string prog = WideToUtf8(info.programName);
        SSelectionOut out;
        memset(&out, 0, sizeof(out));
        out.text = text.c_str();
        out.programName = prog.c_str();
        out.startTop = {info.startTop.x, info.startTop.y};
        out.startBottom = {info.startBottom.x, info.startBottom.y};
        out.endTop = {info.endTop.x, info.endTop.y};
        out.endBottom = {info.endBottom.x, info.endBottom.y};
        out.mousePosStart = {info.mousePosStart.x, info.mousePosStart.y};
        out.mousePosEnd = {info.mousePosEnd.x, info.mousePosEnd.y};
        out.method = (int)info.method;
        out.posLevel = (int)info.posLevel;
        onSel_(&out, user_);
    }

    void SetError(const std::string& e)
    {
        std::lock_guard<std::mutex> lk(configMutex_);
        lastError_ = e;
    }

  private:
    // ---- MTA retrieval worker ----
    class Worker
    {
      public:
        bool Start(SelHook* owner)
        {
            owner_ = owner;
            std::unique_lock<std::mutex> lk(m_);
            thread_ = std::thread(&Worker::ThreadMain, this);
            cvReady_.wait_for(lk, std::chrono::seconds(5), [this] { return ready_ || failed_; });
            return !failed_;
        }

        void Stop()
        {
            {
                std::lock_guard<std::mutex> lk(m_);
                stop_ = true;
                cv_.notify_all();
            }
            if (thread_.joinable())
                thread_.join();
        }

        bool Enqueue(const SelectionRequest& req)
        {
            std::lock_guard<std::mutex> lk(m_);
            if (stop_ || hasPending_)
                return false;
            pending_ = req;
            hasPending_ = true;
            cv_.notify_all();
            return true;
        }

        bool Busy() const { return busy_.load(); }

      private:
        void ThreadMain()
        {
            HRESULT hr = CoInitializeEx(nullptr, COINIT_MULTITHREADED);
            if (FAILED(hr))
            {
                std::lock_guard<std::mutex> lk(m_);
                failed_ = true;
                cvReady_.notify_all();
                return;
            }
            IUIAutomation* uia = nullptr;
            HRESULT hr2 = CoCreateInstance(__uuidof(CUIAutomation), nullptr, CLSCTX_INPROC_SERVER, __uuidof(IUIAutomation),
                                           (void**)&uia);
            {
                std::lock_guard<std::mutex> lk(m_);
                failed_ = FAILED(hr2) || !uia;
                cvReady_.notify_all();
            }
            if (FAILED(hr2) || !uia)
            {
                CoUninitialize();
                return;
            }

            SelectionEngine engine(uia);
            std::unique_lock<std::mutex> lk(m_);
            while (!stop_)
            {
                cv_.wait(lk, [this] { return stop_ || hasPending_; });
                if (stop_)
                    break;
                SelectionRequest req = pending_;
                hasPending_ = false;
                lk.unlock();
                busy_.store(true);
                {
                    HWND fgw = GetForegroundWindow();
                    char cls[128] = {0}, fcls[128] = {0};
                    if (req.retrievalHwnd) GetClassNameA(req.retrievalHwnd, cls, sizeof(cls));
                    if (fgw) GetClassNameA(fgw, fcls, sizeof(fcls));
                    SHDBG("worker: req hwnd=%p class=%s | foreground=%p class=%s same=%d", (void*)req.retrievalHwnd, cls,
                          (void*)fgw, fcls, (int)(fgw == req.retrievalHwnd));
                }
                if (DebugEnabled())
                {
                    HCURSOR beam = LoadCursor(NULL, IDC_IBEAM);
                    HCURSOR arrow = LoadCursor(NULL, IDC_ARROW);
                    SHDBG("worker: cursors down=%p up=%p | beam=%p arrow=%p", (void*)req.config.mouseDownCursor,
                          (void*)req.config.mouseUpCursor, (void*)beam, (void*)arrow);
                }
                if (IsWindow(req.retrievalHwnd))
                {
                    TextSelectionInfo info;
                    DWORD endSeq = req.config.clipboardSequence;
                    CONTROLTYPEID endType = UIA_WindowControlTypeId;
                    std::atomic<RetrievalStage> stage{RetrievalStage::Idle};
                    bool ok = engine.GetSelectedText(req.retrievalHwnd, info, req.config,
                                                     ClipboardLockMode::InterruptibleWait, nullptr, &stage, &endSeq,
                                                     &endType);
                    SHDBG("worker: GetSelectedText ok=%d textLen=%d method=%d posLevel=%d stage=%d stageName=%s",
                          (int)ok, (int)info.text.size(), (int)info.method, (int)info.posLevel, (int)stage.load(),
                          StageName(stage.load()));
                    if (ok && owner_)
                        owner_->EmitSelection(info);
                }
                busy_.store(false);
                lk.lock();
            }
            uia->Release();
            CoUninitialize();
        }

        SelHook* owner_ = nullptr;
        std::thread thread_;
        std::mutex m_;
        std::condition_variable cv_;
        std::condition_variable cvReady_;
        bool ready_ = false;
        bool failed_ = false;
        bool stop_ = false;
        bool hasPending_ = false;
        std::atomic<bool> busy_{false};
        SelectionRequest pending_;
    };

    // ---- hook thread ----
    void HookThreadProc()
    {
        MSG msg = {};
        PeekMessageW(&msg, nullptr, WM_USER, WM_USER, PM_NOREMOVE);
        SetEvent(readyEvent_);

        mouseHook_ = SetWindowsHookExW(WH_MOUSE_LL, &SelHook::MouseProc, nullptr, 0);
        keyboardHook_ = SetWindowsHookExW(WH_KEYBOARD_LL, &SelHook::KeyboardProc, nullptr, 0);
        if (!mouseHook_)
            fprintf(stderr, "[shim] WH_MOUSE_LL install failed: %lu\n", GetLastError());
        if (!keyboardHook_)
            fprintf(stderr, "[shim] WH_KEYBOARD_LL install failed: %lu\n", GetLastError());

        DWORD waitFlags = MWMO_INPUTAVAILABLE;
        while (true)
        {
            HANDLE stop = stopEvent_;
            DWORD r = MsgWaitForMultipleObjectsEx(1, &stop, INFINITE, QS_ALLINPUT, waitFlags);
            if (r == WAIT_OBJECT_0)
                break;
            if (r == WAIT_OBJECT_0 + 1)
            {
                BOOL found = PeekMessageW(&msg, nullptr, 0, 0, PM_REMOVE);
                if (found && msg.message == WM_QUIT)
                    break;
                waitFlags = found ? MWMO_INPUTAVAILABLE : 0;
                continue;
            }
            break;
        }
        if (mouseHook_)
        {
            UnhookWindowsHookEx(mouseHook_);
            mouseHook_ = nullptr;
        }
        if (keyboardHook_)
        {
            UnhookWindowsHookEx(keyboardHook_);
            keyboardHook_ = nullptr;
        }
    }

    static LRESULT CALLBACK MouseProc(int nCode, WPARAM wParam, LPARAM lParam)
    {
        SelHook* self = instance_;
        if (nCode == HC_ACTION && self && self->running_.load())
        {
            MSLLHOOKSTRUCT* mi = (MSLLHOOKSTRUCT*)lParam;
            // Zero-I/O bookkeeping: any logging here can exceed Windows'
            // LowLevelHooksTimeout and get the hook silently removed.
            const char* tag = "other";
            switch (wParam)
            {
                case WM_LBUTTONDOWN: self->statDown_.fetch_add(1); tag = "down"; break;
                case WM_LBUTTONUP: self->statUp_.fetch_add(1); tag = "up"; break;
                case WM_MOUSEMOVE: self->statMove_.fetch_add(1); tag = "move"; break;
                case WM_MOUSEWHEEL:
                case WM_MOUSEHWHEEL: self->statWheel_.fetch_add(1); tag = "wheel"; break;
                default: break;
            }
            self->RecordRaw((unsigned)wParam, (unsigned long)mi->time, mi->pt.x, mi->pt.y, tag);
            switch (wParam)
            {
                case WM_LBUTTONDOWN: self->buttonFlag_.fetch_or(0x01u); break;
                case WM_LBUTTONUP: self->buttonFlag_.fetch_and(~0x01u); break;
                case WM_RBUTTONDOWN: self->buttonFlag_.fetch_or(0x02u); break;
                case WM_RBUTTONUP: self->buttonFlag_.fetch_and(~0x02u); break;
                case WM_MBUTTONDOWN: self->buttonFlag_.fetch_or(0x04u); break;
                case WM_MBUTTONUP: self->buttonFlag_.fetch_and(~0x04u); break;
                default: break;
            }

            bool isMove = (wParam == WM_MOUSEMOVE);
            if (!(isMove && !self->mouseMoveEnabled_.load()))
            {
                MouseCtx c;
                memset(&c, 0, sizeof(c));
                c.event = wParam;
                c.ptX = mi->pt.x;
                c.ptY = mi->pt.y;
                c.mouseData = mi->mouseData;
                c.timestampMs = mi->time;
                c.buttonMask = self->buttonFlag_.load();
                DWORD mod = 0;
                if (GetAsyncKeyState(VK_SHIFT) & 0x8000) mod |= 0x01;
                if (GetAsyncKeyState(VK_CONTROL) & 0x8000) mod |= 0x02;
                if (GetAsyncKeyState(VK_MENU) & 0x8000) mod |= 0x04;
                c.modifierMask = mod;
                if (wParam == WM_LBUTTONDOWN || wParam == WM_LBUTTONUP)
                {
                    CURSORINFO ci = {sizeof(CURSORINFO)};
                    if (GetCursorInfo(&ci))
                        c.cursorHandle = ci.hCursor;
                    HWND h = WindowFromPoint(mi->pt);
                    if (!h)
                        h = GetForegroundWindow();
                    if (h && GetWindowRect(h, &c.windowRect))
                    {
                        c.windowHandler = h;
                        c.hasWindowRect = true;
                    }
                    if (wParam == WM_LBUTTONDOWN)
                        c.clipboardSequence = GetClipboardSequenceNumber();
                }
                self->Enqueue({true, c, {}});
            }
        }
        return CallNextHookEx(nullptr, nCode, wParam, lParam);
    }

    static LRESULT CALLBACK KeyboardProc(int nCode, WPARAM wParam, LPARAM lParam)
    {
        SelHook* self = instance_;
        if (nCode == HC_ACTION && self && self->running_.load())
        {
            KBDLLHOOKSTRUCT* ki = (KBDLLHOOKSTRUCT*)lParam;
            KeyCtx c;
            c.event = wParam;
            c.vkCode = ki->vkCode;
            c.scanCode = ki->scanCode;
            c.flags = ki->flags;
            self->Enqueue({false, {}, c});
        }
        return CallNextHookEx(nullptr, nCode, wParam, lParam);
    }

    void Enqueue(QueuedEvent ev)
    {
        {
            std::lock_guard<std::mutex> lk(queueMutex_);
            if (queue_.size() > 512)
                queue_.pop_front();
            queue_.push_back(std::move(ev));
        }
        dispatchCv_.notify_one();
    }

    // ---- dispatch thread: gesture detection + event emission ----
    void DispatchLoop()
    {
        while (true)
        {
            QueuedEvent ev;
            {
                std::unique_lock<std::mutex> lk(queueMutex_);
                dispatchCv_.wait(lk, [this] { return dispatchStop_.load() || !queue_.empty(); });
                if (queue_.empty())
                {
                    if (dispatchStop_.load())
                        return;
                    continue;
                }
                ev = std::move(queue_.front());
                queue_.pop_front();
            }
            if (ev.isMouse)
                HandleMouse(ev.mouse);
            else
                HandleKey(ev.key);
        }
    }

    void HandleKey(const KeyCtx& k)
    {
        if (!onKey_)
            return;
        SKeyEvent out;
        memset(&out, 0, sizeof(out));
        out.action = (k.event == WM_KEYDOWN || k.event == WM_SYSKEYDOWN) ? 0 : 1;
        std::string uni = convertKeyCodeToUniKey((DWORD)k.vkCode, k.scanCode, k.flags);
        out.uniKey = uni.c_str();
        out.vkCode = (int)k.vkCode;
        out.sys = (k.event == WM_SYSKEYDOWN || k.event == WM_SYSKEYUP) ? 1 : 0;
        out.flags = (int)k.flags;
        onKey_(&out, user_);
    }

    void HandleMouse(const MouseCtx& m)
    {
        if (!running_.load())
            return;
        POINT pos = {m.ptX, m.ptY};
        DWORD eventTime = m.timestampMs;
        bool shouldDetect = false;
        SelectionDetectType detection = SelectionDetectType::None;
        const char* action = nullptr;
        int button = -1;
        int flag = 0;

        switch (m.event)
        {
            case WM_LBUTTONDOWN:
                action = "mouse-down";
                button = 0;
                lastMouseDownTime_ = eventTime;
                lastMouseDownPos_ = pos;
                lastWindowHandler_ = m.windowHandler;
                lastWindowRect_ = m.windowRect;
                hasLastWindowRect_ = m.hasWindowRect;
                SHDBG("down: t=%lu pos=%d,%d hwnd=%p", (unsigned long)eventTime, pos.x, pos.y, (void*)m.windowHandler);
                if (clipboardEnabled_.load())
                    mouseDownCursor_ = m.cursorHandle;
                clipboardSequence_ = m.clipboardSequence;
                break;
            case WM_LBUTTONUP:
            {
                action = "mouse-up";
                button = 0;
                DWORD currentTime = eventTime;
                if (!passiveMode_.load())
                {
                    int dx = pos.x - lastMouseDownPos_.x;
                    int dy = pos.y - lastMouseDownPos_.y;
                    double dist = sqrt((double)dx * dx + (double)dy * dy);
                    bool isCurrentValidClick = (currentTime - lastMouseDownTime_) <= DOUBLE_CLICK_TIME_MS;

                    if ((currentTime - lastMouseDownTime_) > MAX_DRAG_TIME_MS)
                    {
                        shouldDetect = false;
                        SHDBG("up: reject=time el=%lu cur=%lu down=%lu",
                              (unsigned long)(currentTime - lastMouseDownTime_), (unsigned long)currentTime,
                              (unsigned long)lastMouseDownTime_);
                    }
                    else if (dist >= MIN_DRAG_DISTANCE)
                    {
                        bool sameWin = (m.windowHandler && m.windowHandler == lastWindowHandler_);
                        bool moved = (m.hasWindowRect && hasLastWindowRect_ && HasWindowMoved(m.windowRect, lastWindowRect_));
                        SHDBG("up: drag dist=%.1f sameWin=%d moved=%d curHwnd=%p lastHwnd=%p", dist, (int)sameWin, (int)moved,
                              (void*)m.windowHandler, (void*)lastWindowHandler_);
                        if (sameWin && !moved)
                        {
                            shouldDetect = true;
                            detection = SelectionDetectType::Drag;
                        }
                    }
                    else if (isLastValidClick_ && isCurrentValidClick && dist <= DOUBLE_CLICK_MAX_DISTANCE)
                    {
                        int ddx = pos.x - lastMouseUpPos_.x;
                        int ddy = pos.y - lastMouseUpPos_.y;
                        double d2 = sqrt((double)ddx * ddx + (double)ddy * ddy);
                        if (d2 <= DOUBLE_CLICK_MAX_DISTANCE &&
                            (lastMouseDownTime_ - lastMouseUpTime_) <= DOUBLE_CLICK_TIME_MS)
                        {
                            if (m.windowHandler && m.windowHandler == lastWindowHandler_ &&
                                !(m.hasWindowRect && hasLastWindowRect_ && HasWindowMoved(m.windowRect, lastWindowRect_)))
                            {
                                shouldDetect = true;
                                detection = SelectionDetectType::DoubleClick;
                            }
                        }
                    }
                    if (!shouldDetect)
                    {
                        bool shift = (m.modifierMask & 0x01) != 0;
                        bool ctrl = (m.modifierMask & 0x02) != 0;
                        bool alt = (m.modifierMask & 0x04) != 0;
                        if (shift && !ctrl && !alt)
                        {
                            shouldDetect = true;
                            detection = SelectionDetectType::ShiftClick;
                        }
                    }
                    if (shouldDetect && clipboardEnabled_.load())
                        mouseUpCursor_ = m.cursorHandle;
                    isLastValidClick_ = isCurrentValidClick;
                }
                lastLastMouseUpPos_ = lastMouseUpPos_;
                lastMouseUpTime_ = currentTime;
                lastMouseUpPos_ = pos;
                break;
            }
            case WM_MOUSEMOVE:
                action = "mouse-move";
                button = (m.buttonMask & 0x01) ? 0 : (m.buttonMask & 0x02) ? 2 : (m.buttonMask & 0x04) ? 1 : -1;
                break;
            case WM_RBUTTONDOWN: action = "mouse-down"; button = 2; break;
            case WM_RBUTTONUP: action = "mouse-up"; button = 2; break;
            case WM_MBUTTONDOWN: action = "mouse-down"; button = 1; break;
            case WM_MBUTTONUP: action = "mouse-up"; button = 1; break;
            case WM_XBUTTONDOWN: action = "mouse-down"; button = (HIWORD(m.mouseData) == XBUTTON1) ? 3 : 4; break;
            case WM_XBUTTONUP: action = "mouse-up"; button = (HIWORD(m.mouseData) == XBUTTON1) ? 3 : 4; break;
            case WM_MOUSEWHEEL: action = "mouse-wheel"; button = 0; flag = (GET_WHEEL_DELTA_WPARAM(m.mouseData) > 0) ? 1 : -1; break;
            case WM_MOUSEHWHEEL: action = "mouse-wheel"; button = 1; flag = (GET_WHEEL_DELTA_WPARAM(m.mouseData) > 0) ? 1 : -1; break;
            default: break;
        }

        if (shouldDetect && !worker_.Busy())
        {
            HWND fg = GetForegroundWindow();
            if (fg)
            {
                SelectionRequest req;
                req.sequence = ++sequence_;
                req.retrievalHwnd = fg;
                req.detectionType = detection;
                req.mouseDownPos = lastMouseDownPos_;
                req.mouseUpPos = lastMouseUpPos_;
                req.previousMouseUpPos = lastLastMouseUpPos_;
                req.config = BuildConfig(false);
                bool ok = worker_.Enqueue(req);
                SHDBG("dispatch: enqueue detection=%d hwnd=%p ok=%d", (int)detection, (void*)fg, (int)ok);
            }
            else
                SHDBG("dispatch: no foreground window");
        }
        else if (shouldDetect)
            SHDBG("dispatch: skipped, worker busy");

        if (action && onMouse_)
        {
            SMouseEvent out;
            out.action = (strcmp(action, "mouse-down") == 0) ? SH_MOUSE_DOWN
                         : (strcmp(action, "mouse-up") == 0) ? SH_MOUSE_UP
                         : (strcmp(action, "mouse-wheel") == 0) ? SH_MOUSE_WHEEL
                                                                : SH_MOUSE_MOVE;
            out.x = pos.x;
            out.y = pos.y;
            out.button = button;
            out.flag = flag;
            onMouse_(&out, user_);
        }
    }

    void EnableDpiAwareness()
    {
        HMODULE user32 = GetModuleHandleW(L"user32.dll");
        if (user32)
        {
            typedef BOOL(WINAPI * SetProcessDpiAwarenessContextFn)(HANDLE);
            auto fn = (SetProcessDpiAwarenessContextFn)GetProcAddress(user32, "SetProcessDpiAwarenessContext");
            if (fn)
            {
                fn((HANDLE)-4);  // DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2
                return;
            }
        }
        SetProcessDPIAware();
    }

    // ---- state ----
  public:
    static SelHook* instance_;

  private:

    std::atomic<bool> running_{false};
    SSelectionCb onSel_ = nullptr;
    SMouseCb onMouse_ = nullptr;
    SKeyCb onKey_ = nullptr;
    void* user_ = nullptr;

    std::thread hookThread_;
    std::thread dispatchThread_;
    HANDLE stopEvent_ = nullptr;
    HANDLE readyEvent_ = nullptr;
    HHOOK mouseHook_ = nullptr;
    HHOOK keyboardHook_ = nullptr;

    std::mutex queueMutex_;
    std::condition_variable dispatchCv_;
    std::deque<QueuedEvent> queue_;
    std::atomic<bool> dispatchStop_{false};

    Worker worker_;

    // gesture state
    std::atomic<DWORD> buttonFlag_{0};
    POINT lastMouseDownPos_{0, 0};
    DWORD lastMouseDownTime_ = 0;
    POINT lastMouseUpPos_{0, 0};
    POINT lastLastMouseUpPos_{0, 0};
    DWORD lastMouseUpTime_ = 0;
    bool isLastValidClick_ = false;
    HWND lastWindowHandler_ = nullptr;
    RECT lastWindowRect_{0, 0, 0, 0};
    bool hasLastWindowRect_ = false;
    HCURSOR mouseDownCursor_ = nullptr;
    HCURSOR mouseUpCursor_ = nullptr;
    DWORD clipboardSequence_ = 0;

    std::atomic<bool> passiveMode_{false};
    std::atomic<bool> clipboardEnabled_{true};
    std::atomic<bool> mouseMoveEnabled_{false};

    mutable std::mutex configMutex_;
    FilterMode globalFilterMode_ = FilterMode::Default;
    std::vector<std::string> globalFilterList_;
    FilterMode clipboardFilterMode_ = FilterMode::Default;
    std::vector<std::string> clipboardFilterList_;
    std::vector<std::string> ftlExcludeCursor_;
    std::vector<std::string> ftlIncludeDelay_;
    std::string lastError_;

    IUIAutomation* pUIAutomation_ = nullptr;
    CONTROLTYPEID uiaControlType_ = UIA_WindowControlTypeId;
    bool comInitialized_ = false;
    std::atomic<bool> syncInFlight_{false};
    uint64_t sequence_ = 0;

  public:
    // ---- zero-I/O diagnostics (safe to touch from the hook callback) ----
    std::atomic<int32_t> statDown_{0};
    std::atomic<int32_t> statUp_{0};
    std::atomic<int32_t> statMove_{0};
    std::atomic<int32_t> statWheel_{0};

    struct RawEv
    {
        unsigned msg;
        unsigned long time;
        long x, y;
        char tag[16];
    };

    /// Record a raw hook event without any I/O. Ring buffer, last 64 events.
    void RecordRaw(unsigned msg, unsigned long time, long x, long y, const char* tag)
    {
        const uint32_t i = rawCount_.fetch_add(1);
        RawEv* e = &raw_[i % RAW_CAP];
        e->msg = msg;
        e->time = time;
        e->x = x;
        e->y = y;
        size_t n = 0;
        for (; tag[n] && n < sizeof(e->tag) - 1; ++n)
            e->tag[n] = tag[n];
        e->tag[n] = 0;
    }

    static constexpr uint32_t RAW_CAP = 64;
    RawEv raw_[RAW_CAP];
    std::atomic<uint32_t> rawCount_{0};
};

SelHook* SelHook::instance_ = nullptr;

//============================================================================
// C ABI
//============================================================================
extern "C" {

void* sh_create(void)
{
    SelHook* h = new (std::nothrow) SelHook();
    if (h)
        SelHook::instance_ = h;
    return h;
}

void sh_destroy(void* handle)
{
    SelHook* h = (SelHook*)handle;
    if (!h)
        return;
    if (SelHook::instance_ == h)
        SelHook::instance_ = nullptr;
    delete h;
}

int32_t sh_start(void* handle, SSelectionCb onSel, SMouseCb onMouse, SKeyCb onKey, void* user)
{
    SelHook* h = (SelHook*)handle;
    if (!h)
        return 0;
    return h->Start(onSel, onMouse, onKey, user);
}

void sh_stop(void* handle)
{
    if (handle)
        ((SelHook*)handle)->Stop();
}

int32_t sh_is_running(void* handle)
{
    return handle && ((SelHook*)handle)->IsRunning() ? 1 : 0;
}

void sh_set_passive_mode(void* handle, int32_t passive)
{
    if (handle)
        ((SelHook*)handle)->SetPassive(passive != 0);
}

void sh_set_clipboard_enabled(void* handle, int32_t enabled)
{
    if (handle)
        ((SelHook*)handle)->SetClipboardEnabled(enabled != 0);
}

void sh_enable_mouse_move(void* handle, int32_t enable)
{
    if (handle)
        ((SelHook*)handle)->EnableMouseMove(enable != 0);
}

static std::vector<std::string> ToList(const char* const* list, int32_t count)
{
    std::vector<std::string> v;
    for (int32_t i = 0; i < count && list; ++i)
        if (list[i])
            v.emplace_back(list[i]);
    return v;
}

void sh_set_global_filter(void* handle, int32_t mode, const char* const* list, int32_t count)
{
    if (handle)
        ((SelHook*)handle)->SetGlobalFilter(mode, ToList(list, count));
}

void sh_set_clipboard_filter(void* handle, int32_t mode, const char* const* list, int32_t count)
{
    if (handle)
        ((SelHook*)handle)->SetClipboardFilter(mode, ToList(list, count));
}

void sh_set_fine_tuned(void* handle, int32_t listType, const char* const* list, int32_t count)
{
    if (handle)
        ((SelHook*)handle)->SetFineTuned(listType, ToList(list, count));
}

int32_t sh_request_selection(void* handle, int32_t triggeredByUser)
{
    SelHook* h = (SelHook*)handle;
    if (!h)
        return 0;
    return h->RequestSelection(triggeredByUser != 0) ? 1 : 0;
}

int32_t sh_get_current_selection(void* handle, SSelectionOut* out)
{
    SelHook* h = (SelHook*)handle;
    if (!h || !out)
        return 0;
    TextSelectionInfo info;
    if (!h->GetCurrentSelection(info))
        return 0;
    // Reuse the emission path to fill the struct; static storage keeps the strings alive.
    static thread_local std::string sText;
    static thread_local std::string sProg;
    sText = WideToUtf8(info.text);
    sProg = WideToUtf8(info.programName);
    memset(out, 0, sizeof(*out));
    out->text = sText.c_str();
    out->programName = sProg.c_str();
    out->startTop = {info.startTop.x, info.startTop.y};
    out->startBottom = {info.startBottom.x, info.startBottom.y};
    out->endTop = {info.endTop.x, info.endTop.y};
    out->endBottom = {info.endBottom.x, info.endBottom.y};
    out->mousePosStart = {info.mousePosStart.x, info.mousePosStart.y};
    out->mousePosEnd = {info.mousePosEnd.x, info.mousePosEnd.y};
    out->method = (int)info.method;
    out->posLevel = (int)info.posLevel;
    return 1;
}

int32_t sh_write_clipboard(void* handle, const char* utf8)
{
    SelHook* h = (SelHook*)handle;
    if (!h)
        return 0;
    return h->WriteClipboard(Utf8ToWide(utf8)) ? 1 : 0;
}

const char* sh_read_clipboard(void* handle)
{
    SelHook* h = (SelHook*)handle;
    if (!h)
        return nullptr;
    static thread_local std::string s;
    std::wstring w;
    if (!h->ReadClipboard(w))
        return nullptr;
    s = WideToUtf8(w);
    return s.c_str();
}

const char* sh_last_error(void* handle)
{
    SelHook* h = (SelHook*)handle;
    if (h)
        return h->LastError().c_str();
    return "invalid handle";
}

void sh_stats(void* handle, int32_t* down, int32_t* up, int32_t* move, int32_t* wheel)
{
    SelHook* h = (SelHook*)handle;
    if (!h)
        return;
    if (down) *down = h->statDown_.load();
    if (up) *up = h->statUp_.load();
    if (move) *move = h->statMove_.load();
    if (wheel) *wheel = h->statWheel_.load();
}

int32_t sh_dump_events(void* handle, char* buf, int32_t buflen)
{
    SelHook* h = (SelHook*)handle;
    if (!h || !buf || buflen <= 0)
        return 0;
    const uint32_t total = h->rawCount_.load();
    const uint32_t start = total > SelHook::RAW_CAP ? total - SelHook::RAW_CAP : 0;
    int32_t used = 0;
    for (uint32_t i = start; i < total; ++i)
    {
        const SelHook::RawEv& e = h->raw_[i % SelHook::RAW_CAP];
        int n = snprintf(buf + used, (size_t)(buflen - used), "%s@%lu(%ld,%ld) ", e.tag, e.time, e.x, e.y);
        if (n < 0 || n >= buflen - used)
            break;
        used += n;
    }
    buf[used < buflen ? used : buflen - 1] = 0;
    return used;
}

}  // extern "C"

/**
 * Test harness for SelectionHook.dll (C ABI).
 * Starts the hook, prints events as JSON lines, and can also poll getCurrentSelection.
 *
 * Build: g++ -std=c++17 -O2 test_shim.cpp -o test_shim.exe -L. -lSelectionHook
 */
#define WIN32_LEAN_AND_MEAN 1
#include <windows.h>
#include <cstdio>
#include <cstring>
#include <string>

#include "shim.h"

static void J(const char* fmt, ...)
{
    va_list ap;
    va_start(ap, fmt);
    vprintf(fmt, ap);
    va_end(ap);
    printf("\n");
    fflush(stdout);
}

static void JsonEsc(const char* s, char* out, size_t n)
{
    size_t j = 0;
    for (size_t i = 0; s && s[i] && j + 2 < n; ++i)
    {
        char c = s[i];
        if (c == '"' || c == '\\')
        {
            out[j++] = '\\';
            out[j++] = c;
        }
        else if (c == '\n')
        {
            out[j++] = '\\';
            out[j++] = 'n';
        }
        else if (c == '\r')
        {
            out[j++] = '\\';
            out[j++] = 'r';
        }
        else if ((unsigned char)c < 0x20)
        {
            j += snprintf(out + j, n - j, "\\u%04x", (unsigned char)c);
        }
        else
            out[j++] = c;
    }
    out[j] = 0;
}

static void OnSelection(const SSelectionOut* s, void*)
{
    char text[8192], prog[512];
    JsonEsc(s->text, text, sizeof(text));
    JsonEsc(s->programName, prog, sizeof(prog));
    J("{\"ev\":\"selection\",\"text\":\"%s\",\"programName\":\"%s\",\"posLevel\":%d,\"refPoint\":{\"x\":%d,\"y\":%d}}", text,
      prog, s->posLevel, s->mousePosEnd.x, s->mousePosEnd.y);
}

static void OnMouse(const SMouseEvent* e, void*)
{
    J("{\"ev\":\"mouse\",\"action\":%d,\"x\":%d,\"y\":%d,\"button\":%d,\"flag\":%d}", e->action, e->x, e->y, e->button,
      e->flag);
}

static void OnKey(const SKeyEvent* e, void*)
{
    J("{\"ev\":\"key\",\"action\":%d,\"uniKey\":\"%s\",\"vk\":%d,\"sys\":%d}", e->action, e->uniKey ? e->uniKey : "",
      e->vkCode, e->sys);
}

int main(int argc, char** argv)
{
    bool passive = false;
    bool pollMode = false;
    bool injectMode = false;
    bool injectOnly = false;
    for (int i = 1; i < argc; ++i)
    {
        if (strcmp(argv[i], "--passive") == 0)
            passive = true;
        if (strcmp(argv[i], "--poll") == 0)
            pollMode = true;
        if (strcmp(argv[i], "--inject") == 0)
            injectMode = true;
        if (strcmp(argv[i], "--inject-only") == 0)
            injectOnly = true;
    }

    // Injection helper: drive a drag without installing our own hook, so a separate
    // process (the app under test) can own the hook and observe the selection.
    if (injectOnly)
    {
        // Declare DPI awareness first: otherwise Windows virtualises SetCursorPos and
        // GetWindowRect, so the injected coordinates land elsewhere on a scaled display
        // and make a correct app look wrong.
        {
            HMODULE u32 = GetModuleHandleW(L"user32.dll");
            if (u32)
            {
                typedef BOOL(WINAPI * Fn)(HANDLE);
                auto fn = (Fn)GetProcAddress(u32, "SetProcessDpiAwarenessContext");
                if (fn)
                    fn((HANDLE)-4);  // PER_MONITOR_AWARE_V2
            }
        }
        HWND t = nullptr;
        for (int i = 0; i < 40 && !t; ++i)
        {
            t = FindWindowW(L"SaTestTargetWnd", nullptr);
            if (!t)
                Sleep(250);
        }
        if (!t)
        {
            printf("INJECT_NO_TARGET\n");
            return 2;
        }
        SetForegroundWindow(t);
        Sleep(600);
        RECT r;
        GetWindowRect(t, &r);
        int x0 = r.left + 50, x1 = r.left + 520, y = r.top + 91;
        printf("INJECT from=%d,%d to=%d,%d\n", x0, y, x1, y);
        fflush(stdout);
        SetCursorPos(x0, y);
        Sleep(350);
        INPUT dn = {};
        dn.type = INPUT_MOUSE;
        dn.mi.dwFlags = MOUSEEVENTF_LEFTDOWN;
        SendInput(1, &dn, sizeof(INPUT));
        Sleep(180);
        for (int s = 1; s <= 24; ++s)
        {
            SetCursorPos(x0 + (x1 - x0) * s / 24, y);
            Sleep(45);
        }
        Sleep(220);
        INPUT up2 = {};
        up2.type = INPUT_MOUSE;
        up2.mi.dwFlags = MOUSEEVENTF_LEFTUP;
        SendInput(1, &up2, sizeof(INPUT));
        printf("INJECT_DONE\n");
        return 0;
    }

    void* h = sh_create();
    if (!h)
    {
        J("{\"ev\":\"fatal\",\"msg\":\"sh_create failed\"}");
        return 1;
    }
    if (passive)
        sh_set_passive_mode(h, 1);

    if (!sh_start(h, OnSelection, OnMouse, OnKey, nullptr))
    {
        J("{\"ev\":\"fatal\",\"msg\":\"sh_start failed\",\"error\":\"%s\"}", sh_last_error(h));
        return 1;
    }
    J("{\"ev\":\"started\",\"running\":%d,\"passive\":%d}", sh_is_running(h), passive ? 1 : 0);

    if (pollMode)
    {
        // Poll getCurrentSelection every 300ms; print when a new selection appears.
        std::string last;
        for (int i = 0; i < 100; ++i)
        {
            SSelectionOut s;
            if (sh_get_current_selection(h, &s) && s.text && s.text[0])
            {
                std::string cur = s.text;
                if (cur != last)
                {
                    last = cur;
                    char t[8192];
                    JsonEsc(s.text, t, sizeof(t));
                    J("{\"ev\":\"poll\",\"text\":\"%s\",\"posLevel\":%d}", t, s.posLevel);
                }
            }
            Sleep(300);
        }
    }
    else
    {
        // Periodically report raw hook counters so we can tell whether the hook is
        // alive without any logging inside the hook callback itself.
        for (int i = 0; i < 200; ++i)
        {
            Sleep(300);
            if (i == 5 && injectMode)
            {
                // Self-contained injection: find the target by class, focus it, then drag
                // across its first text line using SendInput. Doing this in-process removes
                // any cross-process focus/timing ambiguity from the PowerShell path.
                HWND t = FindWindowW(L"SaTestTargetWnd", nullptr);
                if (t)
                {
                    SetForegroundWindow(t);
                    Sleep(400);
                    RECT r;
                    GetWindowRect(t, &r);
                    // Start well inside the text area: starting on the edit border leaves the
                    // cursor as an arrow, which the clipboard fallback rejects by design.
                    int x0 = r.left + 50, x1 = r.left + 520, y = r.top + 91;
                    J("{\"ev\":\"inject\",\"rect\":[%ld,%ld,%ld,%ld],\"from\":[%d,%d],\"to\":[%d,%d]}", r.left, r.top,
                      r.right, r.bottom, x0, y, x1, y);
                    SetCursorPos(x0, y);
                    Sleep(300);
                    INPUT in = {};
                    in.type = INPUT_MOUSE;
                    in.mi.dwFlags = MOUSEEVENTF_LEFTDOWN;
                    SendInput(1, &in, sizeof(INPUT));
                    Sleep(150);
                    for (int s = 1; s <= 20; ++s)
                    {
                        SetCursorPos(x0 + (x1 - x0) * s / 20, y);
                        Sleep(40);
                    }
                    Sleep(200);
                    INPUT up = {};
                    up.type = INPUT_MOUSE;
                    up.mi.dwFlags = MOUSEEVENTF_LEFTUP;
                    SendInput(1, &up, sizeof(INPUT));
                }
                else
                    J("{\"ev\":\"inject\",\"error\":\"target window not found\"}");
            }
            if (i % 3 == 0)
            {
                int d = 0, u = 0, m = 0, w = 0;
                sh_stats(h, &d, &u, &m, &w);
                char dump[1024] = {0};
                sh_dump_events(h, dump, sizeof(dump));
                // Trim the dump so the log stays readable.
                J("{\"ev\":\"stats\",\"down\":%d,\"up\":%d,\"move\":%d,\"wheel\":%d,\"tail\":\"%.300s\"}", d, u, m, w, dump);
            }
        }
    }

    sh_stop(h);
    sh_destroy(h);
    J("{\"ev\":\"stopped\"}");
    return 0;
}

/**
 * Minimal, dialog-free text target for selection testing.
 *
 * Shows a normal top-level window with a multi-line EDIT control pre-filled with
 * known text. Unlike Notepad it prompts for nothing and never opens a save dialog,
 * so it can be left open and dragged safely while the selection hook is tested.
 *
 * Build: g++ -std=c++17 -O2 test_target.cpp -o test_target.exe -luser32 -lgdi32 -mwindows
 */
#define WIN32_LEAN_AND_MEAN 1
#include <windows.h>
#include <string>

static const wchar_t* kText =
    L"The quick brown fox jumps over the lazy dog selection test line one.\r\n"
    L"Second line with some more words so there is plenty to select.\r\n"
    L"Third line ends the sample content.";

static HWND g_edit = nullptr;

static LRESULT CALLBACK WndProc(HWND hwnd, UINT msg, WPARAM wp, LPARAM lp)
{
    switch (msg)
    {
        case WM_CREATE:
        {
            RECT rc;
            GetClientRect(hwnd, &rc);
            g_edit = CreateWindowExW(WS_EX_CLIENTEDGE, L"EDIT", kText,
                                     WS_CHILD | WS_VISIBLE | WS_VSCROLL | ES_MULTILINE | ES_AUTOVSCROLL | ES_LEFT,
                                     10, 10, rc.right - 20, rc.bottom - 20, hwnd, (HMENU)1, GetModuleHandleW(nullptr),
                                     nullptr);
            return 0;
        }
        case WM_SIZE:
            if (g_edit)
                MoveWindow(g_edit, 10, 10, LOWORD(lp) - 20, HIWORD(lp) - 20, TRUE);
            return 0;
        case WM_SETFOCUS:
            if (g_edit)
                SetFocus(g_edit);
            return 0;
        case WM_CLOSE:
            DestroyWindow(hwnd);
            return 0;
        case WM_DESTROY:
            PostQuitMessage(0);
            return 0;
    }
    return DefWindowProcW(hwnd, msg, wp, lp);
}

int WINAPI WinMain(HINSTANCE hInst, HINSTANCE, LPSTR, int)
{
    const wchar_t* cls = L"SaTestTargetWnd";
    WNDCLASSW wc = {};
    wc.lpfnWndProc = WndProc;
    wc.hInstance = hInst;
    wc.lpszClassName = cls;
    wc.hCursor = LoadCursor(nullptr, IDC_ARROW);
    wc.hbrBackground = (HBRUSH)(COLOR_WINDOW + 1);
    RegisterClassW(&wc);

    HWND hwnd = CreateWindowExW(0, cls, L"Selection Test Target", WS_OVERLAPPEDWINDOW, 200, 200, 760, 320, nullptr,
                                nullptr, hInst, nullptr);
    ShowWindow(hwnd, SW_SHOW);
    UpdateWindow(hwnd);
    SetForegroundWindow(hwnd);
    if (g_edit)
        SetFocus(g_edit);

    MSG msg;
    while (GetMessageW(&msg, nullptr, 0, 0) > 0)
    {
        TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
    return 0;
}

/**
 * C ABI over the selection-hook Windows text-selection engine.
 *
 * The engine (core/engine.*, lib/*) is the original MIT-licensed code from
 * selection-hook by 0xfullex. This shim replaces the N-API glue with a plain C
 * interface so a non-Node host (Tauri/Rust) can drive it.
 *
 * Threading: all callbacks are invoked on internal threads. Hosts must be thread-safe.
 * Strings passed to a callback are valid only for the duration of that callback.
 */
#ifndef SELECTION_SHIM_H
#define SELECTION_SHIM_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* --- value types (physical screen coordinates, matching the original API) --- */
typedef struct
{
    int32_t x;
    int32_t y;
} SPoint;

typedef struct
{
    const char* text;        /* UTF-8, valid only during the callback */
    const char* programName; /* UTF-8, valid only during the callback */
    SPoint startTop;
    SPoint startBottom;
    SPoint endTop;
    SPoint endBottom;
    SPoint mousePosStart;
    SPoint mousePosEnd;
    int32_t method;   /* SelectionMethod */
    int32_t posLevel; /* SelectionPositionLevel */
} SSelection;

typedef struct
{
    const char* text;        /* UTF-8, valid only during the callback */
    const char* programName; /* UTF-8, valid only during the callback */
    SPoint startTop;
    SPoint startBottom;
    SPoint endTop;
    SPoint endBottom;
    SPoint mousePosStart;
    SPoint mousePosEnd;
    int32_t method;
    int32_t posLevel;
} SSelectionOut;

/* mouse action codes */
enum
{
    SH_MOUSE_DOWN = 0,
    SH_MOUSE_UP = 1,
    SH_MOUSE_MOVE = 2,
    SH_MOUSE_WHEEL = 3
};

typedef struct
{
    int32_t action; /* SH_MOUSE_* */
    int32_t x;
    int32_t y;
    int32_t button; /* same as WebAPI MouseEvent.button */
    int32_t flag;   /* wheel direction: 1 up/right, -1 down/left */
} SMouseEvent;

typedef struct
{
    int32_t action; /* 0 = key-down, 1 = key-up */
    const char* uniKey;
    int32_t vkCode;
    int32_t sys;
    int32_t flags;
} SKeyEvent;

/* filter modes (mirror FilterMode) */
enum
{
    SH_FILTER_DEFAULT = 0,
    SH_FILTER_INCLUDE = 1,
    SH_FILTER_EXCLUDE = 2
};

/* fine-tuned list types */
enum
{
    SH_FTL_EXCLUDE_CLIPBOARD_CURSOR_DETECT = 0,
    SH_FTL_INCLUDE_CLIPBOARD_DELAY_READ = 1
};

/* selection methods reported in SSelection.method */
enum
{
    SH_METHOD_NONE = 0,
    SH_METHOD_UIA = 1,
    SH_METHOD_ACCESSIBLE = 3,
    SH_METHOD_CLIPBOARD = 99
};

typedef void (*SSelectionCb)(const SSelectionOut* sel, void* user);
typedef void (*SMouseCb)(const SMouseEvent* ev, void* user);
typedef void (*SKeyCb)(const SKeyEvent* ev, void* user);

/* --- lifecycle --- */
void* sh_create(void);
void sh_destroy(void* h);

/**
 * Install hooks and start automatic detection.
 * @return 1 on success, 0 on failure.
 */
int32_t sh_start(void* h, SSelectionCb onSelection, SMouseCb onMouse, SKeyCb onKey, void* user);
void sh_stop(void* h);
int32_t sh_is_running(void* h);

/* --- configuration (safe to call while running) --- */
void sh_set_passive_mode(void* h, int32_t passive);
void sh_set_clipboard_enabled(void* h, int32_t enabled);
void sh_set_global_filter(void* h, int32_t mode, const char* const* list, int32_t count);
void sh_set_clipboard_filter(void* h, int32_t mode, const char* const* list, int32_t count);
void sh_set_fine_tuned(void* h, int32_t listType, const char* const* list, int32_t count);
void sh_enable_mouse_move(void* h, int32_t enable);

/* --- synchronous selection (for hotkey / "get current selection") --- */
/**
 * Retrieve the current selection synchronously. Returns 1 when text was found.
 * On success, `out` fields point into shim-owned storage valid until the next call.
 */
int32_t sh_get_current_selection(void* h, SSelectionOut* out);

/* --- clipboard --- */
int32_t sh_write_clipboard(void* h, const char* utf8);
/** Returns a shim-owned UTF-8 string valid until the next call, or NULL. */
const char* sh_read_clipboard(void* h);

/* --- diagnostics --- */
/** Returns a shim-owned UTF-8 string; never NULL. */
const char* sh_last_error(void* h);

/**
 * Raw low-level hook counters. Safe to poll from any thread.
 *
 * Exists because logging inside a WH_MOUSE_LL callback can exceed Windows'
 * LowLevelHooksTimeout, after which the OS silently removes the hook. Counting is
 * cheap enough to stay inside the callback and lets a host verify the hook is live.
 */
void sh_stats(void* h, int32_t* down, int32_t* up, int32_t* move, int32_t* wheel);

/** Format the last raw hook events into `buf`. Returns bytes written (excluding NUL). */
int32_t sh_dump_events(void* h, char* buf, int32_t buflen);

#ifdef __cplusplus
}
#endif

#endif /* SELECTION_SHIM_H */

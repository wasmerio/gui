// What the test programs share: checks that end the program, and one line of
// text per event record, the same on every host, so that what a program saw
// can be compared with what it should have seen.

#ifndef WGUI_TEST_H_
#define WGUI_TEST_H_

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <wasmer/gui.h>
#include <wasmer/gui_names.h>

#define CHECK(condition)                                                                  \
    do {                                                                                  \
        if (!(condition)) {                                                               \
            fprintf(stderr, "%s:%d: CHECK(%s) failed\n", __FILE__, __LINE__, #condition); \
            exit(1);                                                                      \
        }                                                                                 \
    } while (0)

// A call that has to succeed.
#define OK(call)                                                                \
    do {                                                                        \
        WGUIStatus wt_status = (call);                                          \
        if (wt_status != WGUIStatus_Success) {                                  \
            fprintf(stderr, "%s:%d: %s: %s\n", __FILE__, __LINE__, #call,       \
                    wguiStatusName(wt_status) ? wguiStatusName(wt_status) : "?"); \
            exit(1);                                                            \
        }                                                                       \
    } while (0)

static inline const char* wt_name(const char* name) {
    return name != NULL ? name : "?";
}

// Windows are printed as w1, w2... in the order the program first meets them:
// handles are the host's business.
static WGUIWindow wt_windows[32];
static size_t wt_window_count;

static inline size_t wt_window_number(WGUIWindow window) {
    for (size_t i = 0; i < wt_window_count; i++) {
        if (wt_windows[i] == window) {
            return i + 1;
        }
    }
    CHECK(wt_window_count < sizeof wt_windows / sizeof wt_windows[0]);
    wt_windows[wt_window_count++] = window;
    return wt_window_count;
}

static inline void wt_print_text(const char* data, size_t length) {
    putchar('"');
    for (size_t i = 0; i < length; i++) {
        unsigned char c = (unsigned char)data[i];
        if (c == '"' || c == '\\') {
            printf("\\%c", c);
        } else if (c < 0x20 || c == 0x7F) {
            printf("\\x%02X", c);
        } else {
            putchar(c);
        }
    }
    putchar('"');
}

static inline void wt_print_string(const WGUIEventHeader* event, WGUIEventString string) {
    WGUIStringView view = wguiEventGetString(event, string);
    wt_print_text(view.data, view.length);
}

#define WT_PRINT_FLAGS(Type, value)                                    \
    do {                                                               \
        int wt_first = 1;                                              \
        for (size_t wt_i = 0; wt_i < wgui##Type##Count(); wt_i++) {    \
            if ((value) & wgui##Type##Value(wt_i)) {                   \
                printf("%s%s", wt_first ? "" : "|", wgui##Type##Name(wt_i)); \
                wt_first = 0;                                          \
            }                                                          \
        }                                                              \
        if (wt_first) {                                                \
            printf("0");                                               \
        }                                                              \
    } while (0)

static inline void wt_print_scalar(uint32_t scalar) {
    char utf8[5] = {0};
    if (scalar < 0x80) {
        utf8[0] = (char)scalar;
    } else if (scalar < 0x800) {
        utf8[0] = (char)(0xC0 | (scalar >> 6));
        utf8[1] = (char)(0x80 | (scalar & 0x3F));
    } else if (scalar < 0x10000) {
        utf8[0] = (char)(0xE0 | (scalar >> 12));
        utf8[1] = (char)(0x80 | ((scalar >> 6) & 0x3F));
        utf8[2] = (char)(0x80 | (scalar & 0x3F));
    } else {
        utf8[0] = (char)(0xF0 | (scalar >> 18));
        utf8[1] = (char)(0x80 | ((scalar >> 12) & 0x3F));
        utf8[2] = (char)(0x80 | ((scalar >> 6) & 0x3F));
        utf8[3] = (char)(0x80 | (scalar & 0x3F));
    }
    if (scalar < 0x20 || scalar == 0x7F) {
        printf("U+%04X", scalar);
    } else {
        printf("'%s'", utf8);
    }
}

static inline void wt_print_key(WGUIKey key) {
    uint32_t value = WGUI_KEY_VALUE(key);
    switch (WGUI_KEY_TAG(key)) {
        case WGUI_KEY_CHARACTER:
            if (value == 0) {
                printf("Unidentified");
            } else {
                wt_print_scalar(value);
            }
            break;
        case WGUI_KEY_NAMED:
            printf("%s", wt_name(wguiKeyName(key)));
            break;
        case WGUI_KEY_DEAD:
            printf("Dead(");
            if (value != 0) {
                printf("U+%04X", value);
            }
            printf(")");
            break;
        case WGUI_KEY_SEQUENCE:
            printf("Sequence(U+%04X..)", value);
            break;
        default:
            printf("0x%08X", key);
            break;
    }
}

static inline void wt_print_pointer(const WGUIPointerEvent* p) {
    printf(" id=%u %s flags=", p->pointerId, wt_name(wguiPointerKindName(p->pointerKind)));
    WT_PRINT_FLAGS(PointerFlags, p->pointerFlags);
    if (p->button != 0) {
        printf(" button=%u", (unsigned)p->button);
    }
    printf(" buttons=");
    WT_PRINT_FLAGS(PointerButtons, p->buttons);
    printf(" at=%g,%g delta=%g,%g pressure=%g", p->x, p->y, p->deltaX, p->deltaY, (double)p->pressure);
    if (p->modifiers != 0) {
        printf(" mods=");
        WT_PRINT_FLAGS(Modifiers, p->modifiers);
    }
    if (p->clickCount != 0) {
        printf(" clicks=%u", p->clickCount);
    }
}

// One line for one record.
static inline void wt_print_event(const WGUIEventHeader* event) {
    const char* name = wguiEventTypeName(event->type);
    if (name == NULL) {
        printf("Event(0x%04X) size=%u\n", (unsigned)event->type, event->size);
        return;
    }
    printf("%s", name);
    if (event->window != 0) {
        printf(" w%zu", wt_window_number(event->window));
    }
    switch (event->type) {
        case WGUIEventType_EventsLost: {
            const WGUIEventsLostEvent* e = (const WGUIEventsLostEvent*)event;
            printf(" count=%u classes=", e->count);
            WT_PRINT_FLAGS(EventClass, e->classes);
            break;
        }
        case WGUIEventType_RequestCompleted: {
            const WGUIRequestCompletedEvent* e = (const WGUIRequestCompletedEvent*)event;
            printf(" tag=%llu kind=%s status=%s", (unsigned long long)e->tag,
                   wt_name(wguiRequestKindName(e->kind)), wt_name(wguiStatusName(e->status)));
            break;
        }
        case WGUIEventType_AppThemeChanged:
        case WGUIEventType_WindowThemeChanged: {
            const WGUIThemeEvent* e = (const WGUIThemeEvent*)event;
            printf(" theme=%s", wt_name(wguiThemeName(e->theme)));
            break;
        }
        case WGUIEventType_WindowResized: {
            const WGUIWindowResizedEvent* e = (const WGUIWindowResizedEvent*)event;
            printf(" %ux%u scale=%g changed=", e->width, e->height, e->scaleFactor);
            WT_PRINT_FLAGS(ResizeFlags, e->changed);
            break;
        }
        case WGUIEventType_WindowMoved: {
            const WGUIWindowMovedEvent* e = (const WGUIWindowMovedEvent*)event;
            printf(" %d,%d", e->x, e->y);
            break;
        }
        case WGUIEventType_WindowFlagsChanged: {
            const WGUIWindowFlagsChangedEvent* e = (const WGUIWindowFlagsChangedEvent*)event;
            printf(" flags=");
            WT_PRINT_FLAGS(WindowFlags, e->flags);
            printf(" changed=");
            WT_PRINT_FLAGS(WindowFlags, e->changed);
            break;
        }
        case WGUIEventType_WindowDisplayChanged: {
            const WGUIWindowDisplayChangedEvent* e = (const WGUIWindowDisplayChangedEvent*)event;
            printf(" display=%s", e->display != 0 ? "known" : "unknown");
            break;
        }
        case WGUIEventType_WindowCursorGrabChanged: {
            const WGUIWindowCursorGrabChangedEvent* e = (const WGUIWindowCursorGrabChangedEvent*)event;
            printf(" grab=%s reason=%s", wt_name(wguiCursorGrabName(e->grab)),
                   wt_name(wguiCursorGrabReasonName(e->reason)));
            break;
        }
        case WGUIEventType_KeyDown:
        case WGUIEventType_KeyUp: {
            const WGUIKeyEvent* e = (const WGUIKeyEvent*)event;
            printf(" %s key=", wt_name(wguiPhysicalKeyName(e->physicalKey)));
            wt_print_key(e->key);
            if (e->keyUnmodified != 0 && e->keyUnmodified != e->key) {
                printf(" base=");
                wt_print_key(e->keyUnmodified);
            }
            if (e->modifiers != 0) {
                printf(" mods=");
                WT_PRINT_FLAGS(Modifiers, e->modifiers);
            }
            if (e->keyFlags != 0) {
                printf(" flags=");
                WT_PRINT_FLAGS(KeyFlags, e->keyFlags);
            }
            if (e->location != WGUIKeyLocation_Standard) {
                printf(" location=%s", wt_name(wguiKeyLocationName(e->location)));
            }
            if (e->text.length != 0) {
                printf(" text=");
                wt_print_string(event, e->text);
            }
            if (e->textWithAllModifiers.length != 0) {
                printf(" raw=");
                wt_print_string(event, e->textWithAllModifiers);
            }
            break;
        }
        case WGUIEventType_ModifiersChanged: {
            const WGUIModifiersChangedEvent* e = (const WGUIModifiersChangedEvent*)event;
            printf(" mods=");
            WT_PRINT_FLAGS(Modifiers, e->modifiers);
            break;
        }
        case WGUIEventType_TextInputState: {
            const WGUITextInputStateEvent* e = (const WGUITextInputStateEvent*)event;
            printf(" state=");
            WT_PRINT_FLAGS(TextInputStateFlags, e->stateFlags);
            break;
        }
        case WGUIEventType_TextComposition: {
            const WGUITextCompositionEvent* e = (const WGUITextCompositionEvent*)event;
            putchar(' ');
            wt_print_string(event, e->text);
            if (e->selectionStart == WGUI_NO_TEXT_CURSOR) {
                printf(" cursor=none");
            } else {
                printf(" cursor=%u..%u", e->selectionStart, e->selectionEnd);
            }
            break;
        }
        case WGUIEventType_TextCommit: {
            const WGUITextCommitEvent* e = (const WGUITextCommitEvent*)event;
            putchar(' ');
            wt_print_string(event, e->text);
            if (e->textFlags != 0) {
                printf(" flags=");
                WT_PRINT_FLAGS(TextFlags, e->textFlags);
            }
            break;
        }
        case WGUIEventType_PointerEnter:
        case WGUIEventType_PointerLeave:
        case WGUIEventType_PointerMotion:
        case WGUIEventType_PointerDown:
        case WGUIEventType_PointerUp:
        case WGUIEventType_PointerCancel:
            wt_print_pointer((const WGUIPointerEvent*)event);
            break;
        case WGUIEventType_Wheel: {
            const WGUIWheelEvent* e = (const WGUIWheelEvent*)event;
            printf(" %s phase=%s delta=%g,%g at=%g,%g", wt_name(wguiWheelUnitName(e->unit)),
                   wt_name(wguiWheelPhaseName(e->phase)), e->deltaX, e->deltaY, e->x, e->y);
            break;
        }
        case WGUIEventType_RawMotion: {
            const WGUIRawMotionEvent* e = (const WGUIRawMotionEvent*)event;
            printf(" delta=%g,%g", e->deltaX, e->deltaY);
            break;
        }
        case WGUIEventType_RawButton: {
            const WGUIRawButtonEvent* e = (const WGUIRawButtonEvent*)event;
            printf(" button=%u %s", (unsigned)e->button, e->pressed ? "down" : "up");
            break;
        }
        case WGUIEventType_RawWheel: {
            const WGUIRawWheelEvent* e = (const WGUIRawWheelEvent*)event;
            printf(" %s delta=%g,%g", wt_name(wguiWheelUnitName(e->unit)), e->deltaX, e->deltaY);
            break;
        }
        case WGUIEventType_RawKey: {
            const WGUIRawKeyEvent* e = (const WGUIRawKeyEvent*)event;
            printf(" %s %s", wt_name(wguiPhysicalKeyName(e->physicalKey)), e->pressed ? "down" : "up");
            break;
        }
        case WGUIEventType_ClipboardChanged: {
            const WGUIClipboardChangedEvent* e = (const WGUIClipboardChangedEvent*)event;
            printf(" %s flags=", wt_name(wguiClipboardSelectionName(e->selection)));
            WT_PRINT_FLAGS(ClipboardFlags, e->clipboardFlags);
            break;
        }
        default:
            break;
    }
    if (event->flags != 0) {
        printf(" [");
        WT_PRINT_FLAGS(EventFlags, event->flags);
        printf("]");
    }
    putchar('\n');
}

// Blocks until there is something to read. A wait the host cut short, which
// it does for signals, is a wait to make again.
static inline void wt_wait(WGUIInstance instance) {
    for (;;) {
        WGUIWaitReasons reasons = 0;
        OK(wguiInstanceWaitEvents(instance, WGUI_WAIT_FOREVER, &reasons));
        if (reasons & (WGUIWaitReasons_Events | WGUIWaitReasons_Woken)) {
            return;
        }
    }
}

#endif // WGUI_TEST_H_

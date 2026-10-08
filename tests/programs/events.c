// Opens a window and prints every event it gets, one line each, until the
// window is asked to close. The function keys make it use the rest of the
// API, so that a script of input can walk it through all of it:
//
//   F1  fullscreen on and off       F2  lock the cursor     F3  let it go
//   F4  text entry on and off       F5  copy a text         F6  ask for a redraw
//   F7  ask for 640x480             F8  maximize            F9  quit
//
// and the platform's paste shortcut (Control or Super with V) pastes.
//
// Arguments: --raw (also ask for device events), --text (start with text
// entry on), --target <name>, --size <width>x<height>, --logical.

#include "wgui_test.h"

static WGUIInstance instance;
static WGUIWindow window;
static uint32_t shortcut;
static uint64_t next_tag = 1;
static int fullscreen;
static int text_entry;
static int running = 1;

static void set_text_entry(int on) {
    WGUITextInputDescriptor descriptor = WGUI_TEXT_INPUT_DESCRIPTOR_INIT;
    descriptor.cursorArea.x = 10;
    descriptor.cursorArea.y = 20;
    descriptor.cursorArea.width = 2;
    descriptor.cursorArea.height = 16;
    OK(wguiWindowSetTextInput(window, on ? &descriptor : NULL));
    text_entry = on;
}

static void paste(void) {
    char text[256];
    size_t size = 0;
    WGUIStatus status = wguiInstanceReadClipboard(instance, WGUIClipboardSelection_Clipboard, "text/plain",
                                                  WGUI_STRLEN, text, sizeof text, &size);
    if (status == WGUIStatus_Success) {
        printf("pasted ");
        wt_print_text(text, size);
        putchar('\n');
    } else if (status == WGUIStatus_Pending) {
        uint64_t tag = next_tag++;
        printf("paste has to ask first, tag=%llu\n", (unsigned long long)tag);
        OK(wguiInstanceRequestClipboard(instance, WGUIClipboardSelection_Clipboard, "text/plain", WGUI_STRLEN,
                                        tag));
    } else {
        printf("paste: %s\n", wt_name(wguiStatusName(status)));
    }
}

static void on_key(const WGUIKeyEvent* key) {
    if (key->keyFlags & WGUIKeyFlags_Repeat) {
        return;
    }
    switch (key->physicalKey) {
        case WGUIPhysicalKey_F1: {
            WGUIFullscreenOptions options = WGUI_FULLSCREEN_OPTIONS_INIT;
            fullscreen = !fullscreen;
            OK(wguiWindowSetFullscreen(window, fullscreen ? &options : NULL, next_tag++));
            break;
        }
        case WGUIPhysicalKey_F2:
            OK(wguiWindowSetCursorGrab(window, WGUICursorGrab_Locked, next_tag++));
            break;
        case WGUIPhysicalKey_F3:
            OK(wguiWindowSetCursorGrab(window, WGUICursorGrab_None, next_tag++));
            break;
        case WGUIPhysicalKey_F4:
            set_text_entry(!text_entry);
            break;
        case WGUIPhysicalKey_F5: {
            static const char text[] = "copied by the guest";
            WGUIClipboardItem item = WGUI_CLIPBOARD_ITEM_INIT;
            item.mimeType.data = "text/plain";
            item.data = text;
            item.size = sizeof text - 1;
            OK(wguiInstanceWriteClipboard(instance, WGUIClipboardSelection_Clipboard, 1, &item, next_tag++));
            break;
        }
        case WGUIPhysicalKey_KeyV:
            if (key->modifiers & shortcut) {
                paste();
            }
            break;
        case WGUIPhysicalKey_F6:
            OK(wguiWindowRequestRedraw(window));
            break;
        case WGUIPhysicalKey_F7:
            OK(wguiWindowSetSize(window, 640, 480));
            break;
        case WGUIPhysicalKey_F8: {
            WGUIWindowFlags unsupported = 0;
            OK(wguiWindowSetFlags(window, WGUIWindowFlags_Maximized, WGUIWindowFlags_Maximized, &unsupported));
            break;
        }
        case WGUIPhysicalKey_F9:
            running = 0;
            break;
        default:
            break;
    }
}

int main(int argc, char** argv) {
    WGUIInstanceDescriptor instance_descriptor = WGUI_INSTANCE_DESCRIPTOR_INIT;
    WGUIWindowDescriptor window_descriptor = WGUI_WINDOW_DESCRIPTOR_INIT;
    int start_with_text = 0;
    setvbuf(stdout, NULL, _IOLBF, 0);

    window_descriptor.title.data = "events";
    for (int i = 1; i < argc; i++) {
        if (strcmp(argv[i], "--raw") == 0) {
            instance_descriptor.eventMask |= WGUIEventClass_Raw;
        } else if (strcmp(argv[i], "--text") == 0) {
            start_with_text = 1;
        } else if (strcmp(argv[i], "--logical") == 0) {
            window_descriptor.options |= WGUIWindowOptions_LogicalUnits;
        } else if (strcmp(argv[i], "--target") == 0 && i + 1 < argc) {
            window_descriptor.target.data = argv[++i];
        } else if (strcmp(argv[i], "--size") == 0 && i + 1 < argc) {
            unsigned width = 0, height = 0;
            CHECK(sscanf(argv[++i], "%ux%u", &width, &height) == 2);
            window_descriptor.width = width;
            window_descriptor.height = height;
        } else {
            fprintf(stderr, "events: unknown argument %s\n", argv[i]);
            return 2;
        }
    }

    OK(wguiCreateInstance(&instance_descriptor, &instance));
    WGUIInstanceInfo info = WGUI_INSTANCE_INFO_INIT;
    OK(wguiInstanceGetInfo(instance, &info));
    shortcut = info.shortcutModifier;
    printf("host %s, version %u.%u\n", wt_name(wguiHostKindName(info.hostKind)), info.abiVersion >> 16,
           info.abiVersion & 0xFFFFU);

    OK(wguiInstanceCreateWindow(instance, &window_descriptor, &window));
    CHECK(wt_window_number(window) == 1);
    if (start_with_text) {
        set_text_entry(1);
    }

    uint64_t buffer[2 * WGUI_MAX_EVENT_SIZE / 8];
    while (running) {
        wt_wait(instance);
        for (;;) {
            size_t written = 0;
            OK(wguiInstancePollEvents(instance, buffer, sizeof buffer, &written));
            if (written == 0) {
                break;
            }
            const WGUIEventHeader* event = wguiEventFirst(buffer, written);
            for (; event != NULL; event = wguiEventNext(buffer, written, event)) {
                wt_print_event(event);
                switch (event->type) {
                    case WGUIEventType_KeyDown:
                        on_key((const WGUIKeyEvent*)event);
                        break;
                    case WGUIEventType_RequestCompleted: {
                        const WGUIRequestCompletedEvent* done = (const WGUIRequestCompletedEvent*)event;
                        if (done->kind == WGUIRequestKind_ClipboardRequest && done->status == WGUIStatus_Success) {
                            paste();
                        }
                        break;
                    }
                    case WGUIEventType_WindowCloseRequested:
                        running = 0;
                        break;
                    default:
                        break;
                }
            }
        }
    }

    WGUIWindowState state = WGUI_WINDOW_STATE_INIT;
    OK(wguiWindowGetState(window, &state));
    printf("closing at %ux%u, flags=", state.width, state.height);
    WT_PRINT_FLAGS(WindowFlags, state.flags);
    putchar('\n');
    wguiWindowDestroy(window);
    wguiInstanceDestroy(instance);
    printf("done\n");
    return 0;
}

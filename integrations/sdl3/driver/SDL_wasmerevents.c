/*
  The events of the "wasmer" video driver: the host's event records, read
  when SDL pumps or waits, as SDL's own events.
*/
#include "SDL_internal.h"

#ifdef SDL_VIDEO_DRIVER_WASMER

#include "SDL_wasmervideo.h"
#include "../SDL_clipboard_c.h"
#include "../../events/SDL_events_c.h"
#include "../../events/SDL_keyboard_c.h"
#include "../../events/SDL_mouse_c.h"
#include "../../events/SDL_touch_c.h"

// Room for records to be read into: a few of the largest at a time. A
// buffer of 64-bit words is aligned as records have to be.
static uint64_t wasmer_records[4 * WGUI_MAX_EVENT_SIZE / 8];

// A wheel that reports pixels (a touchpad, a browser) turns this many for
// what SDL counts as one notch.
#define WASMER_PIXELS_PER_NOTCH 100.0

static SDL_Window *WASMER_FindWindow(SDL_VideoDevice *_this, WGUIWindow handle)
{
    for (SDL_Window *window = _this->windows; window; window = window->next) {
        if (window->internal && window->internal->window == handle) {
            return window;
        }
    }
    return NULL;
}

// SDL scancodes are the usages of the USB keyboard page, as physical keys
// of that page are. The few SDL has from other pages go by this table.
static SDL_Scancode WASMER_Scancode(WGUIPhysicalKey key)
{
    static const struct
    {
        WGUIPhysicalKey key;
        SDL_Scancode scancode;
    } others[] = {
        { WGUIPhysicalKey_Sleep, SDL_SCANCODE_SLEEP },
        { WGUIPhysicalKey_WakeUp, SDL_SCANCODE_WAKE },
        { WGUIPhysicalKey_MediaPlay, SDL_SCANCODE_MEDIA_PLAY },
        { WGUIPhysicalKey_MediaPause, SDL_SCANCODE_MEDIA_PAUSE },
        { WGUIPhysicalKey_MediaRecord, SDL_SCANCODE_MEDIA_RECORD },
        { WGUIPhysicalKey_MediaFastForward, SDL_SCANCODE_MEDIA_FAST_FORWARD },
        { WGUIPhysicalKey_MediaRewind, SDL_SCANCODE_MEDIA_REWIND },
        { WGUIPhysicalKey_MediaTrackNext, SDL_SCANCODE_MEDIA_NEXT_TRACK },
        { WGUIPhysicalKey_MediaTrackPrevious, SDL_SCANCODE_MEDIA_PREVIOUS_TRACK },
        { WGUIPhysicalKey_MediaStop, SDL_SCANCODE_MEDIA_STOP },
        { WGUIPhysicalKey_Eject, SDL_SCANCODE_MEDIA_EJECT },
        { WGUIPhysicalKey_MediaPlayPause, SDL_SCANCODE_MEDIA_PLAY_PAUSE },
        { WGUIPhysicalKey_MediaSelect, SDL_SCANCODE_MEDIA_SELECT },
        { WGUIPhysicalKey_BrowserSearch, SDL_SCANCODE_AC_SEARCH },
        { WGUIPhysicalKey_BrowserHome, SDL_SCANCODE_AC_HOME },
        { WGUIPhysicalKey_BrowserBack, SDL_SCANCODE_AC_BACK },
        { WGUIPhysicalKey_BrowserForward, SDL_SCANCODE_AC_FORWARD },
        { WGUIPhysicalKey_BrowserStop, SDL_SCANCODE_AC_STOP },
        { WGUIPhysicalKey_BrowserRefresh, SDL_SCANCODE_AC_REFRESH },
        { WGUIPhysicalKey_BrowserFavorites, SDL_SCANCODE_AC_BOOKMARKS },
    };
    const uint32_t page = (uint32_t)key >> 16;
    const uint32_t usage = (uint32_t)key & 0xFFFF;

    if (page == 0x07 && usage >= SDL_SCANCODE_A && usage <= SDL_SCANCODE_RGUI) {
        return (SDL_Scancode)usage;
    }
    for (size_t i = 0; i < SDL_arraysize(others); i++) {
        if (others[i].key == key) {
            return others[i].scancode;
        }
    }
    return SDL_SCANCODE_UNKNOWN;
}

// A copy of a record's string that ends where C strings end. The caller frees it.
static char *WASMER_String(const WGUIEventHeader *event, WGUIEventString string)
{
    WGUIStringView view = wguiEventGetString(event, string);
    char *text = (char *)SDL_malloc(view.length + 1);
    if (text) {
        if (view.length) {
            SDL_memcpy(text, view.data, view.length);
        }
        text[view.length] = '\0';
    }
    return text;
}

// How many characters the first `bytes` bytes of `text` are.
static int WASMER_Characters(const char *text, uint32_t bytes)
{
    int count = 0;
    for (uint32_t i = 0; i < bytes && text[i]; i++) {
        if (((unsigned char)text[i] & 0xC0) != 0x80) {
            count++;
        }
    }
    return count;
}

static void WASMER_WindowResized(SDL_Window *window, const WGUIWindowResizedEvent *size)
{
    SDL_WindowData *data = window->internal;
    const double scale = size->scaleFactor > 0.0 ? size->scaleFactor : 1.0;

    data->width = size->width;
    data->height = size->height;
    data->scale = scale;
    WASMER_UpdateSafeArea(window);
    // SDL counts windows in the host's logical units. It asks for the size
    // in pixels itself, also when only the scale factor changed, and tells
    // the program.
    SDL_SendWindowEvent(window, SDL_EVENT_WINDOW_RESIZED, (int)SDL_lround(size->width / scale), (int)SDL_lround(size->height / scale));
    // A window that is new or has another size has nothing drawn in it yet.
    SDL_SendWindowEvent(window, SDL_EVENT_WINDOW_EXPOSED, 0, 0);
}

static void WASMER_WindowFlags(SDL_Window *window, const WGUIWindowFlagsChangedEvent *change)
{
    SDL_WindowData *data = window->internal;
    const WGUIWindowFlags flags = change->flags;
    const WGUIWindowFlags changed = change->changed;
    const bool fullscreen = (flags & WGUIWindowFlags_Fullscreen) != 0;

    data->flags = flags;
    if (changed & WGUIWindowFlags_Visible) {
        SDL_SendWindowEvent(window, (flags & WGUIWindowFlags_Visible) ? SDL_EVENT_WINDOW_SHOWN : SDL_EVENT_WINDOW_HIDDEN, 0, 0);
    }
    if ((changed & WGUIWindowFlags_Fullscreen) && !fullscreen) {
        // What SDL was told is pending is settled by this. The user can
        // also have done it: SDL's own books are brought in line either way.
        data->fullscreen_tag = 0;
        SDL_SendWindowEvent(window, SDL_EVENT_WINDOW_LEAVE_FULLSCREEN, 0, 0);
        SDL_UpdateFullscreenMode(window, SDL_FULLSCREEN_OP_LEAVE, false);
    }
    if (changed & (WGUIWindowFlags_Minimized | WGUIWindowFlags_Maximized | WGUIWindowFlags_Fullscreen)) {
        // To the host a window can be maximized and fullscreen at once. To
        // SDL a fullscreen window is not maximized: it is that again when it
        // leaves fullscreen.
        if (flags & WGUIWindowFlags_Minimized) {
            SDL_SendWindowEvent(window, SDL_EVENT_WINDOW_MINIMIZED, 0, 0);
        } else if ((flags & WGUIWindowFlags_Maximized) && !fullscreen) {
            SDL_SendWindowEvent(window, SDL_EVENT_WINDOW_MAXIMIZED, 0, 0);
        } else {
            SDL_SendWindowEvent(window, SDL_EVENT_WINDOW_RESTORED, 0, 0);
        }
    }
    if ((changed & WGUIWindowFlags_Fullscreen) && fullscreen) {
        data->fullscreen_tag = 0;
        SDL_SendWindowEvent(window, SDL_EVENT_WINDOW_ENTER_FULLSCREEN, 0, 0);
        SDL_UpdateFullscreenMode(window, SDL_FULLSCREEN_OP_ENTER, false);
    }
    if (changed & WGUIWindowFlags_Occluded) {
        SDL_SendWindowEvent(window, (flags & WGUIWindowFlags_Occluded) ? SDL_EVENT_WINDOW_OCCLUDED : SDL_EVENT_WINDOW_EXPOSED, 0, 0);
    }
}

static void WASMER_Key(SDL_VideoDevice *_this, Uint64 timestamp, const WGUIKeyEvent *key, bool down)
{
    const SDL_Scancode scancode = WASMER_Scancode(key->physicalKey);
    // The key the layout has there without modifiers is what SDL calls the
    // keycode; a host that cannot tell gives the key with them.
    const WGUIKey base = key->keyUnmodified ? key->keyUnmodified : key->key;
    const uint32_t value = WGUI_KEY_VALUE(base);

    if (down && (key->keyFlags & WGUIKeyFlags_Composing)) {
        // The input method took this key: what comes of it arrives as text.
        return;
    }
    if (WGUI_KEY_TAG(base) == WGUI_KEY_CHARACTER && value != 0) {
        SDL_Keycode keycode = (SDL_Keycode)value;
        if (keycode >= 'A' && keycode <= 'Z') {
            keycode += 'a' - 'A';
        }
        SDL_SendKeyboardKeyAndKeycode(timestamp, SDL_DEFAULT_KEYBOARD_ID, (int)key->nativeScancode, scancode, keycode, down);
    } else if (scancode != SDL_SCANCODE_UNKNOWN) {
        // A named key: SDL's own table says what it is.
        SDL_SendKeyboardKey(timestamp, SDL_DEFAULT_KEYBOARD_ID, (int)key->nativeScancode, scancode, down);
    }
}

static Uint8 WASMER_Button(WGUIPointerButton button)
{
    switch (button) {
    case WGUIPointerButton_Primary: return SDL_BUTTON_LEFT;
    case WGUIPointerButton_Auxiliary: return SDL_BUTTON_MIDDLE;
    case WGUIPointerButton_Secondary: return SDL_BUTTON_RIGHT;
    case WGUIPointerButton_Back: return SDL_BUTTON_X1;
    case WGUIPointerButton_Forward: return SDL_BUTTON_X2;
    // A pen's eraser, and whatever a later host adds.
    default: return (Uint8)SDL_min((int)button, 31);
    }
}

static void WASMER_Pointer(SDL_VideoDevice *_this, SDL_Window *window, Uint64 timestamp, const WGUIPointerEvent *pointer)
{
    SDL_VideoData *video = _this->internal;
    SDL_WindowData *data = window->internal;
    const WGUIEventType type = pointer->header.type;
    const double scale = WASMER_Scale(window);
    const float x = (float)(pointer->x / scale);
    const float y = (float)(pointer->y / scale);

    if (pointer->pointerKind == WGUIPointerKind_Touch) {
        const SDL_FingerID finger = (SDL_FingerID)pointer->pointerId + 1;
        // SDL wants where a finger is as a fraction of the window.
        const float fx = data->width ? (float)(pointer->x / data->width) : 0.0f;
        const float fy = data->height ? (float)(pointer->y / data->height) : 0.0f;

        if (!video->touch_added) {
            video->touch_added = SDL_AddTouch(WASMER_TOUCH_ID, SDL_TOUCH_DEVICE_DIRECT, "touch") >= 0;
        }
        switch (type) {
        case WGUIEventType_PointerDown:
            SDL_SendTouch(timestamp, WASMER_TOUCH_ID, finger, window, SDL_EVENT_FINGER_DOWN, fx, fy, pointer->pressure);
            break;
        case WGUIEventType_PointerUp:
            SDL_SendTouch(timestamp, WASMER_TOUCH_ID, finger, window, SDL_EVENT_FINGER_UP, fx, fy, pointer->pressure);
            break;
        case WGUIEventType_PointerCancel:
            SDL_SendTouch(timestamp, WASMER_TOUCH_ID, finger, window, SDL_EVENT_FINGER_CANCELED, fx, fy, pointer->pressure);
            break;
        case WGUIEventType_PointerMotion:
            if (pointer->pointerFlags & WGUIPointerFlags_InContact) {
                SDL_SendTouchMotion(timestamp, WASMER_TOUCH_ID, finger, window, fx, fy, pointer->pressure);
            }
            break;
        default:
            break;
        }
        return;
    }

    // The mouse, and a pen, which SDL is told of as one for now.
    switch (type) {
    case WGUIEventType_PointerEnter:
        SDL_SetMouseFocus(window);
        if (!data->locked) {
            SDL_SendMouseMotion(timestamp, window, SDL_DEFAULT_MOUSE_ID, false, x, y);
        }
        break;
    case WGUIEventType_PointerLeave:
        if (SDL_GetMouse()->focus == window) {
            SDL_SetMouseFocus(NULL);
        }
        break;
    case WGUIEventType_PointerMotion:
        if (data->locked) {
            // A locked cursor does not move: how far the mouse did is all
            // there is.
            if (pointer->deltaX != 0.0 || pointer->deltaY != 0.0) {
                SDL_SendMouseMotion(timestamp, window, SDL_DEFAULT_MOUSE_ID, true, (float)(pointer->deltaX / scale), (float)(pointer->deltaY / scale));
            }
        } else {
            SDL_SendMouseMotion(timestamp, window, SDL_DEFAULT_MOUSE_ID, false, x, y);
        }
        break;
    case WGUIEventType_PointerDown:
    case WGUIEventType_PointerUp:
        if (!data->locked) {
            SDL_SendMouseMotion(timestamp, window, SDL_DEFAULT_MOUSE_ID, false, x, y);
            if (type == WGUIEventType_PointerDown) {
                WASMER_Relock(_this, window);
            }
        }
        if (pointer->button != WGUIPointerButton_None) {
            SDL_SendMouseButton(timestamp, window, SDL_DEFAULT_MOUSE_ID, WASMER_Button(pointer->button), type == WGUIEventType_PointerDown);
        }
        break;
    case WGUIEventType_PointerCancel:
        // Whatever was held is let go of.
        for (Uint8 button = SDL_BUTTON_LEFT; button <= SDL_BUTTON_X2; button++) {
            SDL_SendMouseButton(timestamp, window, SDL_DEFAULT_MOUSE_ID, button, false);
        }
        break;
    default:
        break;
    }
}

static void WASMER_Wheel(SDL_Window *window, Uint64 timestamp, const WGUIWheelEvent *wheel)
{
    const double per_notch = wheel->unit == WGUIWheelUnit_Pixel ? WASMER_PIXELS_PER_NOTCH : 1.0;
    // The host counts what comes into view: down and to the right are
    // positive. SDL counts a wheel turned away from the user as positive.
    SDL_SendMouseWheel(timestamp, window, SDL_DEFAULT_MOUSE_ID, (float)(wheel->deltaX / per_notch), (float)(-wheel->deltaY / per_notch), SDL_MOUSEWHEEL_NORMAL);
}

static void WASMER_ClipboardChanged(const WGUIClipboardChangedEvent *change)
{
    static const char *const text_types[] = { "text/plain;charset=utf-8", "text/plain" };
    // A host that has to ask for the clipboard first (a browser) cannot say
    // what is on it: there may be text, and a paste finds out.
    const bool has_text = (change->clipboardFlags & (WGUIClipboardFlags_HasText | WGUIClipboardFlags_NeedsRequest)) != 0;
    const bool owned = (change->clipboardFlags & WGUIClipboardFlags_Owned) != 0;
    char **types;

    if (change->selection != WGUIClipboardSelection_Clipboard || owned) {
        // What the program put there itself, SDL already knows.
        return;
    }
    types = has_text ? SDL_CopyClipboardMimeTypes(text_types, SDL_arraysize(text_types), true) : NULL;
    SDL_SendClipboardUpdate(false, types, (has_text && types) ? SDL_arraysize(text_types) : 0);
}

static void WASMER_TextInputState(SDL_Window *window, const WGUITextInputStateEvent *state)
{
    SDL_WindowData *data = window->internal;
    const bool shown = (state->stateFlags & WGUITextInputStateFlags_ScreenKeyboardVisible) != 0;

    if (shown != data->screen_keyboard_shown) {
        data->screen_keyboard_shown = shown;
        if (shown) {
            SDL_SendScreenKeyboardShown();
        } else {
            SDL_SendScreenKeyboardHidden();
        }
    }
}

static void WASMER_Dispatch(SDL_VideoDevice *_this, const WGUIEventHeader *event)
{
    SDL_VideoData *video = _this->internal;
    SDL_Window *window = event->window ? WASMER_FindWindow(_this, event->window) : NULL;
    const Uint64 timestamp = (Uint64)((Sint64)event->timestamp + video->clock_offset);

    switch (event->type) {
    case WGUIEventType_RequestCompleted: {
        const WGUIRequestCompletedEvent *done = (const WGUIRequestCompletedEvent *)event;
        if (done->tag > video->last_done) {
            video->last_done = done->tag;
        }
        if (done->tag == video->clipboard_tag) {
            video->clipboard_tag = 0;
            video->clipboard_status = done->status;
        }
        if (window && window->internal->fullscreen_tag == done->tag) {
            SDL_WindowData *data = window->internal;
            const bool is_fullscreen = (data->flags & WGUIWindowFlags_Fullscreen) != 0;
            data->fullscreen_tag = 0;
            // A request that came to nothing leaves the window as it was,
            // and SDL waiting: it is told what the window is.
            if (done->status != WGUIStatus_Success && is_fullscreen != data->fullscreen_wanted) {
                SDL_SendWindowEvent(window, is_fullscreen ? SDL_EVENT_WINDOW_ENTER_FULLSCREEN : SDL_EVENT_WINDOW_LEAVE_FULLSCREEN, 0, 0);
                SDL_UpdateFullscreenMode(window, is_fullscreen ? SDL_FULLSCREEN_OP_ENTER : SDL_FULLSCREEN_OP_LEAVE, false);
            }
        }
        if (window && done->kind == WGUIRequestKind_CursorGrab && done->status != WGUIStatus_Success && done->status != WGUIStatus_Cancelled) {
            // The host would not: the next time SDL wants it, it is asked again.
            window->internal->grab_wanted = WGUICursorGrab_None;
        }
        break;
    }
    case WGUIEventType_AppSuspended:
        SDL_SendAppEvent(SDL_EVENT_WILL_ENTER_BACKGROUND);
        SDL_SendAppEvent(SDL_EVENT_DID_ENTER_BACKGROUND);
        break;
    case WGUIEventType_AppResumed:
        SDL_SendAppEvent(SDL_EVENT_WILL_ENTER_FOREGROUND);
        SDL_SendAppEvent(SDL_EVENT_DID_ENTER_FOREGROUND);
        break;
    case WGUIEventType_AppQuitRequested:
        SDL_SendQuit();
        break;
    case WGUIEventType_AppTerminating:
        SDL_SendAppEvent(SDL_EVENT_TERMINATING);
        break;
    case WGUIEventType_AppThemeChanged: {
        const WGUITheme theme = ((const WGUIThemeEvent *)event)->theme;
        SDL_SetSystemTheme(theme == WGUITheme_Dark ? SDL_SYSTEM_THEME_DARK : theme == WGUITheme_Light ? SDL_SYSTEM_THEME_LIGHT : SDL_SYSTEM_THEME_UNKNOWN);
        break;
    }
    case WGUIEventType_ClipboardChanged:
        WASMER_ClipboardChanged((const WGUIClipboardChangedEvent *)event);
        break;
    case WGUIEventType_DisplaysChanged:
        WASMER_RefreshDisplays(_this);
        break;
    default:
        break;
    }

    if (!window) {
        return;
    }
    switch (event->type) {
    case WGUIEventType_WindowResized:
        WASMER_WindowResized(window, (const WGUIWindowResizedEvent *)event);
        break;
    case WGUIEventType_WindowMoved: {
        const WGUIWindowMovedEvent *moved = (const WGUIWindowMovedEvent *)event;
        const double scale = WASMER_Scale(window);
        SDL_SendWindowEvent(window, SDL_EVENT_WINDOW_MOVED, (int)SDL_lround(moved->x / scale), (int)SDL_lround(moved->y / scale));
        break;
    }
    case WGUIEventType_WindowFlagsChanged:
        WASMER_WindowFlags(window, (const WGUIWindowFlagsChangedEvent *)event);
        break;
    case WGUIEventType_WindowFocusGained:
        SDL_SetKeyboardFocus(window);
        break;
    case WGUIEventType_WindowFocusLost:
        if (SDL_GetKeyboardFocus() == window) {
            SDL_SetKeyboardFocus(NULL);
        }
        break;
    case WGUIEventType_WindowCloseRequested:
    case WGUIEventType_WindowLost:
        // A window the host took away is one the program can only close.
        SDL_SendWindowEvent(window, SDL_EVENT_WINDOW_CLOSE_REQUESTED, 0, 0);
        break;
    case WGUIEventType_WindowRedrawRequested:
        if (window->internal->frame_wanted) {
            // The display is ready for the frame after the one just shown:
            // what WASMER_WaitForFrame is waiting to hear.
            window->internal->frame_wanted = false;
        } else {
            SDL_SendWindowEvent(window, SDL_EVENT_WINDOW_EXPOSED, 0, 0);
        }
        break;
    case WGUIEventType_WindowDisplayChanged: {
        const SDL_DisplayID display = WASMER_FindDisplay(_this, ((const WGUIWindowDisplayChangedEvent *)event)->display);
        if (display) {
            SDL_SendWindowEvent(window, SDL_EVENT_WINDOW_DISPLAY_CHANGED, (int)display, 0);
        }
        break;
    }
    case WGUIEventType_WindowCursorGrabChanged:
        window->internal->locked = ((const WGUIWindowCursorGrabChangedEvent *)event)->grab == WGUICursorGrab_Locked;
        break;
    case WGUIEventType_KeyDown:
    case WGUIEventType_KeyUp:
        WASMER_Key(_this, timestamp, (const WGUIKeyEvent *)event, event->type == WGUIEventType_KeyDown);
        break;
    case WGUIEventType_ModifiersChanged: {
        // SDL follows Shift and its like from their keys. The locks may have
        // been set before the window had the keyboard.
        const WGUIModifiers modifiers = ((const WGUIModifiersChangedEvent *)event)->modifiers;
        SDL_ToggleModState(SDL_KMOD_CAPS, (modifiers & WGUIModifiers_CapsLock) != 0);
        SDL_ToggleModState(SDL_KMOD_NUM, (modifiers & WGUIModifiers_NumLock) != 0);
        SDL_ToggleModState(SDL_KMOD_SCROLL, (modifiers & WGUIModifiers_ScrollLock) != 0);
        break;
    }
    case WGUIEventType_TextInputState:
        WASMER_TextInputState(window, (const WGUITextInputStateEvent *)event);
        break;
    case WGUIEventType_TextCommit: {
        // While text is entered every insertion is a commit, the ones keys
        // type among them: SDL gets them all from here, and none from keys.
        const WGUITextCommitEvent *commit = (const WGUITextCommitEvent *)event;
        char *text = WASMER_String(event, commit->text);
        if (window->internal->composing && !(commit->textFlags & WGUITextFlags_FromKey)) {
            // What was being composed has become this text: programs take
            // an empty composition for the end of one.
            window->internal->composing = false;
            SDL_SendEditingText("", 0, 0);
        }
        if (text && text[0]) {
            SDL_SendKeyboardText(text);
        }
        SDL_free(text);
        break;
    }
    case WGUIEventType_TextComposition: {
        const WGUITextCompositionEvent *composition = (const WGUITextCompositionEvent *)event;
        char *text = WASMER_String(event, composition->text);
        if (text) {
            int start = 0, length = 0;
            if (composition->selectionStart != WGUI_NO_TEXT_CURSOR) {
                start = WASMER_Characters(text, composition->selectionStart);
                length = WASMER_Characters(text, composition->selectionEnd) - start;
            }
            window->internal->composing = text[0] != '\0';
            SDL_SendEditingText(text, start, SDL_max(length, 0));
        }
        SDL_free(text);
        break;
    }
    case WGUIEventType_PointerEnter:
    case WGUIEventType_PointerLeave:
    case WGUIEventType_PointerMotion:
    case WGUIEventType_PointerDown:
    case WGUIEventType_PointerUp:
    case WGUIEventType_PointerCancel:
        WASMER_Pointer(_this, window, timestamp, (const WGUIPointerEvent *)event);
        break;
    case WGUIEventType_Wheel:
        WASMER_Wheel(window, timestamp, (const WGUIWheelEvent *)event);
        break;
    default:
        // Records of kinds this driver does not know are stepped over.
        break;
    }
}

void WASMER_PumpEvents(SDL_VideoDevice *_this)
{
    SDL_VideoData *video = _this->internal;

    for (;;) {
        size_t written = 0;
        const WGUIEventHeader *event;

        // Asking is also where signals reach the program: SDL's own handler
        // for SIGINT runs in this call, and SDL makes its quit event of it
        // once the pump is over.
        if (wguiInstancePollEvents(video->instance, wasmer_records, sizeof(wasmer_records), &written) != WGUIStatus_Success || written == 0) {
            return;
        }
        for (event = wguiEventFirst(wasmer_records, written); event; event = wguiEventNext(wasmer_records, written, event)) {
            WASMER_Dispatch(_this, event);
        }
    }
}

int WASMER_WaitEventTimeout(SDL_VideoDevice *_this, Sint64 timeoutNS)
{
    SDL_VideoData *video = _this->internal;
    const uint64_t timeout = timeoutNS < 0 ? WGUI_WAIT_FOREVER : (uint64_t)timeoutNS;
    WGUIWaitReasons reasons = 0;

    // The thread sleeps in the host until there is something to read: a
    // program waiting for its user costs nothing.
    if (wguiInstanceWaitEvents(video->instance, timeout, &reasons) != WGUIStatus_Success) {
        return -1;
    }
    if (reasons == 0) {
        return 0;
    }
    // SDL looks in its own queue next: what the host has goes there now.
    // A wait the host cut short (WGUIWaitReasons_Interrupted) ends the same
    // way: a signal may have come with it, which SDL finds when it pumps.
    WASMER_PumpEvents(_this);
    return 1;
}

void WASMER_SendWakeupEvent(SDL_VideoDevice *_this, SDL_Window *window)
{
    SDL_VideoData *video = _this->internal;
    wguiInstanceWake(video->instance);
}

bool WASMER_WaitForRequest(SDL_VideoDevice *_this, Uint64 tag, Sint64 timeoutNS)
{
    SDL_VideoData *video = _this->internal;
    const Uint64 deadline = SDL_GetTicksNS() + (Uint64)timeoutNS;

    for (;;) {
        Uint64 now;
        WGUIWaitReasons reasons = 0;

        WASMER_PumpEvents(_this);
        if (video->last_done >= tag) {
            return true;
        }
        now = SDL_GetTicksNS();
        if (now >= deadline) {
            return false;
        }
        if (wguiInstanceWaitEvents(video->instance, deadline - now, &reasons) != WGUIStatus_Success) {
            return false;
        }
    }
}

void WASMER_WaitForFrame(SDL_VideoDevice *_this, SDL_Window *window, Sint64 timeoutNS)
{
    SDL_VideoData *video = _this->internal;
    const Uint64 deadline = SDL_GetTicksNS() + (Uint64)timeoutNS;

    for (;;) {
        Uint64 now;
        WGUIWaitReasons reasons = 0;

        // Everything else that happened meanwhile goes to SDL's queue, for
        // the program to read when it next asks.
        WASMER_PumpEvents(_this);
        if (!window->internal || !window->internal->frame_wanted) {
            return;
        }
        now = SDL_GetTicksNS();
        if (now >= deadline) {
            return;
        }
        if (wguiInstanceWaitEvents(video->instance, deadline - now, &reasons) != WGUIStatus_Success) {
            return;
        }
    }
}

// SDL_SyncWindow: what was asked of the window so far has happened, or been
// refused, when this returns.
bool WASMER_SyncWindow(SDL_VideoDevice *_this, SDL_Window *window)
{
    SDL_VideoData *video = _this->internal;
    if (video->next_tag <= 1) {
        return true;
    }
    return WASMER_WaitForRequest(_this, video->next_tag - 1, SDL_NS_PER_SECOND);
}

#endif // SDL_VIDEO_DRIVER_WASMER

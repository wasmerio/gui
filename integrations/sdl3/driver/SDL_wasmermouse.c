/*
  The mouse of the "wasmer" video driver: cursors, and the cursor lock that
  SDL calls relative mode.
*/
#include "SDL_internal.h"

#ifdef SDL_VIDEO_DRIVER_WASMER

#include "SDL_wasmervideo.h"
#include "../../events/SDL_mouse_c.h"

struct SDL_CursorData
{
    // One of the system's cursors, or an image of the program's own.
    bool custom;
    WGUICursorShape shape;
    WGUICursor cursor;
};

static WGUICursorShape WASMER_Shape(SDL_SystemCursor id)
{
    switch (id) {
    case SDL_SYSTEM_CURSOR_TEXT: return WGUICursorShape_Text;
    case SDL_SYSTEM_CURSOR_WAIT: return WGUICursorShape_Wait;
    case SDL_SYSTEM_CURSOR_CROSSHAIR: return WGUICursorShape_Crosshair;
    case SDL_SYSTEM_CURSOR_PROGRESS: return WGUICursorShape_Progress;
    case SDL_SYSTEM_CURSOR_NWSE_RESIZE: return WGUICursorShape_NwseResize;
    case SDL_SYSTEM_CURSOR_NESW_RESIZE: return WGUICursorShape_NeswResize;
    case SDL_SYSTEM_CURSOR_EW_RESIZE: return WGUICursorShape_EwResize;
    case SDL_SYSTEM_CURSOR_NS_RESIZE: return WGUICursorShape_NsResize;
    case SDL_SYSTEM_CURSOR_MOVE: return WGUICursorShape_Move;
    case SDL_SYSTEM_CURSOR_NOT_ALLOWED: return WGUICursorShape_NotAllowed;
    case SDL_SYSTEM_CURSOR_POINTER: return WGUICursorShape_Pointer;
    case SDL_SYSTEM_CURSOR_NW_RESIZE: return WGUICursorShape_NwResize;
    case SDL_SYSTEM_CURSOR_N_RESIZE: return WGUICursorShape_NResize;
    case SDL_SYSTEM_CURSOR_NE_RESIZE: return WGUICursorShape_NeResize;
    case SDL_SYSTEM_CURSOR_E_RESIZE: return WGUICursorShape_EResize;
    case SDL_SYSTEM_CURSOR_SE_RESIZE: return WGUICursorShape_SeResize;
    case SDL_SYSTEM_CURSOR_S_RESIZE: return WGUICursorShape_SResize;
    case SDL_SYSTEM_CURSOR_SW_RESIZE: return WGUICursorShape_SwResize;
    case SDL_SYSTEM_CURSOR_W_RESIZE: return WGUICursorShape_WResize;
    default: return WGUICursorShape_Default;
    }
}

static SDL_Cursor *WASMER_NewCursor(bool custom, WGUICursorShape shape, WGUICursor handle)
{
    SDL_Cursor *cursor = (SDL_Cursor *)SDL_calloc(1, sizeof(*cursor));
    SDL_CursorData *data = (SDL_CursorData *)SDL_calloc(1, sizeof(*data));
    if (!cursor || !data) {
        SDL_free(cursor);
        SDL_free(data);
        return NULL;
    }
    data->custom = custom;
    data->shape = shape;
    data->cursor = handle;
    cursor->internal = data;
    return cursor;
}

static SDL_Cursor *WASMER_CreateSystemCursor(SDL_SystemCursor id)
{
    return WASMER_NewCursor(false, WASMER_Shape(id), 0);
}

static SDL_Cursor *WASMER_CreateCursor(SDL_Surface *surface, int hot_x, int hot_y)
{
    SDL_VideoData *video = SDL_GetVideoDevice()->internal;
    WGUICursorDescriptor descriptor = WGUI_CURSOR_DESCRIPTOR_INIT;
    WGUICursor handle = 0;
    SDL_Surface *rgba;
    SDL_Cursor *cursor;
    WGUIStatus status;

    // Red, green, blue, alpha in memory, alpha not multiplied in, rows with
    // nothing between them: what the host takes.
    rgba = SDL_ConvertSurface(surface, SDL_PIXELFORMAT_RGBA32);
    if (!rgba) {
        return NULL;
    }
    descriptor.width = (uint32_t)rgba->w;
    descriptor.height = (uint32_t)rgba->h;
    descriptor.hotspotX = (uint32_t)SDL_clamp(hot_x, 0, rgba->w - 1);
    descriptor.hotspotY = (uint32_t)SDL_clamp(hot_y, 0, rgba->h - 1);
    if (rgba->pitch == rgba->w * 4) {
        descriptor.pixels = rgba->pixels;
        descriptor.pixelsSize = (size_t)rgba->w * (size_t)rgba->h * 4;
        status = wguiInstanceCreateCursor(video->instance, &descriptor, &handle);
    } else {
        // Rows with padding: packed first.
        Uint8 *packed = (Uint8 *)SDL_malloc((size_t)rgba->w * (size_t)rgba->h * 4);
        if (!packed) {
            SDL_DestroySurface(rgba);
            return NULL;
        }
        for (int y = 0; y < rgba->h; y++) {
            SDL_memcpy(packed + (size_t)y * (size_t)rgba->w * 4, (Uint8 *)rgba->pixels + (size_t)y * (size_t)rgba->pitch, (size_t)rgba->w * 4);
        }
        descriptor.pixels = packed;
        descriptor.pixelsSize = (size_t)rgba->w * (size_t)rgba->h * 4;
        status = wguiInstanceCreateCursor(video->instance, &descriptor, &handle);
        SDL_free(packed);
    }
    SDL_DestroySurface(rgba);
    if (status != WGUIStatus_Success) {
        SDL_SetError("The host cannot show this cursor (status %d)", (int)status);
        return NULL;
    }
    cursor = WASMER_NewCursor(true, WGUICursorShape_Default, handle);
    if (!cursor) {
        wguiCursorDestroy(handle);
    }
    return cursor;
}

static void WASMER_FreeCursor(SDL_Cursor *cursor)
{
    if (cursor) {
        if (cursor->internal && cursor->internal->custom) {
            wguiCursorDestroy(cursor->internal->cursor);
        }
        SDL_free(cursor->internal);
        SDL_free(cursor);
    }
}

static void WASMER_ShowOn(SDL_Window *window, SDL_Cursor *cursor)
{
    SDL_WindowData *data = window->internal;
    if (!data) {
        return;
    }
    if (!cursor || !cursor->internal) {
        wguiWindowSetCursorVisible(data->window, false);
        return;
    }
    if (cursor->internal->custom) {
        wguiWindowSetCustomCursor(data->window, cursor->internal->cursor);
    } else {
        wguiWindowSetCursorShape(data->window, cursor->internal->shape);
    }
    wguiWindowSetCursorVisible(data->window, true);
}

void WASMER_ApplyCursor(SDL_Window *window)
{
    SDL_Mouse *mouse = SDL_GetMouse();
    WASMER_ShowOn(window, mouse->cursor_visible ? mouse->cur_cursor : NULL);
}

// The cursor is the window's it is over, and SDL has one for all of them.
static bool WASMER_ShowCursor(SDL_Cursor *cursor)
{
    SDL_VideoDevice *_this = SDL_GetVideoDevice();
    for (SDL_Window *window = _this->windows; window; window = window->next) {
        WASMER_ShowOn(window, cursor);
    }
    return true;
}

static bool WASMER_WarpMouse(SDL_Window *window, float x, float y)
{
    SDL_WindowData *data = window->internal;
    const double scale = WASMER_Scale(window);
    if (!WASMER_Has(WGUIFeatureName_CursorWarp)) {
        return SDL_Unsupported();
    }
    if (wguiWindowSetCursorPosition(data->window, x * scale, y * scale) != WGUIStatus_Success) {
        return SDL_SetError("The cursor could not be moved");
    }
    return true;
}

bool WASMER_UpdateGrab(SDL_VideoDevice *_this, SDL_Window *window, bool grabbed)
{
    SDL_VideoData *video = _this->internal;
    SDL_WindowData *data = window->internal;
    // A locked cursor is more than a confined one, and SDL asks for a grab
    // for either.
    const WGUICursorGrab wanted = !grabbed ? WGUICursorGrab_None : video->relative ? WGUICursorGrab_Locked : WGUICursorGrab_Confined;
    WGUIStatus status;

    if (!data) {
        return !grabbed;
    }
    // The host never gives one kind of grab for another, and is not asked
    // for one it does not have.
    if (wanted == WGUICursorGrab_Confined && !WASMER_Has(WGUIFeatureName_CursorConfine)) {
        return SDL_Unsupported();
    }
    if (wanted == WGUICursorGrab_Locked && !WASMER_Has(WGUIFeatureName_CursorLock)) {
        return SDL_Unsupported();
    }
    if (wanted == data->grab_wanted) {
        return true;
    }
    // The answer comes as events: a completed request, and the grab's state.
    status = wguiWindowSetCursorGrab(data->window, wanted, WASMER_NextTag(_this));
    if (status != WGUIStatus_Success) {
        return SDL_SetError("The host would not change the cursor's grab (status %d)", (int)status);
    }
    data->grab_wanted = wanted;
    return true;
}

void WASMER_Relock(SDL_VideoDevice *_this, SDL_Window *window)
{
    SDL_VideoData *video = _this->internal;
    SDL_WindowData *data = window->internal;

    if (video->relative && data->grab_wanted == WGUICursorGrab_Locked && !data->locked && window == SDL_GetKeyboardFocus()) {
        wguiWindowSetCursorGrab(data->window, WGUICursorGrab_Locked, WASMER_NextTag(_this));
    }
}

static bool WASMER_SetRelativeMouseMode(bool enabled)
{
    SDL_VideoDevice *_this = SDL_GetVideoDevice();
    SDL_VideoData *video = _this->internal;

    if (!WASMER_Has(WGUIFeatureName_CursorLock)) {
        return SDL_Unsupported();
    }
    video->relative = enabled;
    if (enabled) {
        // SDL turns this on for the window that has the keyboard.
        SDL_Window *window = SDL_GetKeyboardFocus();
        if (!window || !WASMER_UpdateGrab(_this, window, true)) {
            video->relative = false;
            return window ? false : SDL_SetError("No window has the keyboard");
        }
        return true;
    }
    // Every window gets back what it had asked for besides.
    for (SDL_Window *window = _this->windows; window; window = window->next) {
        const bool focused = (window->flags & SDL_WINDOW_INPUT_FOCUS) != 0;
        const bool grabbed = focused && (window->flags & SDL_WINDOW_MOUSE_GRABBED) != 0 && WASMER_Has(WGUIFeatureName_CursorConfine);
        WASMER_UpdateGrab(_this, window, grabbed);
    }
    return true;
}

void WASMER_InitMouse(SDL_VideoDevice *_this)
{
    SDL_Mouse *mouse = SDL_GetMouse();

    mouse->CreateSystemCursor = WASMER_CreateSystemCursor;
    if (WASMER_Has(WGUIFeatureName_CustomCursors)) {
        mouse->CreateCursor = WASMER_CreateCursor;
    }
    mouse->ShowCursor = WASMER_ShowCursor;
    mouse->FreeCursor = WASMER_FreeCursor;
    mouse->WarpMouse = WASMER_WarpMouse;
    mouse->SetRelativeMouseMode = WASMER_SetRelativeMouseMode;

    SDL_SetDefaultCursor(WASMER_CreateSystemCursor(SDL_SYSTEM_CURSOR_DEFAULT));
}

void WASMER_QuitMouse(SDL_VideoDevice *_this)
{
}

#endif // SDL_VIDEO_DRIVER_WASMER

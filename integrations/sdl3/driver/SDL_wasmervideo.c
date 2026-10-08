/*
  The "wasmer" video driver: SDL's windows, input, text entry, clipboard and
  software framebuffer on <wasmer/gui.h>, for WASIX programs running on the
  Wasmer runtime, natively or in a browser.

  The host never calls into the program: what happens arrives as event
  records, which SDL_wasmerevents.c reads when SDL pumps or waits. What a
  host cannot do it says, and SDL is told the same: nothing is imitated.

  There is no GPU path here. SDL has no WebGPU renderer, so its renderer is
  the software one, drawing into the framebuffer of SDL_wasmerframebuffer.c.
  A program that wants the GPU uses webgpu.h itself, on the surface named by
  the window's SDL_PROP_WINDOW_WASMER_SURFACE_SELECTOR_STRING property.
*/
#include "SDL_internal.h"

#ifdef SDL_VIDEO_DRIVER_WASMER

#include "SDL_wasmervideo.h"
#include "../../events/SDL_keyboard_c.h"
#include "../../events/SDL_mouse_c.h"

#define WASMERVID_DRIVER_NAME "wasmer"

// The target a window adopts (a canvas of the page, in a browser), for
// SDL_CreateWindowWithProperties. Without it the one this hint names, which
// the environment variable of the same name sets for a program that knows
// nothing of targets; without either, the host's default one.
#define SDL_PROP_WINDOW_CREATE_WASMER_TARGET_STRING "SDL.window.create.wasmer.target"
#define SDL_HINT_VIDEO_WASMER_TARGET                "SDL_VIDEO_WASMER_TARGET"
// What a WebGPU surface for the window is created with: the selector of
// WGPUWasixSurfaceSourceCanvas.
#define SDL_PROP_WINDOW_WASMER_SURFACE_SELECTOR_STRING "SDL.window.wasmer.surface_selector"
// The host's own handle of the window (a WGUIWindow).
#define SDL_PROP_WINDOW_WASMER_WINDOW_NUMBER "SDL.window.wasmer.window"

// How long a clipboard that has to be asked for first is waited for.
#define WASMER_CLIPBOARD_WAIT_NS (500 * SDL_NS_PER_MS)

Uint64 WASMER_NextTag(SDL_VideoDevice *_this)
{
    SDL_VideoData *video = _this->internal;
    return video->next_tag++;
}

bool WASMER_Has(WGUIFeatureName feature)
{
    return wguiHasFeature(feature);
}

double WASMER_Scale(SDL_Window *window)
{
    SDL_WindowData *data = window->internal;
    return (data && data->scale > 0.0) ? data->scale : 1.0;
}

// What the host counts in pixels, from what SDL counts in window coordinates.
static uint32_t WASMER_Pixels(SDL_Window *window, int coordinate)
{
    return (uint32_t)SDL_max(SDL_lround(coordinate * WASMER_Scale(window)), 0);
}

// The same for a place, which can be left of or above where counting starts.
static int32_t WASMER_Position(SDL_Window *window, int coordinate)
{
    return (int32_t)SDL_lround(coordinate * WASMER_Scale(window));
}

// ---- displays ---------------------------------------------------------------

static void WASMER_FillDisplay(WGUIDisplay handle, SDL_VideoDisplay *display, SDL_DisplayMode *mode)
{
    WGUIDisplayInfo info = WGUI_DISPLAY_INFO_INIT;
    double scale = 1.0;

    SDL_zerop(mode);
    mode->format = SDL_PIXELFORMAT_XRGB8888;
    mode->w = 1280;
    mode->h = 720;
    mode->pixel_density = 1.0f;
    if (handle && wguiDisplayGetInfo(handle, &info) == WGUIStatus_Success && info.width && info.height) {
        scale = info.scaleFactor > 0.0 ? info.scaleFactor : 1.0;
        // A mode is in window coordinates, with the pixels behind each.
        mode->w = (int)SDL_lround(info.width / scale);
        mode->h = (int)SDL_lround(info.height / scale);
        mode->pixel_density = (float)scale;
        mode->refresh_rate = info.refreshRate / 1000.0f;
        mode->refresh_rate_numerator = (int)info.refreshRate;
        mode->refresh_rate_denominator = info.refreshRate ? 1000 : 0;
    }
    display->desktop_mode = *mode;
    // Window coordinates are logical units already: what is drawn at them
    // needs no further scale, as on macOS. The pixels are in the density.
    display->content_scale = 1.0f;
}

static bool WASMER_AddDisplays(SDL_VideoDevice *_this)
{
    SDL_VideoData *video = _this->internal;
    WGUIDisplay handles[16];
    size_t count = 0;
    bool added = false;

    if (wguiInstanceGetDisplays(video->instance, SDL_arraysize(handles), handles, &count) != WGUIStatus_Success) {
        count = 0;
    }
    count = SDL_min(count, SDL_arraysize(handles));
    for (size_t i = 0; i < count; i++) {
        SDL_VideoDisplay display;
        SDL_DisplayMode mode;
        SDL_DisplayData *data = (SDL_DisplayData *)SDL_calloc(1, sizeof(*data));
        if (!data) {
            return false;
        }
        data->display = handles[i];
        SDL_zero(display);
        WASMER_FillDisplay(handles[i], &display, &mode);
        display.internal = data;
        if (SDL_AddVideoDisplay(&display, false) == 0) {
            SDL_free(data);
            return false;
        }
        added = true;
    }
    if (!added) {
        // SDL needs a display, and some hosts have none to tell of.
        SDL_VideoDisplay display;
        SDL_DisplayMode mode;
        SDL_zero(display);
        WASMER_FillDisplay(0, &display, &mode);
        display.internal = (SDL_DisplayData *)SDL_calloc(1, sizeof(SDL_DisplayData));
        if (!display.internal || SDL_AddVideoDisplay(&display, false) == 0) {
            SDL_free(display.internal);
            return false;
        }
    }
    return true;
}

// The host's displays changed: SDL's list is made to say the same.
void WASMER_RefreshDisplays(SDL_VideoDevice *_this)
{
    SDL_VideoData *video = _this->internal;
    WGUIDisplay handles[16];
    size_t count = 0;

    if (wguiInstanceGetDisplays(video->instance, SDL_arraysize(handles), handles, &count) != WGUIStatus_Success || count == 0) {
        // A host that tells of none keeps the one SDL was given.
        return;
    }
    count = SDL_min(count, SDL_arraysize(handles));

    // The ones that are gone, last first: removing one moves the rest.
    for (int i = _this->num_displays - 1; i >= 0; i--) {
        SDL_VideoDisplay *display = _this->displays[i];
        bool found = false;
        for (size_t j = 0; j < count; j++) {
            found = found || (display->internal && display->internal->display == handles[j]);
        }
        if (!found && _this->num_displays > 1) {
            SDL_DelVideoDisplay(display->id, true);
        }
    }
    for (size_t j = 0; j < count; j++) {
        SDL_VideoDisplay *known = NULL;
        SDL_VideoDisplay filled;
        SDL_DisplayMode mode;

        for (int i = 0; i < _this->num_displays; i++) {
            if (_this->displays[i]->internal && _this->displays[i]->internal->display == handles[j]) {
                known = _this->displays[i];
            }
        }
        SDL_zero(filled);
        WASMER_FillDisplay(handles[j], &filled, &mode);
        if (known) {
            SDL_SetDesktopDisplayMode(known, &mode);
        } else {
            SDL_DisplayData *data = (SDL_DisplayData *)SDL_calloc(1, sizeof(*data));
            if (data) {
                data->display = handles[j];
                filled.internal = data;
                if (SDL_AddVideoDisplay(&filled, true) == 0) {
                    SDL_free(data);
                }
            }
        }
    }
}

SDL_DisplayID WASMER_FindDisplay(SDL_VideoDevice *_this, WGUIDisplay display)
{
    for (int i = 0; display && i < _this->num_displays; i++) {
        if (_this->displays[i]->internal && _this->displays[i]->internal->display == display) {
            return _this->displays[i]->id;
        }
    }
    return 0;
}

static SDL_DisplayID WASMER_GetDisplayForWindow(SDL_VideoDevice *_this, SDL_Window *window)
{
    WGUIWindowState state = WGUI_WINDOW_STATE_INIT;

    // The host knows which display a window is on, also where windows have
    // no position to work it out from.
    if (window->internal && wguiWindowGetState(window->internal->window, &state) == WGUIStatus_Success) {
        return WASMER_FindDisplay(_this, state.display);
    }
    return 0;
}

static bool WASMER_DisplayRect(SDL_VideoDisplay *display, SDL_Rect *rect, bool usable)
{
    WGUIDisplayInfo info = WGUI_DISPLAY_INFO_INIT;
    double scale;

    if (!display->internal->display || wguiDisplayGetInfo(display->internal->display, &info) != WGUIStatus_Success) {
        rect->x = rect->y = 0;
        rect->w = display->desktop_mode.w;
        rect->h = display->desktop_mode.h;
        return true;
    }
    scale = info.scaleFactor > 0.0 ? info.scaleFactor : 1.0;
    rect->x = (int)SDL_lround((usable ? info.workX : info.x) / scale);
    rect->y = (int)SDL_lround((usable ? info.workY : info.y) / scale);
    rect->w = (int)SDL_lround((usable ? info.workWidth : info.width) / scale);
    rect->h = (int)SDL_lround((usable ? info.workHeight : info.height) / scale);
    return true;
}

static bool WASMER_GetDisplayBounds(SDL_VideoDevice *_this, SDL_VideoDisplay *display, SDL_Rect *rect)
{
    return WASMER_DisplayRect(display, rect, false);
}

static bool WASMER_GetDisplayUsableBounds(SDL_VideoDevice *_this, SDL_VideoDisplay *display, SDL_Rect *rect)
{
    return WASMER_DisplayRect(display, rect, true);
}

// ---- windows ----------------------------------------------------------------

// What notches and system bars cover of a window, for SDL_GetWindowSafeArea.
void WASMER_UpdateSafeArea(SDL_Window *window)
{
    WGUIWindowState state = WGUI_WINDOW_STATE_INIT;
    double density;
    int left, right, top, bottom;

    if (wguiWindowGetState(window->internal->window, &state) != WGUIStatus_Success) {
        return;
    }
    density = state.scaleFactor > 0.0 ? state.scaleFactor : 1.0;
    left = (int)SDL_ceil(state.safeLeft / density);
    right = (int)SDL_ceil(state.safeRight / density);
    top = (int)SDL_ceil(state.safeTop / density);
    bottom = (int)SDL_ceil(state.safeBottom / density);
    if (left != window->safe_inset_left || right != window->safe_inset_right || top != window->safe_inset_top || bottom != window->safe_inset_bottom) {
        SDL_SetWindowSafeAreaInsets(window, left, right, top, bottom);
    }
}

static WGUIWindowFlags WASMER_WantedFlags(SDL_Window *window)
{
    WGUIWindowFlags flags = 0;
    if (!(window->flags & SDL_WINDOW_HIDDEN)) {
        flags |= WGUIWindowFlags_Visible;
    }
    if (window->flags & SDL_WINDOW_RESIZABLE) {
        flags |= WGUIWindowFlags_Resizable;
    }
    if (!(window->flags & SDL_WINDOW_BORDERLESS)) {
        flags |= WGUIWindowFlags_Decorated;
    }
    if (window->flags & SDL_WINDOW_MAXIMIZED) {
        flags |= WGUIWindowFlags_Maximized;
    }
    if (window->flags & SDL_WINDOW_MINIMIZED) {
        flags |= WGUIWindowFlags_Minimized;
    }
    if (window->flags & SDL_WINDOW_ALWAYS_ON_TOP) {
        flags |= WGUIWindowFlags_AlwaysOnTop;
    }
    if (window->flags & SDL_WINDOW_TRANSPARENT) {
        flags |= WGUIWindowFlags_Transparent;
    }
    return flags;
}

static bool WASMER_CreateWindow(SDL_VideoDevice *_this, SDL_Window *window, SDL_PropertiesID create_props)
{
    SDL_VideoData *video = _this->internal;
    WGUIWindowDescriptor descriptor = WGUI_WINDOW_DESCRIPTOR_INIT;
    WGUIWindowState state = WGUI_WINDOW_STATE_INIT;
    SDL_WindowData *data;
    const char *target = SDL_GetStringProperty(create_props, SDL_PROP_WINDOW_CREATE_WASMER_TARGET_STRING, SDL_GetHint(SDL_HINT_VIDEO_WASMER_TARGET));
    char selector[128];
    size_t selector_length = 0;
    WGUIStatus status;

    data = (SDL_WindowData *)SDL_calloc(1, sizeof(*data));
    if (!data) {
        return false;
    }
    data->scale = 1.0;

    if (window->title) {
        descriptor.title.data = window->title;
    }
    if (target && target[0]) {
        descriptor.target.data = target;
    } else {
        target = NULL;
    }
    // SDL's window coordinates are the host's logical units.
    descriptor.options = WGUIWindowOptions_LogicalUnits;
    descriptor.width = (uint32_t)SDL_max(window->w, 1);
    descriptor.height = (uint32_t)SDL_max(window->h, 1);
    descriptor.minWidth = (uint32_t)SDL_max(window->min_w, 0);
    descriptor.minHeight = (uint32_t)SDL_max(window->min_h, 0);
    descriptor.maxWidth = (uint32_t)SDL_max(window->max_w, 0);
    descriptor.maxHeight = (uint32_t)SDL_max(window->max_h, 0);
    descriptor.flags = WASMER_WantedFlags(window);
    if (!window->undefined_x && !window->undefined_y && WASMER_Has(WGUIFeatureName_WindowPosition)) {
        descriptor.options |= WGUIWindowOptions_Positioned;
        descriptor.x = window->x;
        descriptor.y = window->y;
    }

    status = wguiInstanceCreateWindow(video->instance, &descriptor, &data->window);
    if (status != WGUIStatus_Success) {
        SDL_free(data);
        switch (status) {
        case WGUIStatus_NotFound:
            return SDL_SetError("The host has no window target named '%s'", target ? target : "");
        case WGUIStatus_Busy:
            return SDL_SetError("The host's window target is already a window");
        case WGUIStatus_LimitExceeded:
            return SDL_SetError("The host allows no more windows");
        default:
            return SDL_SetError("The host could not open a window (status %d)", (int)status);
        }
    }
    window->internal = data;

    // The host decides how big the window is, and says so before the call
    // above returns: SDL is told here, the first event records in a moment.
    if (wguiWindowGetState(data->window, &state) == WGUIStatus_Success) {
        data->width = state.width;
        data->height = state.height;
        data->scale = state.scaleFactor > 0.0 ? state.scaleFactor : 1.0;
        data->flags = state.flags;
        window->w = window->floating.w = (int)SDL_lround(state.width / data->scale);
        window->h = window->floating.h = (int)SDL_lround(state.height / data->scale);
        if (state.x != WGUI_POSITION_UNDEFINED && state.y != WGUI_POSITION_UNDEFINED) {
            window->x = window->floating.x = (int)SDL_lround(state.x / data->scale);
            window->y = window->floating.y = (int)SDL_lround(state.y / data->scale);
        }
    }

    WASMER_UpdateSafeArea(window);

    if (wguiWindowGetSurfaceSelector(data->window, selector, sizeof(selector) - 1, &selector_length) == WGUIStatus_Success && selector_length < sizeof(selector)) {
        selector[selector_length] = '\0';
        SDL_SetStringProperty(SDL_GetWindowProperties(window), SDL_PROP_WINDOW_WASMER_SURFACE_SELECTOR_STRING, selector);
    }
    SDL_SetNumberProperty(SDL_GetWindowProperties(window), SDL_PROP_WINDOW_WASMER_WINDOW_NUMBER, (Sint64)data->window);

    WASMER_ApplyCursor(window);
    return true;
}

static void WASMER_DestroyWindow(SDL_VideoDevice *_this, SDL_Window *window)
{
    SDL_WindowData *data = window->internal;
    if (data) {
        SDL_DestroySurface(data->framebuffer);
        wguiWindowDestroy(data->window);
        SDL_free(data);
        window->internal = NULL;
    }
}

static void WASMER_SetWindowTitle(SDL_VideoDevice *_this, SDL_Window *window)
{
    wguiWindowSetTitle(window->internal->window, window->title ? window->title : "", WGUI_STRLEN);
}

static bool WASMER_SetWindowPosition(SDL_VideoDevice *_this, SDL_Window *window)
{
    if (!WASMER_Has(WGUIFeatureName_WindowPosition)) {
        return SDL_Unsupported();
    }
    // SDL hears where the window ended up from the event that follows.
    if (wguiWindowSetPosition(window->internal->window, WASMER_Position(window, window->pending.x), WASMER_Position(window, window->pending.y)) != WGUIStatus_Success) {
        return SDL_SetError("The window could not be moved");
    }
    return true;
}

static void WASMER_SetWindowSize(SDL_VideoDevice *_this, SDL_Window *window)
{
    // SDL takes the new size from the resize event that follows, which says
    // what the host made of the wish.
    wguiWindowSetSize(window->internal->window, WASMER_Pixels(window, window->pending.w), WASMER_Pixels(window, window->pending.h));
}

static void WASMER_SetWindowSizeLimits(SDL_VideoDevice *_this, SDL_Window *window)
{
    wguiWindowSetSizeLimits(window->internal->window, WASMER_Pixels(window, window->min_w), WASMER_Pixels(window, window->min_h), WASMER_Pixels(window, window->max_w), WASMER_Pixels(window, window->max_h));
}

static void WASMER_GetWindowSizeInPixels(SDL_VideoDevice *_this, SDL_Window *window, int *w, int *h)
{
    SDL_WindowData *data = window->internal;
    if (data && (window->flags & SDL_WINDOW_HIGH_PIXEL_DENSITY) && data->width && data->height) {
        *w = (int)data->width;
        *h = (int)data->height;
    } else {
        *w = window->w;
        *h = window->h;
    }
}

static bool WASMER_GetWindowBordersSize(SDL_VideoDevice *_this, SDL_Window *window, int *top, int *left, int *bottom, int *right)
{
    WGUIWindowState state = WGUI_WINDOW_STATE_INIT;
    double scale;

    if (wguiWindowGetState(window->internal->window, &state) != WGUIStatus_Success) {
        return SDL_SetError("The window's state is not known");
    }
    scale = state.scaleFactor > 0.0 ? state.scaleFactor : 1.0;
    *top = (int)SDL_lround(state.frameTop / scale);
    *left = (int)SDL_lround(state.frameLeft / scale);
    *bottom = (int)SDL_lround(state.frameBottom / scale);
    *right = (int)SDL_lround(state.frameRight / scale);
    return true;
}

// Asks the host for flags; what it will not do, it leaves, and its events
// say what the window is.
static void WASMER_SetFlags(SDL_Window *window, WGUIWindowFlags mask, WGUIWindowFlags values)
{
    WGUIWindowFlags refused = 0;
    wguiWindowSetFlags(window->internal->window, mask, values, &refused);
}

static void WASMER_ShowWindow(SDL_VideoDevice *_this, SDL_Window *window)
{
    // Whether a window that appears takes the keyboard is the host's to say,
    // as it is the window system's elsewhere. SDL_RaiseWindow asks for it.
    WASMER_SetFlags(window, WGUIWindowFlags_Visible, WGUIWindowFlags_Visible);
}

static void WASMER_HideWindow(SDL_VideoDevice *_this, SDL_Window *window)
{
    WASMER_SetFlags(window, WGUIWindowFlags_Visible, 0);
}

static void WASMER_RaiseWindow(SDL_VideoDevice *_this, SDL_Window *window)
{
    wguiWindowFocus(window->internal->window);
}

static void WASMER_MaximizeWindow(SDL_VideoDevice *_this, SDL_Window *window)
{
    WASMER_SetFlags(window, WGUIWindowFlags_Maximized | WGUIWindowFlags_Minimized, WGUIWindowFlags_Maximized);
}

static void WASMER_MinimizeWindow(SDL_VideoDevice *_this, SDL_Window *window)
{
    WASMER_SetFlags(window, WGUIWindowFlags_Minimized, WGUIWindowFlags_Minimized);
}

static void WASMER_RestoreWindow(SDL_VideoDevice *_this, SDL_Window *window)
{
    WASMER_SetFlags(window, WGUIWindowFlags_Maximized | WGUIWindowFlags_Minimized, 0);
}

static void WASMER_SetWindowBordered(SDL_VideoDevice *_this, SDL_Window *window, bool bordered)
{
    WASMER_SetFlags(window, WGUIWindowFlags_Decorated, bordered ? WGUIWindowFlags_Decorated : 0);
}

static void WASMER_SetWindowResizable(SDL_VideoDevice *_this, SDL_Window *window, bool resizable)
{
    WASMER_SetFlags(window, WGUIWindowFlags_Resizable, resizable ? WGUIWindowFlags_Resizable : 0);
}

static void WASMER_SetWindowAlwaysOnTop(SDL_VideoDevice *_this, SDL_Window *window, bool on_top)
{
    WASMER_SetFlags(window, WGUIWindowFlags_AlwaysOnTop, on_top ? WGUIWindowFlags_AlwaysOnTop : 0);
}

static SDL_FullscreenResult WASMER_SetWindowFullscreen(SDL_VideoDevice *_this, SDL_Window *window, SDL_VideoDisplay *display, SDL_FullscreenOp fullscreen)
{
    SDL_WindowData *data = window->internal;
    WGUIFullscreenOptions options = WGUI_FULLSCREEN_OPTIONS_INIT;
    const bool enter = fullscreen != SDL_FULLSCREEN_OP_LEAVE;
    const bool is_fullscreen = (data->flags & WGUIWindowFlags_Fullscreen) != 0;
    WGUIStatus status;

    if (enter == is_fullscreen && data->fullscreen_tag == 0) {
        return SDL_FULLSCREEN_SUCCEEDED;
    }
    if (!WASMER_Has(WGUIFeatureName_FullscreenBorderless)) {
        SDL_Unsupported();
        return SDL_FULLSCREEN_FAILED;
    }
    if (display && display->internal) {
        options.display = display->internal->display;
    }
    data->fullscreen_wanted = enter;
    data->fullscreen_tag = WASMER_NextTag(_this);
    status = wguiWindowSetFullscreen(data->window, enter ? &options : NULL, data->fullscreen_tag);
    if (status != WGUIStatus_Success) {
        data->fullscreen_tag = 0;
        SDL_SetError("The host would not change fullscreen (status %d)", (int)status);
        return SDL_FULLSCREEN_FAILED;
    }
    // The window's flags changing is what tells SDL that it happened. A host
    // may wait for something the user does first (a browser does).
    return SDL_FULLSCREEN_PENDING;
}

static bool WASMER_SetWindowMouseGrab(SDL_VideoDevice *_this, SDL_Window *window, bool grabbed)
{
    return WASMER_UpdateGrab(_this, window, grabbed);
}

static bool WASMER_FlashWindow(SDL_VideoDevice *_this, SDL_Window *window, SDL_FlashOperation operation)
{
    WGUIAttentionLevel level = WGUIAttentionLevel_None;
    if (!WASMER_Has(WGUIFeatureName_AttentionRequest)) {
        return SDL_Unsupported();
    }
    if (operation == SDL_FLASH_BRIEFLY) {
        level = WGUIAttentionLevel_Informational;
    } else if (operation == SDL_FLASH_UNTIL_FOCUSED) {
        level = WGUIAttentionLevel_Critical;
    }
    return wguiWindowRequestAttention(window->internal->window, level) == WGUIStatus_Success;
}

// ---- text entry ---------------------------------------------------------------

static bool WASMER_DescribeTextInput(SDL_VideoDevice *_this, SDL_Window *window, SDL_PropertiesID props)
{
    SDL_WindowData *data = window->internal;
    WGUITextInputDescriptor descriptor = WGUI_TEXT_INPUT_DESCRIPTOR_INIT;
    const SDL_Rect *area = &window->text_input_rect;
    WGUIStatus status;

    switch (SDL_GetTextInputType(props)) {
    case SDL_TEXTINPUT_TYPE_TEXT_NAME: descriptor.purpose = WGUITextInputPurpose_Name; break;
    case SDL_TEXTINPUT_TYPE_TEXT_EMAIL: descriptor.purpose = WGUITextInputPurpose_Email; break;
    case SDL_TEXTINPUT_TYPE_TEXT_USERNAME: descriptor.purpose = WGUITextInputPurpose_Username; break;
    case SDL_TEXTINPUT_TYPE_TEXT_PASSWORD_HIDDEN:
        descriptor.hints |= WGUITextInputHints_HiddenText;
        SDL_FALLTHROUGH;
    case SDL_TEXTINPUT_TYPE_TEXT_PASSWORD_VISIBLE: descriptor.purpose = WGUITextInputPurpose_Password; break;
    case SDL_TEXTINPUT_TYPE_NUMBER: descriptor.purpose = WGUITextInputPurpose_Number; break;
    case SDL_TEXTINPUT_TYPE_NUMBER_PASSWORD_HIDDEN:
        descriptor.hints |= WGUITextInputHints_HiddenText;
        SDL_FALLTHROUGH;
    case SDL_TEXTINPUT_TYPE_NUMBER_PASSWORD_VISIBLE: descriptor.purpose = WGUITextInputPurpose_Pin; break;
    default: descriptor.purpose = WGUITextInputPurpose_Normal; break;
    }
    switch (SDL_GetTextInputCapitalization(props)) {
    case SDL_CAPITALIZE_SENTENCES: descriptor.hints |= WGUITextInputHints_CapitalizeSentences; break;
    case SDL_CAPITALIZE_WORDS: descriptor.hints |= WGUITextInputHints_CapitalizeWords; break;
    case SDL_CAPITALIZE_LETTERS: descriptor.hints |= WGUITextInputHints_CapitalizeCharacters; break;
    default: break;
    }
    if (SDL_GetTextInputAutocorrect(props)) {
        descriptor.hints |= WGUITextInputHints_Completion | WGUITextInputHints_Spellcheck;
    }
    if (SDL_GetTextInputMultiline(props)) {
        descriptor.hints |= WGUITextInputHints_Multiline;
    }
    if (descriptor.purpose == WGUITextInputPurpose_Password || descriptor.purpose == WGUITextInputPurpose_Pin) {
        descriptor.hints |= WGUITextInputHints_SensitiveData;
    }
    descriptor.screenKeyboard = data->screen_keyboard;
    // Where the caret is, so that the input method opens next to it: the
    // host counts in the window's pixels.
    descriptor.cursorArea.x = WASMER_Position(window, area->x + window->text_input_cursor);
    descriptor.cursorArea.y = WASMER_Position(window, area->y);
    descriptor.cursorArea.width = WASMER_Pixels(window, 1);
    descriptor.cursorArea.height = WASMER_Pixels(window, SDL_max(area->h, 1));
    descriptor.inputArea.x = WASMER_Position(window, area->x);
    descriptor.inputArea.y = WASMER_Position(window, area->y);
    descriptor.inputArea.width = WASMER_Pixels(window, area->w);
    descriptor.inputArea.height = WASMER_Pixels(window, area->h);

    status = wguiWindowSetTextInput(data->window, &descriptor);
    if (status != WGUIStatus_Success) {
        return SDL_SetError("The host has no text entry for this window (status %d)", (int)status);
    }
    data->text_input = true;
    return true;
}

static bool WASMER_StartTextInput(SDL_VideoDevice *_this, SDL_Window *window, SDL_PropertiesID props)
{
    return WASMER_DescribeTextInput(_this, window, props);
}

static bool WASMER_StopTextInput(SDL_VideoDevice *_this, SDL_Window *window)
{
    SDL_WindowData *data = window->internal;
    data->text_input = false;
    data->composing = false;
    wguiWindowSetTextInput(data->window, NULL);
    return true;
}

static bool WASMER_UpdateTextInputArea(SDL_VideoDevice *_this, SDL_Window *window)
{
    if (!window->internal->text_input) {
        return true;
    }
    return WASMER_DescribeTextInput(_this, window, window->text_input_props);
}

static bool WASMER_ClearComposition(SDL_VideoDevice *_this, SDL_Window *window)
{
    wguiWindowResetTextInput(window->internal->window);
    return true;
}

static bool WASMER_HasScreenKeyboardSupport(SDL_VideoDevice *_this)
{
    return WASMER_Has(WGUIFeatureName_ScreenKeyboard);
}

static void WASMER_ShowScreenKeyboard(SDL_VideoDevice *_this, SDL_Window *window, SDL_PropertiesID props)
{
    SDL_WindowData *data = window->internal;
    // Takes effect with the text entry SDL starts next, or now if it is on.
    data->screen_keyboard = WGUIScreenKeyboardPolicy_Show;
    if (data->text_input) {
        WASMER_DescribeTextInput(_this, window, props);
    }
}

static void WASMER_HideScreenKeyboard(SDL_VideoDevice *_this, SDL_Window *window)
{
    SDL_WindowData *data = window->internal;
    if (data->text_input) {
        data->screen_keyboard = WGUIScreenKeyboardPolicy_Hide;
        WASMER_DescribeTextInput(_this, window, window->text_input_props);
    }
    // The next text entry is the host's to decide for again.
    data->screen_keyboard = WGUIScreenKeyboardPolicy_Auto;
}

// ---- the clipboard ------------------------------------------------------------

static bool WASMER_SetClipboardText(SDL_VideoDevice *_this, const char *text)
{
    SDL_VideoData *video = _this->internal;
    WGUIClipboardItem item = WGUI_CLIPBOARD_ITEM_INIT;
    const bool clear = !text || !text[0];
    WGUIStatus status;

    item.mimeType.data = "text/plain";
    item.data = text;
    item.size = clear ? 0 : SDL_strlen(text);
    // The program reads its own text back at once; whether the system took
    // it comes as an event.
    status = wguiInstanceWriteClipboard(video->instance, WGUIClipboardSelection_Clipboard, clear ? 0 : 1, clear ? NULL : &item, WASMER_NextTag(_this));
    if (status != WGUIStatus_Success) {
        return SDL_SetError("The clipboard cannot be written (status %d)", (int)status);
    }
    return true;
}

// The clipboard's text, or NULL when there is none to be had. `status` says why.
static char *WASMER_ReadClipboard(SDL_VideoData *video, WGUIStatus *status)
{
    size_t capacity = 256;
    char *text = NULL;

    for (int attempt = 0; attempt < 4; attempt++) {
        size_t size = 0;
        char *grown = (char *)SDL_realloc(text, capacity + 1);
        if (!grown) {
            SDL_free(text);
            *status = WGUIStatus_Error;
            return NULL;
        }
        text = grown;
        *status = wguiInstanceReadClipboard(video->instance, WGUIClipboardSelection_Clipboard, "text/plain", WGUI_STRLEN, text, capacity, &size);
        if (*status == WGUIStatus_Success) {
            text[SDL_min(size, capacity)] = '\0';
            return text;
        }
        if (*status != WGUIStatus_BufferTooSmall) {
            break;
        }
        // The host said how much there is.
        capacity = size;
    }
    SDL_free(text);
    return NULL;
}

static char *WASMER_GetClipboardText(SDL_VideoDevice *_this)
{
    SDL_VideoData *video = _this->internal;
    WGUIStatus status = WGUIStatus_Error;
    char *text = WASMER_ReadClipboard(video, &status);

    if (!text && status == WGUIStatus_Pending) {
        // A host that has to ask for the clipboard first (a browser). SDL
        // wants an answer now: the request is made and waited for, briefly.
        const Uint64 tag = WASMER_NextTag(_this);
        video->clipboard_tag = tag;
        video->clipboard_status = WGUIStatus_Pending;
        if (wguiInstanceRequestClipboard(video->instance, WGUIClipboardSelection_Clipboard, "text/plain", WGUI_STRLEN, tag) == WGUIStatus_Success) {
            WASMER_WaitForRequest(_this, tag, WASMER_CLIPBOARD_WAIT_NS);
            if (video->clipboard_status == WGUIStatus_Success) {
                text = WASMER_ReadClipboard(video, &status);
            }
        }
        video->clipboard_tag = 0;
    }
    return text ? text : SDL_strdup("");
}

static bool WASMER_HasClipboardText(SDL_VideoDevice *_this)
{
    SDL_VideoData *video = _this->internal;
    WGUIClipboardInfo info = WGUI_CLIPBOARD_INFO_INIT;
    if (wguiInstanceGetClipboardInfo(video->instance, WGUIClipboardSelection_Clipboard, &info) != WGUIStatus_Success) {
        return false;
    }
    // A host that has to ask first cannot say: there may be, and reading
    // finds out.
    return (info.flags & (WGUIClipboardFlags_HasText | WGUIClipboardFlags_NeedsRequest)) != 0;
}

// ---- the device ---------------------------------------------------------------

static bool WASMER_VideoInit(SDL_VideoDevice *_this)
{
    SDL_VideoData *video = _this->internal;
    WGUIInstanceDescriptor descriptor = WGUI_INSTANCE_DESCRIPTOR_INIT;
    uint64_t host_time = 0;
    WGUIStatus status;

    status = wguiCreateInstance(&descriptor, &video->instance);
    if (status != WGUIStatus_Success) {
        return SDL_SetError("The host has no window system for this program (status %d)", (int)status);
    }
    video->info = (WGUIInstanceInfo)WGUI_INSTANCE_INFO_INIT;
    wguiInstanceGetInfo(video->instance, &video->info);
    video->next_tag = 1;
    // Records are stamped on the host's clock; SDL's events on SDL's.
    if (wguiInstanceGetTime(video->instance, &host_time) == WGUIStatus_Success) {
        video->clock_offset = (Sint64)SDL_GetTicksNS() - (Sint64)host_time;
    }

    if (!WASMER_AddDisplays(_this)) {
        return false;
    }
    SDL_AddKeyboard(SDL_DEFAULT_KEYBOARD_ID, NULL);
    SDL_AddMouse(SDL_DEFAULT_MOUSE_ID, NULL);
    WASMER_InitMouse(_this);

    // What the theme is when the program starts is not a change of it.
    switch (video->info.systemTheme) {
    case WGUITheme_Dark: _this->system_theme = SDL_SYSTEM_THEME_DARK; break;
    case WGUITheme_Light: _this->system_theme = SDL_SYSTEM_THEME_LIGHT; break;
    default: break;
    }
    return true;
}

static void WASMER_VideoQuit(SDL_VideoDevice *_this)
{
    SDL_VideoData *video = _this->internal;
    WASMER_QuitMouse(_this);
    if (video->instance) {
        wguiInstanceDestroy(video->instance);
        video->instance = 0;
    }
}

static void WASMER_DeleteDevice(SDL_VideoDevice *device)
{
    SDL_free(device->internal);
    SDL_free(device);
}

static SDL_VideoDevice *WASMER_CreateDevice(void)
{
    SDL_VideoDevice *device;
    SDL_VideoData *video;

    // A host without this API answers 0 to everything: there is no window
    // system, and SDL goes on to its next driver.
    if (wguiGetVersion() == 0 || !wguiHasFeature(WGUIFeatureName_Windows)) {
        return NULL;
    }

    device = (SDL_VideoDevice *)SDL_calloc(1, sizeof(*device));
    video = (SDL_VideoData *)SDL_calloc(1, sizeof(*video));
    if (!device || !video) {
        SDL_free(device);
        SDL_free(video);
        return NULL;
    }
    device->internal = video;

    device->VideoInit = WASMER_VideoInit;
    device->VideoQuit = WASMER_VideoQuit;
    device->free = WASMER_DeleteDevice;

    device->GetDisplayBounds = WASMER_GetDisplayBounds;
    device->GetDisplayUsableBounds = WASMER_GetDisplayUsableBounds;
    device->GetDisplayForWindow = WASMER_GetDisplayForWindow;

    device->CreateSDLWindow = WASMER_CreateWindow;
    device->DestroyWindow = WASMER_DestroyWindow;
    device->SetWindowTitle = WASMER_SetWindowTitle;
    device->SetWindowPosition = WASMER_SetWindowPosition;
    device->SetWindowSize = WASMER_SetWindowSize;
    device->SetWindowMinimumSize = WASMER_SetWindowSizeLimits;
    device->SetWindowMaximumSize = WASMER_SetWindowSizeLimits;
    device->GetWindowSizeInPixels = WASMER_GetWindowSizeInPixels;
    device->GetWindowBordersSize = WASMER_GetWindowBordersSize;
    device->ShowWindow = WASMER_ShowWindow;
    device->HideWindow = WASMER_HideWindow;
    device->RaiseWindow = WASMER_RaiseWindow;
    device->MaximizeWindow = WASMER_MaximizeWindow;
    device->MinimizeWindow = WASMER_MinimizeWindow;
    device->RestoreWindow = WASMER_RestoreWindow;
    device->SetWindowBordered = WASMER_SetWindowBordered;
    device->SetWindowResizable = WASMER_SetWindowResizable;
    device->SetWindowAlwaysOnTop = WASMER_SetWindowAlwaysOnTop;
    device->SetWindowFullscreen = WASMER_SetWindowFullscreen;
    device->SetWindowMouseGrab = WASMER_SetWindowMouseGrab;
    device->FlashWindow = WASMER_FlashWindow;
    device->SyncWindow = WASMER_SyncWindow;

    device->PumpEvents = WASMER_PumpEvents;
    device->WaitEventTimeout = WASMER_WaitEventTimeout;
    device->SendWakeupEvent = WASMER_SendWakeupEvent;

    if (wguiHasFeature(WGUIFeatureName_PixelPresent)) {
        device->CreateWindowFramebuffer = WASMER_CreateWindowFramebuffer;
        device->UpdateWindowFramebuffer = WASMER_UpdateWindowFramebuffer;
        device->DestroyWindowFramebuffer = WASMER_DestroyWindowFramebuffer;
        device->SetWindowFramebufferVSync = WASMER_SetWindowFramebufferVSync;
        device->GetWindowFramebufferVSync = WASMER_GetWindowFramebufferVSync;
    }

    if (wguiHasFeature(WGUIFeatureName_TextInput)) {
        device->StartTextInput = WASMER_StartTextInput;
        device->StopTextInput = WASMER_StopTextInput;
        device->UpdateTextInputArea = WASMER_UpdateTextInputArea;
        device->ClearComposition = WASMER_ClearComposition;
        device->HasScreenKeyboardSupport = WASMER_HasScreenKeyboardSupport;
        device->ShowScreenKeyboard = WASMER_ShowScreenKeyboard;
        device->HideScreenKeyboard = WASMER_HideScreenKeyboard;
    }

    if (wguiHasFeature(WGUIFeatureName_ClipboardText)) {
        device->SetClipboardText = WASMER_SetClipboardText;
        device->GetClipboardText = WASMER_GetClipboardText;
        device->HasClipboardText = WASMER_HasClipboardText;
    }

    // The host says what size a fullscreen window has, and which display a
    // window is on: SDL does not work either out for itself.
    device->device_caps = VIDEO_DEVICE_CAPS_SENDS_FULLSCREEN_DIMENSIONS | VIDEO_DEVICE_CAPS_SENDS_DISPLAY_CHANGES;
    return device;
}

VideoBootStrap WASMER_bootstrap = {
    WASMERVID_DRIVER_NAME, "SDL Wasmer GUI video driver",
    WASMER_CreateDevice,
    NULL, // the host has no message boxes to show
    false
};

#endif // SDL_VIDEO_DRIVER_WASMER

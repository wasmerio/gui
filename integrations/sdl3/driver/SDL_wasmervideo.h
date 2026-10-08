/*
  The "wasmer" video driver: SDL's windows, input, text entry, clipboard and
  software framebuffer on <wasmer/gui.h>, for WASIX programs running on the
  Wasmer runtime, natively or in a browser.

  Units. SDL counts a window in window coordinates and asks separately how
  many pixels it has. Here a window coordinate is one of the host's logical
  units, as on macOS and in a browser: a window of 800 by 600 is that big on
  every display, and has 1600 by 1200 pixels where the scale factor is 2.
  The host itself counts in physical pixels, so everything that crosses is
  multiplied or divided by the window's scale factor.

  A window made with SDL_WINDOW_HIGH_PIXEL_DENSITY draws at the display's
  pixels. One made without draws one pixel per window coordinate, and the
  host stretches its frames.
*/
#ifndef SDL_wasmervideo_h_
#define SDL_wasmervideo_h_

#include "SDL_internal.h"

#include "../SDL_sysvideo.h"

#include <wasmer/gui.h>

// The one touch device the host tells of.
#define WASMER_TOUCH_ID ((SDL_TouchID)1)

struct SDL_VideoData
{
    WGUIInstance instance;
    WGUIInstanceInfo info;
    // Added to a record's timestamp to give the time on SDL's clock.
    Sint64 clock_offset;
    // Tags for requests that complete by event, and the highest one seen done.
    Uint64 next_tag;
    Uint64 last_done;
    // The clipboard request that is waited for, and how it ended.
    Uint64 clipboard_tag;
    WGUIStatus clipboard_status;
    bool touch_added;
    // SDL wants the mouse to say how far it moved, not where it is.
    bool relative;
};

struct SDL_WindowData
{
    WGUIWindow window;
    // What the host last said of the window: its size in physical pixels,
    // and how many of them make one window coordinate.
    Uint32 width, height;
    double scale;
    WGUIWindowFlags flags;
    // The fullscreen request SDL is waiting to hear about, and what it asked.
    Uint64 fullscreen_tag;
    bool fullscreen_wanted;
    // The grab asked of the host, and whether the cursor is locked now: a
    // lock is lent, and the host says when it is given and taken back.
    WGUICursorGrab grab_wanted;
    bool locked;
    // The software framebuffer, and whether showing it waits for the display.
    SDL_Surface *framebuffer;
    int vsync;
    bool frame_wanted;
    // Text entry, as last described to the host, and whether an input
    // method is in the middle of composing text.
    bool text_input;
    bool composing;
    WGUIScreenKeyboardPolicy screen_keyboard;
    bool screen_keyboard_shown;
};

struct SDL_DisplayData
{
    WGUIDisplay display;
};

// SDL_wasmervideo.c
// A request tag nothing else is using.
extern Uint64 WASMER_NextTag(SDL_VideoDevice *_this);
// Whether the host has a feature; what it lacks is never asked of it.
extern bool WASMER_Has(WGUIFeatureName feature);
// The physical pixels of one window coordinate.
extern double WASMER_Scale(SDL_Window *window);
// SDL's displays, made to be what the host says they are now.
extern void WASMER_RefreshDisplays(SDL_VideoDevice *_this);
// The display of SDL's that the host knows by this handle, or 0.
extern SDL_DisplayID WASMER_FindDisplay(SDL_VideoDevice *_this, WGUIDisplay display);
// Tells SDL what of a window notches and system bars cover.
extern void WASMER_UpdateSafeArea(SDL_Window *window);

// SDL_wasmerevents.c
extern void WASMER_PumpEvents(SDL_VideoDevice *_this);
extern int WASMER_WaitEventTimeout(SDL_VideoDevice *_this, Sint64 timeoutNS);
extern void WASMER_SendWakeupEvent(SDL_VideoDevice *_this, SDL_Window *window);
extern bool WASMER_SyncWindow(SDL_VideoDevice *_this, SDL_Window *window);
// Pumps until the request `tag` has completed, or `timeoutNS` has passed.
extern bool WASMER_WaitForRequest(SDL_VideoDevice *_this, Uint64 tag, Sint64 timeoutNS);
// Pumps until the display is ready for the window's next frame, or
// `timeoutNS` has passed.
extern void WASMER_WaitForFrame(SDL_VideoDevice *_this, SDL_Window *window, Sint64 timeoutNS);

// SDL_wasmermouse.c
extern void WASMER_InitMouse(SDL_VideoDevice *_this);
extern void WASMER_QuitMouse(SDL_VideoDevice *_this);
// Shows on a window the cursor SDL has current.
extern void WASMER_ApplyCursor(SDL_Window *window);
// Asks the host for the grab SDL wants of a window: the cursor locked in
// relative mode, kept inside otherwise, or let go.
extern bool WASMER_UpdateGrab(SDL_VideoDevice *_this, SDL_Window *window, bool grabbed);
// Asks again for a lock the user or the system ended. Hosts that want
// something done by the user first (a browser) take a click for it.
extern void WASMER_Relock(SDL_VideoDevice *_this, SDL_Window *window);

// SDL_wasmerframebuffer.c
extern bool WASMER_CreateWindowFramebuffer(SDL_VideoDevice *_this, SDL_Window *window, SDL_PixelFormat *format, void **pixels, int *pitch);
extern bool WASMER_UpdateWindowFramebuffer(SDL_VideoDevice *_this, SDL_Window *window, const SDL_Rect *rects, int numrects);
extern void WASMER_DestroyWindowFramebuffer(SDL_VideoDevice *_this, SDL_Window *window);
extern bool WASMER_SetWindowFramebufferVSync(SDL_VideoDevice *_this, SDL_Window *window, int vsync);
extern bool WASMER_GetWindowFramebufferVSync(SDL_VideoDevice *_this, SDL_Window *window, int *vsync);

#endif // SDL_wasmervideo_h_

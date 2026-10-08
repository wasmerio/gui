/*
  The window framebuffer of the "wasmer" video driver: what SDL's software
  renderer and SDL_GetWindowSurface draw into, shown with
  wguiWindowPresentPixels.
*/
#include "SDL_internal.h"

#ifdef SDL_VIDEO_DRIVER_WASMER

#include "SDL_wasmervideo.h"

// What changed is passed on for this many rectangles; beyond that the whole
// frame is said to have.
#define WASMER_MAX_DAMAGE 16

// How long showing a frame waits for the display at most. A window nobody
// can see is not asked to draw, and its program is slowed to this.
#define WASMER_FRAME_WAIT_NS (100 * SDL_NS_PER_MS)

bool WASMER_CreateWindowFramebuffer(SDL_VideoDevice *_this, SDL_Window *window, SDL_PixelFormat *format, void **pixels, int *pitch)
{
    SDL_WindowData *data = window->internal;
    int w, h;

    // Called again after every change of the window's size in pixels.
    SDL_DestroySurface(data->framebuffer);
    data->framebuffer = NULL;

    SDL_GetWindowSizeInPixels(window, &w, &h);
    if (w <= 0 || h <= 0) {
        return SDL_SetError("The window has no size to draw at");
    }
    // Blue, green, red and a byte that means nothing: WGUIPixelFormat_Bgra8,
    // opaque.
    data->framebuffer = SDL_CreateSurface(w, h, SDL_PIXELFORMAT_XRGB8888);
    if (!data->framebuffer) {
        return false;
    }
    *format = data->framebuffer->format;
    *pixels = data->framebuffer->pixels;
    *pitch = data->framebuffer->pitch;
    return true;
}

bool WASMER_UpdateWindowFramebuffer(SDL_VideoDevice *_this, SDL_Window *window, const SDL_Rect *rects, int numrects)
{
    SDL_WindowData *data = window->internal;
    SDL_Surface *surface = data->framebuffer;
    WGUIPixelFrame frame = WGUI_PIXEL_FRAME_INIT;
    WGUIRect damage[WASMER_MAX_DAMAGE];
    size_t count = 0;
    WGUIStatus status;

    if (!surface) {
        return SDL_SetError("The window has no framebuffer");
    }
    frame.width = (uint32_t)surface->w;
    frame.height = (uint32_t)surface->h;
    frame.stride = (uint32_t)surface->pitch;
    frame.format = WGUIPixelFormat_Bgra8;
    frame.alpha = WGUIPixelAlpha_Opaque;
    frame.pixels = surface->pixels;
    frame.pixelsSize = (size_t)surface->pitch * (size_t)surface->h;

    if (rects && numrects > 0 && numrects <= WASMER_MAX_DAMAGE) {
        for (int i = 0; i < numrects; i++) {
            if (rects[i].w <= 0 || rects[i].h <= 0) {
                continue;
            }
            damage[count].x = rects[i].x;
            damage[count].y = rects[i].y;
            damage[count].width = (uint32_t)rects[i].w;
            damage[count].height = (uint32_t)rects[i].h;
            count++;
        }
        if (count == 0) {
            // Nothing of the window changed.
            return true;
        }
    }

    // The host copies the frame before this returns, and shows the newest
    // one it has when the display is next drawn.
    status = wguiWindowPresentPixels(data->window, &frame, count, count ? damage : NULL);
    if (status != WGUIStatus_Success) {
        return SDL_SetError("The frame could not be shown (status %d)", (int)status);
    }

    if (data->vsync) {
        // One frame for each one the display shows: the host says when it
        // is ready for the next.
        data->frame_wanted = true;
        if (wguiWindowRequestRedraw(data->window) == WGUIStatus_Success) {
            WASMER_WaitForFrame(_this, window, WASMER_FRAME_WAIT_NS);
        }
        // The window may have been closed by what was pumped meanwhile.
        if (window->internal) {
            window->internal->frame_wanted = false;
        }
    }
    return true;
}

void WASMER_DestroyWindowFramebuffer(SDL_VideoDevice *_this, SDL_Window *window)
{
    SDL_WindowData *data = window->internal;
    if (data) {
        SDL_DestroySurface(data->framebuffer);
        data->framebuffer = NULL;
    }
}

bool WASMER_SetWindowFramebufferVSync(SDL_VideoDevice *_this, SDL_Window *window, int vsync)
{
    // Every frame of the display, or as fast as the program draws. There is
    // no tearing to allow, and no skipping of frames to ask for.
    if (vsync != 0 && vsync != 1) {
        return SDL_Unsupported();
    }
    window->internal->vsync = vsync;
    return true;
}

bool WASMER_GetWindowFramebufferVSync(SDL_VideoDevice *_this, SDL_Window *window, int *vsync)
{
    *vsync = window->internal->vsync;
    return true;
}

#endif // SDL_VIDEO_DRIVER_WASMER

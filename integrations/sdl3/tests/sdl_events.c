// SDL on the "wasmer" video driver: opens a window, draws it with the
// software renderer whenever SDL says it is exposed, and prints every SDL
// event it gets, one line each, until it is asked to quit. It sleeps in
// SDL_WaitEvent in between, as a program that only reacts to its user does.
//
// The function keys make it use the rest of SDL's video API, so that a
// script of input can walk it through all of it:
//
//   F1  fullscreen on and off       F2  relative mouse mode  F3  and off
//   F4  text input on and off       F5  copy a text          F6  read the clipboard
//   F7  ask for 400x300             F8  maximize and restore F9  quit
//   F10 the text cursor, then one of the program's own
//
// and Control or the GUI key with V pastes, as programs do.
//
// Arguments: --hidpi (ask for all of a dense display's pixels), --surface
// (draw on the window surface, without a renderer), --target <name>,
// --quit-after <milliseconds> (a timer on another thread ends the wait),
// --animate <milliseconds> (draw frame after frame for that long, as games
// do, and say how many there were), --interrupt-after <milliseconds> (the
// program is sent SIGINT, as by Ctrl-C in its terminal: SDL makes a quit
// event of that).

#include <SDL3/SDL.h>
#include <SDL3/SDL_main.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

static SDL_Window *window;
static SDL_Renderer *renderer;
static bool use_surface;
static bool running = true;
static bool text_input;
static bool maximized;
static int cursor_step;
static int frames;
static float mouse_x, mouse_y;

static void print_text(const char *text)
{
    putchar('"');
    for (const unsigned char *c = (const unsigned char *)text; *c; c++) {
        if (*c < 0x20 || *c == 0x7F) {
            printf("\\x%02X", *c);
        } else {
            putchar(*c);
        }
    }
    putchar('"');
}

static void print_mods(SDL_Keymod mods)
{
    static const struct
    {
        SDL_Keymod bit;
        const char *name;
    } names[] = {
        { SDL_KMOD_LSHIFT, "LShift" }, { SDL_KMOD_RSHIFT, "RShift" }, { SDL_KMOD_LCTRL, "LCtrl" },
        { SDL_KMOD_RCTRL, "RCtrl" },   { SDL_KMOD_LALT, "LAlt" },     { SDL_KMOD_RALT, "RAlt" },
        { SDL_KMOD_LGUI, "LGui" },     { SDL_KMOD_RGUI, "RGui" },     { SDL_KMOD_NUM, "Num" },
        { SDL_KMOD_CAPS, "Caps" },     { SDL_KMOD_MODE, "Mode" },     { SDL_KMOD_SCROLL, "Scroll" },
    };
    bool any = false;
    for (size_t i = 0; i < SDL_arraysize(names); i++) {
        if (mods & names[i].bit) {
            printf("%s%s", any ? "|" : "", names[i].name);
            any = true;
        }
    }
    if (!any) {
        putchar('0');
    }
}

// What the window shows: a colour that says how many frames there were, and
// a square where the mouse is.
static void draw(void)
{
    int w = 0, h = 0;
    const Uint8 shade = (Uint8)(40 + (frames * 16) % 200);

    frames++;
    SDL_GetWindowSizeInPixels(window, &w, &h);
    if (use_surface) {
        SDL_Surface *surface = SDL_GetWindowSurface(window);
        if (!surface) {
            printf("no window surface: %s\n", SDL_GetError());
            return;
        }
        SDL_FillSurfaceRect(surface, NULL, SDL_MapSurfaceRGB(surface, shade, 60, 90));
        if (!SDL_UpdateWindowSurface(window)) {
            printf("the surface was not shown: %s\n", SDL_GetError());
        }
        printf("drew %dx%d on the surface\n", surface->w, surface->h);
    } else {
        const float density = SDL_GetWindowPixelDensity(window);
        const SDL_FRect square = { mouse_x * density - 8.0f, mouse_y * density - 8.0f, 16.0f, 16.0f };
        SDL_SetRenderDrawColor(renderer, shade, 60, 90, 255);
        SDL_RenderClear(renderer);
        SDL_SetRenderDrawColor(renderer, 255, 255, 255, 255);
        SDL_RenderFillRect(renderer, &square);
        if (!SDL_RenderPresent(renderer)) {
            printf("the frame was not shown: %s\n", SDL_GetError());
        }
        printf("drew %dx%d\n", w, h);
    }
}

// Runs on SDL's timer thread: the event it pushes has to end the main
// thread's wait in the host.
static Uint32 SDLCALL quit_timer(void *userdata, SDL_TimerID id, Uint32 interval)
{
    SDL_Event event;
    (void)userdata;
    (void)id;
    (void)interval;
    SDL_zero(event);
    event.type = SDL_EVENT_QUIT;
    SDL_PushEvent(&event);
    return 0;
}

// What Ctrl-C in the program's terminal does. The signal reaches the main
// thread where it asks SDL for events, whether it waits for them or not.
static Uint32 SDLCALL interrupt_timer(void *userdata, SDL_TimerID id, Uint32 interval)
{
    (void)userdata;
    (void)id;
    (void)interval;
    printf("interrupting\n");
    kill(getpid(), SIGINT);
    return 0;
}

static SDL_Cursor *own_cursor(void)
{
    SDL_Surface *image = SDL_CreateSurface(16, 16, SDL_PIXELFORMAT_RGBA32);
    SDL_Cursor *cursor;
    if (!image) {
        return NULL;
    }
    SDL_FillSurfaceRect(image, NULL, SDL_MapSurfaceRGBA(image, 255, 0, 0, 255));
    cursor = SDL_CreateColorCursor(image, 3, 5);
    SDL_DestroySurface(image);
    return cursor;
}

static void paste(const char *how)
{
    const bool has = SDL_HasClipboardText();
    char *text = SDL_GetClipboardText();
    printf("%s %s ", how, has ? "text" : "nothing");
    print_text(text ? text : "");
    putchar('\n');
    SDL_free(text);
}

static void on_key(const SDL_KeyboardEvent *key)
{
    if (key->repeat) {
        return;
    }
    switch (key->scancode) {
    case SDL_SCANCODE_F1: {
        const bool fullscreen = (SDL_GetWindowFlags(window) & SDL_WINDOW_FULLSCREEN) != 0;
        printf("fullscreen %s: %s\n", fullscreen ? "off" : "on", SDL_SetWindowFullscreen(window, !fullscreen) ? "asked" : SDL_GetError());
        break;
    }
    case SDL_SCANCODE_F2:
        printf("relative on: %s\n", SDL_SetWindowRelativeMouseMode(window, true) ? "ok" : SDL_GetError());
        break;
    case SDL_SCANCODE_F3:
        printf("relative off: %s\n", SDL_SetWindowRelativeMouseMode(window, false) ? "ok" : SDL_GetError());
        break;
    case SDL_SCANCODE_F4: {
        const SDL_Rect area = { 10, 20, 200, 16 };
        text_input = !text_input;
        if (text_input) {
            SDL_SetTextInputArea(window, &area, 4);
            printf("text input on: %s\n", SDL_StartTextInput(window) ? "ok" : SDL_GetError());
        } else {
            printf("text input off: %s\n", SDL_StopTextInput(window) ? "ok" : SDL_GetError());
        }
        break;
    }
    case SDL_SCANCODE_F5:
        printf("copy: %s\n", SDL_SetClipboardText("copied by SDL") ? "ok" : SDL_GetError());
        break;
    case SDL_SCANCODE_F6:
        // Reading the clipboard without the user having asked for a paste.
        paste("read");
        break;
    case SDL_SCANCODE_V:
        if (key->mod & (SDL_KMOD_CTRL | SDL_KMOD_GUI)) {
            paste("pasted");
        }
        break;
    case SDL_SCANCODE_F7:
        printf("size 400x300: %s\n", SDL_SetWindowSize(window, 400, 300) ? "asked" : SDL_GetError());
        break;
    case SDL_SCANCODE_F8:
        maximized = !maximized;
        printf("%s: %s\n", maximized ? "maximize" : "restore", (maximized ? SDL_MaximizeWindow(window) : SDL_RestoreWindow(window)) ? "asked" : SDL_GetError());
        break;
    case SDL_SCANCODE_F9:
        running = false;
        break;
    case SDL_SCANCODE_F10: {
        SDL_Cursor *cursor = (cursor_step++ % 2 == 0) ? SDL_CreateSystemCursor(SDL_SYSTEM_CURSOR_TEXT) : own_cursor();
        printf("cursor: %s\n", (cursor && SDL_SetCursor(cursor)) ? "ok" : SDL_GetError());
        break;
    }
    default:
        break;
    }
}

static void on_event(const SDL_Event *event)
{
    switch (event->type) {
    case SDL_EVENT_QUIT:
        printf("QUIT\n");
        running = false;
        break;
    case SDL_EVENT_WINDOW_SHOWN: printf("WINDOW_SHOWN\n"); break;
    case SDL_EVENT_WINDOW_HIDDEN: printf("WINDOW_HIDDEN\n"); break;
    case SDL_EVENT_WINDOW_EXPOSED:
        printf("WINDOW_EXPOSED\n");
        draw();
        break;
    case SDL_EVENT_WINDOW_MOVED: printf("WINDOW_MOVED %d,%d\n", event->window.data1, event->window.data2); break;
    case SDL_EVENT_WINDOW_RESIZED: printf("WINDOW_RESIZED %dx%d\n", event->window.data1, event->window.data2); break;
    case SDL_EVENT_WINDOW_PIXEL_SIZE_CHANGED: printf("WINDOW_PIXEL_SIZE_CHANGED %dx%d\n", event->window.data1, event->window.data2); break;
    case SDL_EVENT_WINDOW_MINIMIZED: printf("WINDOW_MINIMIZED\n"); break;
    case SDL_EVENT_WINDOW_MAXIMIZED: printf("WINDOW_MAXIMIZED\n"); break;
    case SDL_EVENT_WINDOW_RESTORED: printf("WINDOW_RESTORED\n"); break;
    case SDL_EVENT_WINDOW_MOUSE_ENTER: printf("WINDOW_MOUSE_ENTER\n"); break;
    case SDL_EVENT_WINDOW_MOUSE_LEAVE: printf("WINDOW_MOUSE_LEAVE\n"); break;
    case SDL_EVENT_WINDOW_FOCUS_GAINED: printf("WINDOW_FOCUS_GAINED\n"); break;
    case SDL_EVENT_WINDOW_FOCUS_LOST: printf("WINDOW_FOCUS_LOST\n"); break;
    case SDL_EVENT_WINDOW_CLOSE_REQUESTED:
        printf("WINDOW_CLOSE_REQUESTED\n");
        break;
    case SDL_EVENT_WINDOW_DISPLAY_CHANGED: printf("WINDOW_DISPLAY_CHANGED\n"); break;
    case SDL_EVENT_WINDOW_DISPLAY_SCALE_CHANGED: printf("WINDOW_DISPLAY_SCALE_CHANGED %g\n", SDL_GetWindowDisplayScale(window)); break;
    case SDL_EVENT_WINDOW_SAFE_AREA_CHANGED: printf("WINDOW_SAFE_AREA_CHANGED\n"); break;
    case SDL_EVENT_WINDOW_OCCLUDED: printf("WINDOW_OCCLUDED\n"); break;
    case SDL_EVENT_WINDOW_ENTER_FULLSCREEN: printf("WINDOW_ENTER_FULLSCREEN\n"); break;
    case SDL_EVENT_WINDOW_LEAVE_FULLSCREEN: printf("WINDOW_LEAVE_FULLSCREEN\n"); break;
    case SDL_EVENT_KEY_DOWN:
    case SDL_EVENT_KEY_UP:
        printf("%s %s key=%s mods=", event->type == SDL_EVENT_KEY_DOWN ? "KEY_DOWN" : "KEY_UP", SDL_GetScancodeName(event->key.scancode), SDL_GetKeyName(event->key.key));
        print_mods(event->key.mod);
        printf("%s\n", event->key.repeat ? " repeat" : "");
        if (event->type == SDL_EVENT_KEY_DOWN) {
            on_key(&event->key);
        }
        break;
    case SDL_EVENT_TEXT_INPUT:
        printf("TEXT_INPUT ");
        print_text(event->text.text);
        putchar('\n');
        break;
    case SDL_EVENT_TEXT_EDITING:
        printf("TEXT_EDITING ");
        print_text(event->edit.text);
        printf(" start=%d length=%d\n", (int)event->edit.start, (int)event->edit.length);
        break;
    case SDL_EVENT_MOUSE_MOTION:
        mouse_x = event->motion.x;
        mouse_y = event->motion.y;
        printf("MOUSE_MOTION at=%g,%g rel=%g,%g buttons=0x%x\n", event->motion.x, event->motion.y, event->motion.xrel, event->motion.yrel, (unsigned)event->motion.state);
        break;
    case SDL_EVENT_MOUSE_BUTTON_DOWN:
    case SDL_EVENT_MOUSE_BUTTON_UP:
        printf("%s button=%d clicks=%d at=%g,%g\n", event->type == SDL_EVENT_MOUSE_BUTTON_DOWN ? "MOUSE_BUTTON_DOWN" : "MOUSE_BUTTON_UP", (int)event->button.button, (int)event->button.clicks, event->button.x, event->button.y);
        break;
    case SDL_EVENT_MOUSE_WHEEL:
        printf("MOUSE_WHEEL %g,%g at=%g,%g\n", event->wheel.x, event->wheel.y, event->wheel.mouse_x, event->wheel.mouse_y);
        break;
    case SDL_EVENT_FINGER_DOWN:
    case SDL_EVENT_FINGER_UP:
    case SDL_EVENT_FINGER_MOTION:
    case SDL_EVENT_FINGER_CANCELED:
        printf("%s finger=%d at=%.3f,%.3f\n",
               event->type == SDL_EVENT_FINGER_DOWN ? "FINGER_DOWN" : event->type == SDL_EVENT_FINGER_UP ? "FINGER_UP" : event->type == SDL_EVENT_FINGER_MOTION ? "FINGER_MOTION" : "FINGER_CANCELED",
               (int)event->tfinger.fingerID, event->tfinger.x, event->tfinger.y);
        break;
    case SDL_EVENT_CLIPBOARD_UPDATE:
        printf("CLIPBOARD_UPDATE owner=%d types=%d\n", (int)event->clipboard.owner, (int)event->clipboard.num_mime_types);
        break;
    case SDL_EVENT_SYSTEM_THEME_CHANGED:
        printf("SYSTEM_THEME_CHANGED %d\n", (int)SDL_GetSystemTheme());
        break;
    case SDL_EVENT_DISPLAY_CONTENT_SCALE_CHANGED: printf("DISPLAY_CONTENT_SCALE_CHANGED\n"); break;
    case SDL_EVENT_DISPLAY_DESKTOP_MODE_CHANGED: printf("DISPLAY_DESKTOP_MODE_CHANGED\n"); break;
    case SDL_EVENT_DISPLAY_CURRENT_MODE_CHANGED: printf("DISPLAY_CURRENT_MODE_CHANGED\n"); break;
    case SDL_EVENT_KEYMAP_CHANGED:
    case SDL_EVENT_KEYBOARD_ADDED:
    case SDL_EVENT_MOUSE_ADDED:
    case SDL_EVENT_RENDER_TARGETS_RESET:
        // Of SDL's own making, and of no interest here.
        break;
    default:
        printf("event 0x%x\n", (unsigned)event->type);
        break;
    }
}

int main(int argc, char **argv)
{
    SDL_WindowFlags flags = SDL_WINDOW_RESIZABLE;
    SDL_PropertiesID props;
    const char *target = NULL;
    const SDL_DisplayMode *mode;
    SDL_Rect bounds;
    int w = 0, h = 0, pw = 0, ph = 0;
    Uint32 quit_after = 0;
    Uint32 interrupt_after = 0;
    Uint32 animate = 0;

    for (int i = 1; i < argc; i++) {
        if (strcmp(argv[i], "--hidpi") == 0) {
            flags |= SDL_WINDOW_HIGH_PIXEL_DENSITY;
        } else if (strcmp(argv[i], "--surface") == 0) {
            use_surface = true;
        } else if (strcmp(argv[i], "--target") == 0 && i + 1 < argc) {
            target = argv[++i];
        } else if (strcmp(argv[i], "--quit-after") == 0 && i + 1 < argc) {
            quit_after = (Uint32)SDL_atoi(argv[++i]);
        } else if (strcmp(argv[i], "--interrupt-after") == 0 && i + 1 < argc) {
            interrupt_after = (Uint32)SDL_atoi(argv[++i]);
        } else if (strcmp(argv[i], "--animate") == 0 && i + 1 < argc) {
            animate = (Uint32)SDL_atoi(argv[++i]);
        }
    }
    setvbuf(stdout, NULL, _IOLBF, 0);

    if (!SDL_Init(SDL_INIT_VIDEO)) {
        printf("SDL_Init: %s\n", SDL_GetError());
        return 1;
    }
    printf("video driver %s\n", SDL_GetCurrentVideoDriver());
    mode = SDL_GetDesktopDisplayMode(SDL_GetPrimaryDisplay());
    if (mode && SDL_GetDisplayBounds(SDL_GetPrimaryDisplay(), &bounds)) {
        printf("display %dx%d density=%g scale=%g bounds=%d,%d %dx%d\n", mode->w, mode->h, mode->pixel_density, SDL_GetDisplayContentScale(SDL_GetPrimaryDisplay()), bounds.x, bounds.y, bounds.w, bounds.h);
    }

    props = SDL_CreateProperties();
    SDL_SetStringProperty(props, SDL_PROP_WINDOW_CREATE_TITLE_STRING, "SDL on Wasmer");
    SDL_SetNumberProperty(props, SDL_PROP_WINDOW_CREATE_WIDTH_NUMBER, 640);
    SDL_SetNumberProperty(props, SDL_PROP_WINDOW_CREATE_HEIGHT_NUMBER, 480);
    SDL_SetNumberProperty(props, SDL_PROP_WINDOW_CREATE_FLAGS_NUMBER, (Sint64)flags);
    if (target) {
        SDL_SetStringProperty(props, "SDL.window.create.wasmer.target", target);
    }
    window = SDL_CreateWindowWithProperties(props);
    SDL_DestroyProperties(props);
    if (!window) {
        printf("SDL_CreateWindow: %s\n", SDL_GetError());
        SDL_Quit();
        return 1;
    }
    SDL_GetWindowSize(window, &w, &h);
    SDL_GetWindowSizeInPixels(window, &pw, &ph);
    printf("window %dx%d, %dx%d pixels, density=%g scale=%g\n", w, h, pw, ph, SDL_GetWindowPixelDensity(window), SDL_GetWindowDisplayScale(window));
    {
        // The name a WebGPU surface for the window is made with. The host
        // makes one up for each window, unless the window adopted a target.
        const char *selector = SDL_GetStringProperty(SDL_GetWindowProperties(window), "SDL.window.wasmer.surface_selector", "");
        if (strncmp(selector, "wgui:", 5) == 0) {
            printf("surface selector wgui: and %d more\n", (int)strlen(selector) - 5);
        } else {
            printf("surface selector \"%s\"\n", selector);
        }
    }

    if (!use_surface) {
        renderer = SDL_CreateRenderer(window, NULL);
        if (!renderer) {
            printf("SDL_CreateRenderer: %s\n", SDL_GetError());
            SDL_Quit();
            return 1;
        }
        printf("renderer %s, vsync %s\n", SDL_GetRendererName(renderer), SDL_SetRenderVSync(renderer, 1) ? "on" : "not to be had");
    }

    if (quit_after) {
        SDL_AddTimer(quit_after, quit_timer, NULL);
    }
    if (interrupt_after) {
        SDL_AddTimer(interrupt_after, interrupt_timer, NULL);
    }

    if (animate) {
        // A loop that never waits for its user: what paces it is the
        // renderer's vsync, which is the display's.
        const Uint64 started = SDL_GetTicks();
        int drawn = 0;
        while (running && SDL_GetTicks() - started < animate) {
            SDL_Event event;
            while (SDL_PollEvent(&event)) {
                if (event.type == SDL_EVENT_QUIT) {
                    running = false;
                }
            }
            SDL_SetRenderDrawColor(renderer, (Uint8)(drawn * 3), 60, 90, 255);
            SDL_RenderClear(renderer);
            SDL_RenderPresent(renderer);
            drawn++;
        }
        printf("animated %d frames in %d ms\n", drawn, (int)(SDL_GetTicks() - started));
        running = false;
    }

    while (running) {
        SDL_Event event;
        if (!SDL_WaitEvent(&event)) {
            printf("SDL_WaitEvent: %s\n", SDL_GetError());
            break;
        }
        on_event(&event);
    }

    SDL_GetWindowSize(window, &w, &h);
    printf("quitting at %dx%d after %d frames\n", w, h, frames);
    SDL_DestroyRenderer(renderer);
    SDL_DestroyWindow(window);
    SDL_Quit();
    printf("done\n");
    return 0;
}

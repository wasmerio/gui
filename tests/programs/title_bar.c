// A window whose title bar the program draws: a toolbar with a pill of
// buttons and an address field, as a browser has, in the style named on the
// command line (system, overlay or custom). It asks for that style, lays its
// toolbar out by the one it got, and marks the toolbar less its controls as
// what moves the window. In the custom style it draws minimize, maximize and
// close buttons of its own.
//
// It prints what it was given and every press that reaches it. With --once
// it stops after its first frame, which is how the tests run it; otherwise
// it runs until its window is closed, for trying it in a real window; with
// --fullscreen it also goes fullscreen and back once, printing the title bar
// it has in each. With --fixed its window is not resizable, which a double
// click on its toolbar must not change.

#include "wgui_test.h"

static WGUIInstance instance;
static WGUIWindow window;
static uint64_t buffer[4 * WGUI_MAX_EVENT_SIZE / 8];
static uint32_t width, height;
static double scale = 1;
static WGUIWindowState state = WGUI_WINDOW_STATE_INIT;
static uint8_t* pixels;

typedef struct Box {
    int32_t x, y, width, height;
} Box;

// The toolbar's parts, in physical pixels: the band, the pill, the address
// field and, in the custom style, the window's own buttons.
static Box band, pill, field, buttons[3];
static int has_buttons;

static int inside(Box box, double x, double y) {
    return x >= box.x && y >= box.y && x < box.x + box.width && y < box.y + box.height;
}

static int32_t px(double logical) {
    return (int32_t)(logical * scale + 0.5);
}

static void layout(void) {
    OK(wguiWindowGetState(window, &state));
    int custom = state.titleBar == WGUITitleBarStyle_Custom;
    int own = state.titleBar != WGUITitleBarStyle_System && state.titleBarHeight != 0;
    // Where the title bar is the program's, the toolbar is that tall and
    // starts where the system's controls end; else it is the program's own
    // 48 units at the top of the content.
    band = (Box){ 0, 0, (int32_t)width, own ? (int32_t)state.titleBarHeight : px(48) };
    int32_t start = own ? (int32_t)state.titleBarX : 0;
    int32_t end = own ? (int32_t)(state.titleBarX + state.titleBarWidth) : (int32_t)width;
    has_buttons = custom && own;
    int32_t button = px(46);
    if (has_buttons) {
        for (int i = 0; i < 3; i++) {
            buttons[i] = (Box){ end - (3 - i) * button, 0, button, band.height };
        }
        end -= 3 * button;
    }
    int32_t tall = px(30);
    int32_t top = (band.height - tall) / 2;
    pill = (Box){ start + px(12), top, px(72), tall };
    field = (Box){ pill.x + pill.width + px(12), top, 0, tall };
    field.width = end - px(12) - field.x;
    if (field.width < 0) field.width = 0;
    printf("title bar %s, area %ux%u at %u,%u, band %d tall\n",
           wt_name(wguiTitleBarStyleName(state.titleBar)), state.titleBarWidth, state.titleBarHeight,
           state.titleBarX, state.titleBarY, band.height);

    // The band moves the window, less the controls in it: the slices
    // between and around them.
    WGUIRect regions[16];
    size_t count = 0;
    Box controls[5] = { pill, field };
    size_t control_count = 2;
    for (int i = 0; has_buttons && i < 3; i++) controls[control_count++] = buttons[i];
    int32_t x = band.x;
    for (size_t i = 0; i < control_count; i++) {
        Box c = controls[i];
        if (c.x > x) regions[count++] = (WGUIRect){ x, band.y, (uint32_t)(c.x - x), (uint32_t)band.height };
        if (c.y > band.y) regions[count++] = (WGUIRect){ c.x, band.y, (uint32_t)c.width, (uint32_t)(c.y - band.y) };
        int32_t below = band.y + band.height - (c.y + c.height);
        if (below > 0) regions[count++] = (WGUIRect){ c.x, c.y + c.height, (uint32_t)c.width, (uint32_t)below };
        x = c.x + c.width;
    }
    if (band.x + band.width > x) regions[count++] = (WGUIRect){ x, band.y, (uint32_t)(band.x + band.width - x), (uint32_t)band.height };
    WGUIStatus status = wguiWindowSetDragRegions(window, count, regions);
    printf("drag regions: %zu, %s\n", count, wt_name(wguiStatusName(status)));
}

static void fill(Box box, uint32_t rgb) {
    for (int32_t y = box.y < 0 ? 0 : box.y; y < box.y + box.height && y < (int32_t)height; y++) {
        for (int32_t x = box.x < 0 ? 0 : box.x; x < box.x + box.width && x < (int32_t)width; x++) {
            uint8_t* p = pixels + ((size_t)y * width + (size_t)x) * 4;
            p[0] = (uint8_t)(rgb >> 16);
            p[1] = (uint8_t)(rgb >> 8);
            p[2] = (uint8_t)rgb;
            p[3] = 0xFF;
        }
    }
}

static void draw(void) {
    free(pixels);
    pixels = malloc((size_t)width * height * 4);
    CHECK(pixels != NULL);
    fill((Box){ 0, 0, (int32_t)width, (int32_t)height }, 0xFFFFFF);
    fill(band, 0xF2F3F5);
    fill((Box){ 0, band.height - 1, (int32_t)width, 1 }, 0xD0D3D8);
    fill(pill, 0xC9CED6);
    fill(field, 0xE3E6EA);
    if (has_buttons) {
        fill(buttons[0], 0xDADDE2);
        fill(buttons[1], 0xCDD1D7);
        fill(buttons[2], 0xE5484D);
    }
    // Where the system said the program's part of the title bar is.
    if (state.titleBarHeight != 0) {
        Box area = { (int32_t)state.titleBarX, (int32_t)state.titleBarY, (int32_t)state.titleBarWidth, (int32_t)state.titleBarHeight };
        fill((Box){ area.x, area.y, area.width, px(2) }, 0x2EA043);
        fill((Box){ area.x, area.y, px(2), area.height }, 0x2EA043);
    }
    WGUIPixelFrame frame = WGUI_PIXEL_FRAME_INIT;
    frame.format = WGUIPixelFormat_Rgba8;
    frame.alpha = WGUIPixelAlpha_Opaque;
    frame.width = width;
    frame.height = height;
    frame.pixels = pixels;
    frame.pixelsSize = (size_t)width * height * 4;
    OK(wguiWindowPresentPixels(window, &frame, 0, NULL));
}

static const char* what_is_at(double x, double y) {
    if (inside(pill, x, y)) return "pill";
    if (inside(field, x, y)) return "field";
    for (int i = 0; has_buttons && i < 3; i++) {
        if (inside(buttons[i], x, y)) return (const char*[]){ "minimize", "maximize", "close" }[i];
    }
    if (inside(band, x, y)) return "toolbar";
    return "content";
}

int main(int argc, char** argv) {
    setvbuf(stdout, NULL, _IOLBF, 0);
    WGUITitleBarStyle wanted = WGUITitleBarStyle_Overlay;
    int once = 0, fullscreen_test = 0, fixed = 0, phase = 0;
    uint64_t entered = 0;
    for (int i = 1; i < argc; i++) {
        if (!strcmp(argv[i], "system")) wanted = WGUITitleBarStyle_System;
        else if (!strcmp(argv[i], "overlay")) wanted = WGUITitleBarStyle_Overlay;
        else if (!strcmp(argv[i], "custom")) wanted = WGUITitleBarStyle_Custom;
        else if (!strcmp(argv[i], "--once")) once = 1;
        else if (!strcmp(argv[i], "--fullscreen")) fullscreen_test = 1;
        else if (!strcmp(argv[i], "--fixed")) fixed = 1;
    }
    OK(wguiCreateInstance(NULL, &instance));
    printf("overlay: %s, custom: %s, drag regions: %s\n",
           wguiHasFeature(WGUIFeatureName_TitleBarOverlay) ? "yes" : "no",
           wguiHasFeature(WGUIFeatureName_TitleBarCustom) ? "yes" : "no",
           wguiHasFeature(WGUIFeatureName_DragRegions) ? "yes" : "no");

    WGUIWindowDescriptor descriptor = WGUI_WINDOW_DESCRIPTOR_INIT;
    descriptor.title.data = "title bar";
    descriptor.options = WGUIWindowOptions_LogicalUnits;
    if (fixed) descriptor.flags = WGUIWindowFlags_Visible | WGUIWindowFlags_Decorated;
    descriptor.width = 760;
    descriptor.height = 460;
    descriptor.titleBar = wanted;
    descriptor.titleBarHeight = 48;
    OK(wguiInstanceCreateWindow(instance, &descriptor, &window));
    printf("asked for %s\n", wt_name(wguiTitleBarStyleName(wanted)));

    for (;;) {
        size_t written = 0;
        OK(wguiInstancePollEvents(instance, buffer, sizeof buffer, &written));
        int redraw = 0;
        for (const WGUIEventHeader* event = wguiEventFirst(buffer, written); event != NULL;
             event = wguiEventNext(buffer, written, event)) {
            switch (event->type) {
            case WGUIEventType_WindowResized: {
                const WGUIWindowResizedEvent* size = (const WGUIWindowResizedEvent*)event;
                width = size->width;
                height = size->height;
                scale = size->scaleFactor;
                redraw = 1;
                break;
            }
            case WGUIEventType_WindowTitleBarChanged:
                printf("title bar changed\n");
                redraw = 1;
                break;
            case WGUIEventType_WindowRedrawRequested:
                redraw = 1;
                break;
            case WGUIEventType_PointerDown: {
                const WGUIPointerEvent* pointer = (const WGUIPointerEvent*)event;
                const char* what = what_is_at(pointer->x, pointer->y);
                printf("press at %.0f,%.0f: %s\n", pointer->x, pointer->y, what);
                if (!strcmp(what, "minimize")) {
                    OK(wguiWindowSetFlags(window, WGUIWindowFlags_Minimized, WGUIWindowFlags_Minimized, NULL));
                } else if (!strcmp(what, "maximize")) {
                    WGUIWindowState now = WGUI_WINDOW_STATE_INIT;
                    OK(wguiWindowGetState(window, &now));
                    OK(wguiWindowSetFlags(window, WGUIWindowFlags_Maximized,
                                          (now.flags & WGUIWindowFlags_Maximized) ? 0 : WGUIWindowFlags_Maximized, NULL));
                } else if (!strcmp(what, "close")) {
                    printf("closed\n");
                    return 0;
                }
                break;
            }
            case WGUIEventType_PointerUp: {
                const WGUIPointerEvent* pointer = (const WGUIPointerEvent*)event;
                printf("release at %.0f,%.0f\n", pointer->x, pointer->y);
                break;
            }
            case WGUIEventType_WindowCloseRequested:
                printf("closed\n");
                return 0;
            default:
                break;
            }
        }
        if (redraw && width != 0) {
            layout();
            draw();
            if (once) {
                printf("done\n");
                return 0;
            }
        }
        // Fullscreen and back: 1 asked to enter, 2 in it (macOS animates the
        // way in, and ignores a way out meanwhile), 3 asked to leave.
        if (fullscreen_test && width != 0) {
            uint64_t now = 0;
            OK(wguiInstanceGetTime(instance, &now));
            OK(wguiWindowGetState(window, &state));
            if (phase == 0) {
                WGUIFullscreenOptions options = WGUI_FULLSCREEN_OPTIONS_INIT;
                options.mode = WGUIFullscreenMode_Borderless;
                OK(wguiWindowSetFullscreen(window, &options, 1));
                phase = 1;
            } else if (phase == 1 && state.fullscreen != WGUIFullscreenMode_None) {
                printf("in fullscreen\n");
                entered = now;
                phase = 2;
            } else if (phase == 2 && now - entered > 2000000000ull) {
                OK(wguiWindowSetFullscreen(window, NULL, 2));
                phase = 3;
            } else if (phase == 3 && state.fullscreen == WGUIFullscreenMode_None) {
                printf("out of fullscreen\n");
                phase = 4;
            }
        }
        WGUIWaitReasons reasons = 0;
        OK(wguiInstanceWaitEvents(instance, 250 * 1000 * 1000, &reasons));
    }
}

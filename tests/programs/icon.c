// A program with an icon of its own, drawn here at several sizes: a rounded
// square with a ring, orange for every platform, and a blue one inset within
// a margin for macOS, as icons are there. Which colour the Dock, the task bar
// or the page shows says which version the host kept.
//
// It prints what the host made of each call. With --once it stops once its
// window is open, which is how the tests run it; otherwise it runs until its
// window is closed, for looking at the icon.

#include <math.h>

#include "wgui_test.h"

// A 2 by 2 PNG file, blue.
static const unsigned char png_file[] = {
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
    0x52, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x02, 0x08, 0x06, 0x00, 0x00, 0x00, 0x72,
    0xB6, 0x0D, 0x24, 0x00, 0x00, 0x00, 0x11, 0x49, 0x44, 0x41, 0x54, 0x78, 0xDA, 0x63, 0xB0,
    0x6E, 0xFA, 0xF6, 0x1F, 0x84, 0x19, 0x60, 0x0C, 0x00, 0x5B, 0x9A, 0x0A, 0xC9, 0x55, 0x7B,
    0x33, 0x59, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
};

typedef struct Colour {
    double r, g, b;
} Colour;

// How much of the pixel at (x, y) a shape covers whose edge is `distance`
// away, inside where it is negative: one pixel of soft edge.
static double coverage(double distance) {
    return distance <= -0.5 ? 1 : distance >= 0.5 ? 0 : 0.5 - distance;
}

// A `size` by `size` image: a rounded square `inset` of the size in from
// each edge, in `colour`, with a white ring in its middle.
static uint8_t* draw(uint32_t size, double inset, Colour colour) {
    uint8_t* rgba = malloc((size_t)size * size * 4);
    CHECK(rgba != NULL);
    double margin = size * inset, half = size / 2.0 - margin, radius = half * 0.45;
    for (uint32_t y = 0; y < size; y++) {
        for (uint32_t x = 0; x < size; x++) {
            double px = x + 0.5 - size / 2.0, py = y + 0.5 - size / 2.0;
            // The rounded square, by its signed distance.
            double qx = fabs(px) - (half - radius), qy = fabs(py) - (half - radius);
            double outside = hypot(fmax(qx, 0), fmax(qy, 0)) + fmin(fmax(qx, qy), 0) - radius;
            double body = coverage(outside);
            // The ring.
            double ring = coverage(fabs(hypot(px, py) - half * 0.45) - half * 0.1);
            uint8_t* pixel = rgba + ((size_t)y * size + x) * 4;
            pixel[0] = (uint8_t)lround(255 * (colour.r * (1 - ring) + ring));
            pixel[1] = (uint8_t)lround(255 * (colour.g * (1 - ring) + ring));
            pixel[2] = (uint8_t)lround(255 * (colour.b * (1 - ring) + ring));
            pixel[3] = (uint8_t)lround(255 * body);
        }
    }
    return rgba;
}

static WGUIIconImage image(uint32_t size, WGUIPlatforms platforms, const void* data, size_t bytes) {
    WGUIIconImage image = WGUI_ICON_IMAGE_INIT;
    image.width = image.height = size;
    image.format = WGUIIconFormat_Rgba8;
    image.platforms = platforms;
    image.data = data;
    image.size = bytes;
    return image;
}

static void report(const char* what, WGUIStatus status) {
    printf("%s: %s\n", what, wt_name(wguiStatusName(status)));
}

int main(int argc, char** argv) {
    int once = argc > 1 && strcmp(argv[1], "--once") == 0;
    WGUIInstance instance = 0;
    OK(wguiCreateInstance(NULL, &instance));
    printf("app icon: %s\n", wguiHasFeature(WGUIFeatureName_AppIcon) ? "yes" : "no");

    WGUIIconImage file = WGUI_ICON_IMAGE_INIT;
    file.width = file.height = 2;
    file.format = WGUIIconFormat_Png;
    file.data = png_file;
    file.size = sizeof png_file;
    report("a PNG file", wguiInstanceSetIcon(instance, 1, &file));

    uint8_t wrong[16] = { 0 };
    WGUIIconImage mismatched = image(4, 0, wrong, sizeof wrong);
    report("pixels for another size", wguiInstanceSetIcon(instance, 1, &mismatched));

    WGUIIconImage elsewhere = image(2, WGUIPlatforms_Ios, wrong, sizeof wrong);
    report("only for iOS", wguiInstanceSetIcon(instance, 1, &elsewhere));

    // Every platform's, and macOS's, which keeps a margin.
    static const uint32_t sizes[] = { 16, 32, 64, 128, 256, 512 };
    enum { SIZES = sizeof sizes / sizeof sizes[0] };
    const Colour orange = { 0.96, 0.55, 0.15 }, blue = { 0.15, 0.45, 0.95 };
    WGUIIconImage images[2 * SIZES];
    for (size_t i = 0; i < SIZES; i++) {
        uint32_t size = sizes[i];
        size_t bytes = (size_t)size * size * 4;
        images[i] = image(size, WGUIPlatforms_None, draw(size, 0.02, orange), bytes);
        images[SIZES + i] = image(size, WGUIPlatforms_Macos, draw(size, 0.1, blue), bytes);
    }
    report("the icon", wguiInstanceSetIcon(instance, 2 * SIZES, images));
    // The host has its own copy.
    for (size_t i = 0; i < 2 * SIZES; i++) {
        free((void*)images[i].data);
    }

    WGUIWindowDescriptor descriptor = WGUI_WINDOW_DESCRIPTOR_INIT;
    descriptor.title.data = "An icon of its own";
    descriptor.options = WGUIWindowOptions_LogicalUnits;
    descriptor.width = 360;
    descriptor.height = 240;
    WGUIWindow window = 0;
    OK(wguiInstanceCreateWindow(instance, &descriptor, &window));

    static uint64_t buffer[4 * WGUI_MAX_EVENT_SIZE / 8];
    uint8_t* frame = NULL;
    for (int running = !once; running;) {
        WGUIWaitReasons reasons = 0;
        OK(wguiInstanceWaitEvents(instance, WGUI_WAIT_FOREVER, &reasons));
        size_t written = 0;
        OK(wguiInstancePollEvents(instance, buffer, sizeof buffer, &written));
        const WGUIEventHeader* event = wguiEventFirst(buffer, written);
        for (; event != NULL; event = wguiEventNext(buffer, written, event)) {
            if (event->type == WGUIEventType_WindowCloseRequested) {
                running = 0;
            } else if (event->type == WGUIEventType_WindowRedrawRequested) {
                WGUIWindowState state = WGUI_WINDOW_STATE_INIT;
                OK(wguiWindowGetState(window, &state));
                size_t bytes = (size_t)state.width * state.height * 4;
                free(frame);
                frame = malloc(bytes);
                CHECK(frame != NULL);
                memset(frame, 0xF0, bytes);
                WGUIPixelFrame pixels = WGUI_PIXEL_FRAME_INIT;
                pixels.width = state.width;
                pixels.height = state.height;
                pixels.pixels = frame;
                pixels.pixelsSize = bytes;
                wguiWindowPresentPixels(window, &pixels, 0, NULL);
            }
        }
    }
    report("none", wguiInstanceSetIcon(instance, 0, NULL));
    free(frame);
    wguiInstanceDestroy(instance);
    printf("done\n");
    return 0;
}

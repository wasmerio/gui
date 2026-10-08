// Draws without a GPU: fills its window with a pattern it computes itself,
// presents that, and does so again for each size the window is given.
//
// The pattern is the same whatever the frame's size and layout, so that
// whoever looks at the window afterwards knows what should be there: red
// grows to the right and green downward, blue is 0x40 everywhere, and the
// top-left 16 by 16 pixels end up white.

#include "wgui_test.h"

static WGUIInstance instance;
static WGUIWindow window;
static uint64_t buffer[2 * WGUI_MAX_EVENT_SIZE / 8];
static uint32_t width, height;
static double scale;

// Waits up to five seconds for a record of this type.
static const WGUIEventHeader* expect(WGUIEventType type) {
    uint64_t start = 0, now = 0;
    OK(wguiInstanceGetTime(instance, &start));
    for (;;) {
        size_t written = 0;
        OK(wguiInstancePollEvents(instance, buffer, sizeof buffer, &written));
        const WGUIEventHeader* event = wguiEventFirst(buffer, written);
        const WGUIEventHeader* found = NULL;
        for (; event != NULL; event = wguiEventNext(buffer, written, event)) {
            if (event->type == WGUIEventType_WindowResized) {
                const WGUIWindowResizedEvent* size = (const WGUIWindowResizedEvent*)event;
                width = size->width;
                height = size->height;
                scale = size->scaleFactor;
            }
            if (event->type == type && found == NULL) {
                found = event;
            }
        }
        if (found != NULL) {
            return found;
        }
        OK(wguiInstanceGetTime(instance, &now));
        if (now - start > 5000000000ull) {
            fprintf(stderr, "no %s within five seconds\n", wt_name(wguiEventTypeName(type)));
            exit(1);
        }
        WGUIWaitReasons reasons = 0;
        OK(wguiInstanceWaitEvents(instance, 100 * 1000 * 1000, &reasons));
    }
}

// The pattern, in the layout `frame` describes. `padding` is what each row
// has after its pixels, which is filled with a value that must never show.
static void* draw(WGUIPixelFrame* frame, uint32_t padding) {
    uint32_t stride = width * 4 + padding;
    uint8_t* pixels = malloc((size_t)stride * height);
    CHECK(pixels != NULL);
    memset(pixels, 0xEE, (size_t)stride * height);
    for (uint32_t y = 0; y < height; y++) {
        for (uint32_t x = 0; x < width; x++) {
            uint8_t red = width > 1 ? (uint8_t)(x * 255 / (width - 1)) : 0;
            uint8_t green = height > 1 ? (uint8_t)(y * 255 / (height - 1)) : 0;
            uint8_t* pixel = pixels + (size_t)y * stride + (size_t)x * 4;
            int bgra = frame->format == WGUIPixelFormat_Bgra8;
            pixel[bgra ? 2 : 0] = red;
            pixel[1] = green;
            pixel[bgra ? 0 : 2] = 0x40;
            // An opaque frame's fourth byte means nothing: it must not
            // matter that this one says "transparent".
            pixel[3] = 0;
        }
    }
    frame->width = width;
    frame->height = height;
    frame->stride = padding != 0 ? stride : 0;
    frame->pixels = pixels;
    frame->pixelsSize = (size_t)stride * height;
    return pixels;
}

static void present(const char* what, WGUIPixelFrame* frame, size_t damage_count, const WGUIRect* damage) {
    WGUIStatus status = wguiWindowPresentPixels(window, frame, damage_count, damage);
    printf("%s: %s\n", what, wt_name(wguiStatusName(status)));
}

int main(void) {
    setvbuf(stdout, NULL, _IOLBF, 0);
    OK(wguiCreateInstance(NULL, &instance));
    printf("frames can be presented: %s\n", wguiHasFeature(WGUIFeatureName_PixelPresent) ? "yes" : "no");

    WGUIWindowDescriptor descriptor = WGUI_WINDOW_DESCRIPTOR_INIT;
    descriptor.title.data = "pixels";
    descriptor.options = WGUIWindowOptions_LogicalUnits;
    descriptor.width = 320;
    descriptor.height = 200;
    OK(wguiInstanceCreateWindow(instance, &descriptor, &window));
    expect(WGUIEventType_WindowResized);
    printf("opened at %gx%g logical units\n", width / scale, height / scale);

    // The first frame: the layout most software renderers have.
    WGUIPixelFrame frame = WGUI_PIXEL_FRAME_INIT;
    void* pixels = draw(&frame, 0);
    present("the first frame", &frame, 0, NULL);
    // The frame was copied: the memory is the program's again at once.
    memset(pixels, 0x11, frame.pixelsSize);
    free(pixels);

    // A window that changes size is drawn again, at the new one.
    OK(wguiWindowSetSize(window, (uint32_t)(400 * scale), (uint32_t)(250 * scale)));
    expect(WGUIEventType_WindowResized);
    printf("resized to %gx%g logical units\n", width / scale, height / scale);
    WGUIPixelFrame padded = WGUI_PIXEL_FRAME_INIT;
    padded.format = WGUIPixelFormat_Rgba8;
    pixels = draw(&padded, 12);
    present("the same in another layout, with padded rows", &padded, 0, NULL);

    // Only a corner changes: the rest of the frame is as it was presented.
    for (uint32_t y = 0; y < 16; y++) {
        memset((uint8_t*)pixels + (size_t)y * padded.stride, 0xFF, 16 * 4);
    }
    WGUIRect corner = { 0, 0, 16, 16 };
    present("a corner of it", &padded, 1, &corner);
    // What lies outside the frame says nothing, and is not an error.
    WGUIRect outside = { (int32_t)width, 0, 10, 10 };
    present("nothing of it", &padded, 1, &outside);

    // What does not add up is refused, and shows nothing.
    WGUIPixelFrame wrong = padded;
    wrong.pixelsSize = 16;
    present("a frame bigger than its memory", &wrong, 0, NULL);
    wrong = padded;
    wrong.stride = 8;
    present("rows shorter than their pixels", &wrong, 0, NULL);
    wrong = padded;
    wrong.height = 0;
    present("no rows at all", &wrong, 0, NULL);
    wrong = padded;
    wrong.format = (WGUIPixelFormat)77;
    present("a layout nobody knows", &wrong, 0, NULL);

    // A program that animates draws when it is asked to.
    OK(wguiWindowRequestRedraw(window));
    expect(WGUIEventType_WindowRedrawRequested);
    present("a frame when asked for one", &padded, 0, NULL);
    free(pixels);

    printf("done\n");
    // Left open: what was presented last is there for whoever looks.
    fflush(stdout);
    return 0;
}

// Opens a window, asks a few things of it, and closes it again, without
// anybody at the keyboard. On a real window system this shows that a window
// can be made, asked about, changed and drawn; what it prints does not depend
// on the display it runs on.

#include "wgui_test.h"

static WGUIInstance instance;
static uint64_t buffer[2 * WGUI_MAX_EVENT_SIZE / 8];

// Waits up to five seconds for a record of this type, and returns it.
static const WGUIEventHeader* expect(WGUIEventType type) {
    uint64_t start = 0, now = 0;
    OK(wguiInstanceGetTime(instance, &start));
    for (;;) {
        size_t written = 0;
        OK(wguiInstancePollEvents(instance, buffer, sizeof buffer, &written));
        const WGUIEventHeader* event = wguiEventFirst(buffer, written);
        for (; event != NULL; event = wguiEventNext(buffer, written, event)) {
            if (event->type == type) {
                return event;
            }
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

// Wait several display intervals so a redraw that was queued would arrive.
static void expect_no_redraw(WGUIWindow window) {
    uint64_t now = 0;
    OK(wguiInstanceGetTime(instance, &now));
    const uint64_t deadline = now + 125000000ull;
    for (;;) {
        size_t written = 0;
        OK(wguiInstancePollEvents(instance, buffer, sizeof buffer, &written));
        const WGUIEventHeader* event = wguiEventFirst(buffer, written);
        for (; event != NULL; event = wguiEventNext(buffer, written, event))
            CHECK(event->window != window || event->type != WGUIEventType_WindowRedrawRequested);
        OK(wguiInstanceGetTime(instance, &now));
        if (now >= deadline)
            return;
        WGUIWaitReasons reasons = 0;
        OK(wguiInstanceWaitEvents(instance, deadline - now, &reasons));
    }
}

static void check_hidden_redraw(WGUIWindowDescriptor descriptor) {
    WGUIWindow window = 0;
    descriptor.flags &= ~WGUIWindowFlags_Visible;
    OK(wguiInstanceCreateWindow(instance, &descriptor, &window));
    CHECK(expect(WGUIEventType_WindowResized)->window == window);

    WGUIWindowState state = WGUI_WINDOW_STATE_INIT;
    const WGUIWindowFlags visibility = WGUIWindowFlags_Visible | WGUIWindowFlags_Occluded;
    OK(wguiWindowGetState(window, &state));
    CHECK((state.flags & visibility) == WGUIWindowFlags_Occluded);
    OK(wguiWindowRequestRedraw(window));
    OK(wguiWindowRequestRedraw(window));
    expect_no_redraw(window);

    // Showing the window delivers an exposure without another guest request.
    OK(wguiWindowSetFlags(window, WGUIWindowFlags_Visible, WGUIWindowFlags_Visible, NULL));
    CHECK(expect(WGUIEventType_WindowRedrawRequested)->window == window);
    OK(wguiWindowGetState(window, &state));
    CHECK(state.flags & WGUIWindowFlags_Visible);

    OK(wguiWindowSetFlags(window, WGUIWindowFlags_Visible, 0, NULL));
    OK(wguiWindowGetState(window, &state));
    while (state.flags & WGUIWindowFlags_Visible) {
        expect(WGUIEventType_WindowFlagsChanged);
        OK(wguiWindowGetState(window, &state));
    }
    CHECK((state.flags & visibility) == WGUIWindowFlags_Occluded);
    OK(wguiWindowRequestRedraw(window));
    expect_no_redraw(window);

    OK(wguiWindowSetFlags(window, WGUIWindowFlags_Visible, WGUIWindowFlags_Visible, NULL));
    CHECK(expect(WGUIEventType_WindowRedrawRequested)->window == window);
    wguiWindowDestroy(window);
}

int main(void) {
    setvbuf(stdout, NULL, _IOLBF, 0);
    OK(wguiCreateInstance(NULL, &instance));

    WGUIDisplay displays[8];
    size_t display_count = 0;
    OK(wguiInstanceGetDisplays(instance, 8, displays, &display_count));
    CHECK(display_count >= 1);
    WGUIDisplayInfo display = WGUI_DISPLAY_INFO_INIT;
    OK(wguiDisplayGetInfo(displays[0], &display));
    CHECK(display.width > 0 && display.height > 0 && display.scaleFactor >= 1.0);
    printf("the first display is the primary one: %s\n",
           (display.flags & WGUIDisplayFlags_Primary) ? "yes" : "no");

    WGUIWindowDescriptor descriptor = WGUI_WINDOW_DESCRIPTOR_INIT;
    descriptor.title.data = "wasmer-gui smoke test";
    descriptor.options = WGUIWindowOptions_LogicalUnits;
    descriptor.width = 320;
    descriptor.height = 200;
    WGUIWindow window = 0;
    OK(wguiInstanceCreateWindow(instance, &descriptor, &window));

    // The window is announced by its size, in physical pixels.
    const WGUIWindowResizedEvent* size = (const WGUIWindowResizedEvent*)expect(WGUIEventType_WindowResized);
    CHECK(size->header.window == window);
    printf("opened at %gx%g logical units\n", size->width / size->scaleFactor, size->height / size->scaleFactor);

    WGUIWindowState state = WGUI_WINDOW_STATE_INIT;
    OK(wguiWindowGetState(window, &state));
    CHECK(state.width == size->width && state.height == size->height);
    CHECK(state.scaleFactor == size->scaleFactor);
    printf("visible: %s, resizable: %s, decorated: %s\n", (state.flags & WGUIWindowFlags_Visible) ? "yes" : "no",
           (state.flags & WGUIWindowFlags_Resizable) ? "yes" : "no",
           (state.flags & WGUIWindowFlags_Decorated) ? "yes" : "no");
    printf("on a display: %s\n", state.display != 0 ? "yes" : "no");

    char selector[64];
    size_t selector_length = 0;
    OK(wguiWindowGetSurfaceSelector(window, selector, sizeof selector, &selector_length));
    printf("selector: %zu bytes, starts with %.5s\n", selector_length, selector);

    OK(wguiWindowSetTitle(window, "wasmer-gui smoke test (resized)", WGUI_STRLEN));
    OK(wguiWindowSetSize(window, (uint32_t)(400 * state.scaleFactor), (uint32_t)(250 * state.scaleFactor)));
    size = (const WGUIWindowResizedEvent*)expect(WGUIEventType_WindowResized);
    printf("resized to %gx%g logical units\n", size->width / size->scaleFactor, size->height / size->scaleFactor);

    OK(wguiWindowRequestRedraw(window));
    expect(WGUIEventType_WindowRedrawRequested);
    printf("asked to draw\n");

    wguiWindowDestroy(window);

    WGUIInstanceInfo info = WGUI_INSTANCE_INFO_INIT;
    OK(wguiInstanceGetInfo(instance, &info));
    if (info.hostKind == WGUIHostKind_Desktop)
        check_hidden_redraw(descriptor);

    wguiInstanceDestroy(instance);
    printf("done\n");
    return 0;
}

// A handle works on every thread. A second thread changes the window's
// title, sizes it, and wakes the first, which is waiting for events; the
// first then finds both the wake and what the other thread caused.

#include <pthread.h>
#include <unistd.h>

#include "wgui_test.h"

static WGUIInstance instance;
static WGUIWindow window;

static void* other_thread(void* unused) {
    (void)unused;
    // Give the main thread time to be waiting.
    usleep(30 * 1000);
    OK(wguiWindowSetTitle(window, "set from another thread", WGUI_STRLEN));
    OK(wguiWindowSetSize(window, 320, 240));
    OK(wguiInstanceWake(instance));
    return NULL;
}

int main(void) {
    setvbuf(stdout, NULL, _IOLBF, 0);
    OK(wguiCreateInstance(NULL, &instance));
    OK(wguiInstanceCreateWindow(instance, NULL, &window));

    // Read what announces the window, so that the queue is empty.
    uint64_t buffer[WGUI_MAX_EVENT_SIZE / 8];
    size_t written = 0;
    OK(wguiInstancePollEvents(instance, buffer, sizeof buffer, &written));
    CHECK(written != 0);

    pthread_t thread;
    CHECK(pthread_create(&thread, NULL, other_thread, NULL) == 0);

    // The wake may come before or after the resize is seen; wait for both.
    int woken = 0, resized = 0;
    while (!woken || !resized) {
        WGUIWaitReasons reasons = 0;
        OK(wguiInstanceWaitEvents(instance, WGUI_WAIT_FOREVER, &reasons));
        if (reasons & WGUIWaitReasons_Woken) {
            woken = 1;
        }
        if (reasons & WGUIWaitReasons_Events) {
            OK(wguiInstancePollEvents(instance, buffer, sizeof buffer, &written));
            const WGUIEventHeader* event = wguiEventFirst(buffer, written);
            for (; event != NULL; event = wguiEventNext(buffer, written, event)) {
                if (event->type == WGUIEventType_WindowResized) {
                    const WGUIWindowResizedEvent* size = (const WGUIWindowResizedEvent*)event;
                    printf("resized to %ux%u by the other thread\n", size->width, size->height);
                    resized = 1;
                }
            }
        }
    }
    CHECK(pthread_join(thread, NULL) == 0);
    printf("woken by the other thread\n");

    // A wake with nobody waiting ends the next wait, once. A window system
    // may have something to tell at the same moment (X11 has, after a
    // resize), so only the wake is looked at.
    OK(wguiInstanceWake(instance));
    WGUIWaitReasons reasons = 0;
    OK(wguiInstanceWaitEvents(instance, WGUI_WAIT_FOREVER, &reasons));
    CHECK(reasons & WGUIWaitReasons_Woken);
    OK(wguiInstanceWaitEvents(instance, 0, &reasons));
    CHECK(!(reasons & WGUIWaitReasons_Woken));
    printf("a wake waits for the next wait\n");

    wguiInstanceDestroy(instance);
    // Everything made on the instance went with it.
    WGUIWindowState state = WGUI_WINDOW_STATE_INIT;
    CHECK(wguiWindowGetState(window, &state) == WGUIStatus_InvalidHandle);
    printf("done\n");
    return 0;
}

// A program built against a header newer than its host. It imports two
// functions no host has: the host answers 0 for them, WGUIStatus_Unimplemented,
// and the program carries on with what there is.

#include "wgui_test.h"

WGUI_IMPORT(wguiWindowSetOpacityFromTheFuture)
WGUIStatus wguiWindowSetOpacityFromTheFuture(WGUIWindow window, double opacity, uint64_t tag);

WGUI_IMPORT(wguiInstanceForgetEverything)
void wguiInstanceForgetEverything(WGUIInstance instance);

int main(void) {
    WGUIInstance instance = 0;
    WGUIWindow window = 0;
    OK(wguiCreateInstance(NULL, &instance));
    OK(wguiInstanceCreateWindow(instance, NULL, &window));

    WGUIStatus status = wguiWindowSetOpacityFromTheFuture(window, 0.5, 7);
    printf("a function this host does not have: %s\n", wt_name(wguiStatusName(status)));
    CHECK(status == WGUIStatus_Unimplemented);
    wguiInstanceForgetEverything(instance);
    printf("and one that returns nothing: returned\n");

    // What the host does have is unaffected.
    OK(wguiWindowSetTitle(window, "still here", WGUI_STRLEN));
    wguiInstanceDestroy(instance);
    printf("done\n");
    return 0;
}

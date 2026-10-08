// What the API answers when it is used wrongly, or asked for more than it
// may give. Nothing here traps: every mistake is a status.

#include "wgui_test.h"

#define SHOW(call) printf("%s: %s\n", #call, wt_name(wguiStatusName(call)))

int main(void) {
    WGUIInstance instance = 0;
    WGUIWindow window = 0;
    setvbuf(stdout, NULL, _IOLBF, 0);

    // A feature that has to be there and is not.
    WGUIFeatureName gamepads = WGUIFeatureName_Gamepads;
    WGUIInstanceDescriptor needy = WGUI_INSTANCE_DESCRIPTOR_INIT;
    needy.requiredFeatureCount = 1;
    needy.requiredFeatures = &gamepads;
    SHOW(wguiCreateInstance(&needy, &instance));
    CHECK(instance == 0);

    // A chained struct the host does not know is skipped; one that says it
    // must be understood is not.
    WGUIChainedStruct unknown = {NULL, (WGUISType)0x1234};
    WGUIInstanceDescriptor chained = WGUI_INSTANCE_DESCRIPTOR_INIT;
    chained.nextInChain = &unknown;
    SHOW(wguiCreateInstance(&chained, &instance));
    wguiInstanceDestroy(instance);
    unknown.sType = (WGUISType)(0x1234 | WGUI_STYPE_MUST_UNDERSTAND);
    SHOW(wguiCreateInstance(&chained, &instance));

    OK(wguiCreateInstance(NULL, &instance));
    OK(wguiInstanceCreateWindow(instance, NULL, &window));

    // Handles of the wrong kind, and ones that were destroyed.
    SHOW(wguiWindowFocus(instance));
    SHOW(wguiInstanceWake(window));
    SHOW(wguiWindowSetCustomCursor(window, window));
    WGUIWindow gone = 0;
    OK(wguiInstanceCreateWindow(instance, NULL, &gone));
    wguiWindowDestroy(gone);
    SHOW(wguiWindowRequestRedraw(gone));

    // Arguments that mean nothing.
    SHOW(wguiWindowSetSize(window, 0, 100));
    SHOW(wguiWindowSetSizeLimits(window, 500, 0, 100, 0));
    SHOW(wguiWindowSetCursorShape(window, (WGUICursorShape)999));
    SHOW(wguiWindowSetTitle(window, "\xFF\xFE not UTF-8", WGUI_STRLEN));
    SHOW(wguiWindowSetDefaultActions(window, 0x80000000u));
    size_t size = 0;
    SHOW(wguiInstanceReadClipboard(instance, WGUIClipboardSelection_Clipboard, "not a mime type", WGUI_STRLEN,
                                   NULL, 0, &size));
    WGUIFullscreenOptions none = WGUI_FULLSCREEN_OPTIONS_INIT;
    none.mode = WGUIFullscreenMode_None;
    SHOW(wguiWindowSetFullscreen(window, &none, 1));

    // A buffer too small for events, and one that is not aligned.
    uint64_t buffer[WGUI_MAX_EVENT_SIZE / 8];
    size_t written = 99;
    SHOW(wguiInstancePollEvents(instance, buffer, 64, &written));
    SHOW(wguiInstancePollEvents(instance, (char*)buffer + 4, sizeof buffer - 8, &written));
    SHOW(wguiInstancePollEvents(instance, buffer, sizeof buffer, NULL));

    // What this host cannot do, it says; it does not pretend.
    printf("exclusive fullscreen: %s\n", wguiHasFeature(WGUIFeatureName_FullscreenExclusive) ? "yes" : "no");
    WGUIFullscreenOptions exclusive = WGUI_FULLSCREEN_OPTIONS_INIT;
    exclusive.mode = WGUIFullscreenMode_Exclusive;
    SHOW(wguiWindowSetFullscreen(window, &exclusive, 2));
    WGUIClipboardInfo primary = WGUI_CLIPBOARD_INFO_INIT;
    SHOW(wguiInstanceGetClipboardInfo(instance, WGUIClipboardSelection_Primary, &primary));

    // Flags that cannot be asked for are named.
    WGUIWindowFlags refused = 0;
    SHOW(wguiWindowSetFlags(window, WGUIWindowFlags_Focused | WGUIWindowFlags_Minimized,
                            WGUIWindowFlags_Minimized, &refused));
    printf("refused flags: ");
    WT_PRINT_FLAGS(WindowFlags, refused);
    putchar('\n');

    // Text getters say how long the text is, whatever fits.
    char selector[8];
    size_t length = 0;
    SHOW(wguiWindowGetSurfaceSelector(window, selector, sizeof selector, &length));
    printf("selector: %zu bytes, starts with %.5s\n", length, selector);

    // The features there are, and no more than fit.
    WGUIFeatureName features[4];
    size_t count = 0;
    SHOW(wguiGetFeatures(4, features, &count));
    printf("more than four features: %s\n", count > 4 ? "yes" : "no");

    wguiInstanceDestroy(instance);
    wguiInstanceDestroy(instance);
    printf("done\n");
    return 0;
}

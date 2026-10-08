// A window drawn on with WebGPU: three frames, red, then green, then blue.
//
// On the headless window system the frames go to whoever keeps them; in a
// real window they are shown. A real window nobody can see (covered,
// minimised, behind a locked screen) has no frames to give: the program says
// so and ends, which is not the host's failure.
//
// Needs wasmer-webgpu's header and guest library: `make gpu-programs`.

#include <stdbool.h>

#include "../wgui_test.h"

#include <webgpu/webgpu.h>
#include <webgpu/webgpu_wasix.h>

#define FRAMES 3
#define PATIENCE_NS 10000000000ull

static WGUIInstance gui;
static uint32_t width, height;
static int resized;
static int unseen;
static uint64_t buffer[2 * WGUI_MAX_EVENT_SIZE / 8];

static uint64_t now(void) {
    uint64_t time = 0;
    OK(wguiInstanceGetTime(gui, &time));
    return time;
}

// Reads what has happened to the window.
static void pump(void) {
    for (;;) {
        size_t written = 0;
        OK(wguiInstancePollEvents(gui, buffer, sizeof buffer, &written));
        if (written == 0) {
            return;
        }
        const WGUIEventHeader* event = wguiEventFirst(buffer, written);
        for (; event != NULL; event = wguiEventNext(buffer, written, event)) {
            if (event->type == WGUIEventType_WindowResized) {
                const WGUIWindowResizedEvent* size = (const WGUIWindowResizedEvent*)event;
                width = size->width;
                height = size->height;
                resized = 1;
            } else if (event->type == WGUIEventType_WindowFlagsChanged) {
                const WGUIWindowFlagsChangedEvent* change = (const WGUIWindowFlagsChangedEvent*)event;
                unseen = (change->flags & (WGUIWindowFlags_Occluded | WGUIWindowFlags_Minimized)) != 0;
            }
        }
    }
}

static void on_adapter(WGPURequestAdapterStatus status, WGPUAdapter adapter, WGPUStringView message, void* userdata1,
                       void* userdata2) {
    (void)userdata2;
    if (status != WGPURequestAdapterStatus_Success)
        fprintf(stderr, "no adapter: %.*s\n", (int)message.length, message.data);
    *(WGPUAdapter*)userdata1 = adapter;
}

static void on_device(WGPURequestDeviceStatus status, WGPUDevice device, WGPUStringView message, void* userdata1,
                      void* userdata2) {
    (void)userdata2;
    if (status != WGPURequestDeviceStatus_Success)
        fprintf(stderr, "no device: %.*s\n", (int)message.length, message.data);
    *(WGPUDevice*)userdata1 = device;
}

static void on_error(WGPUDevice const* device, WGPUErrorType type, WGPUStringView message, void* userdata1,
                     void* userdata2) {
    (void)device; (void)userdata1; (void)userdata2;
    fprintf(stderr, "WebGPU error (type %d): %.*s\n", (int)type, (int)message.length, message.data);
    exit(1);
}

static void wait_for(WGPUInstance instance, WGPUFuture future) {
    WGPUFutureWaitInfo info = { future, false };
    CHECK(wgpuInstanceWaitAny(instance, 1, &info, UINT64_MAX) == WGPUWaitStatus_Success);
}

static void configure(WGPUSurface surface, WGPUDevice device, WGPUTextureFormat format, WGPUCompositeAlphaMode alpha) {
    WGPUSurfaceConfiguration configuration = WGPU_SURFACE_CONFIGURATION_INIT;
    configuration.device = device;
    configuration.format = format;
    configuration.usage = WGPUTextureUsage_RenderAttachment;
    configuration.width = width;
    configuration.height = height;
    configuration.alphaMode = alpha;
    configuration.presentMode = WGPUPresentMode_Fifo;
    wgpuSurfaceConfigure(surface, &configuration);
    resized = 0;
}

int main(void) {
    setvbuf(stdout, NULL, _IOLBF, 0);
    OK(wguiCreateInstance(NULL, &gui));
    WGUIWindowDescriptor window_descriptor = WGUI_WINDOW_DESCRIPTOR_INIT;
    window_descriptor.title.data = "wasmer-gui surface test";
    window_descriptor.options = WGUIWindowOptions_LogicalUnits;
    window_descriptor.width = 320;
    window_descriptor.height = 200;
    WGUIWindow window = 0;
    OK(wguiInstanceCreateWindow(gui, &window_descriptor, &window));
    // A window is there once it has said how big it is.
    pump();
    CHECK(width != 0 && height != 0);

    char selector[128];
    size_t selector_length = 0;
    OK(wguiWindowGetSurfaceSelector(window, selector, sizeof selector, &selector_length));

    static const WGPUInstanceFeatureName kFeatures[] = { WGPUInstanceFeatureName_TimedWaitAny };
    WGPUInstanceDescriptor instance_descriptor = WGPU_INSTANCE_DESCRIPTOR_INIT;
    instance_descriptor.requiredFeatureCount = 1;
    instance_descriptor.requiredFeatures = kFeatures;
    WGPUInstance instance = wgpuCreateInstance(&instance_descriptor);
    CHECK(instance != NULL);

    WGPUWasixSurfaceSourceCanvas source = WGPU_WASIX_SURFACE_SOURCE_CANVAS_INIT;
    source.selector = (WGPUStringView){ selector, selector_length };
    WGPUSurfaceDescriptor surface_descriptor = WGPU_SURFACE_DESCRIPTOR_INIT;
    surface_descriptor.nextInChain = &source.chain;
    WGPUSurface surface = wgpuInstanceCreateSurface(instance, &surface_descriptor);
    CHECK(surface != NULL);

    WGPUAdapter adapter = NULL;
    WGPURequestAdapterOptions adapter_options = WGPU_REQUEST_ADAPTER_OPTIONS_INIT;
    adapter_options.compatibleSurface = surface;
    WGPURequestAdapterCallbackInfo adapter_callback = WGPU_REQUEST_ADAPTER_CALLBACK_INFO_INIT;
    adapter_callback.mode = WGPUCallbackMode_WaitAnyOnly;
    adapter_callback.callback = on_adapter;
    adapter_callback.userdata1 = &adapter;
    wait_for(instance, wgpuInstanceRequestAdapter(instance, &adapter_options, adapter_callback));
    CHECK(adapter != NULL);

    WGPUDevice device = NULL;
    WGPUDeviceDescriptor device_descriptor = WGPU_DEVICE_DESCRIPTOR_INIT;
    device_descriptor.uncapturedErrorCallbackInfo.callback = on_error;
    WGPURequestDeviceCallbackInfo device_callback = WGPU_REQUEST_DEVICE_CALLBACK_INFO_INIT;
    device_callback.mode = WGPUCallbackMode_WaitAnyOnly;
    device_callback.callback = on_device;
    device_callback.userdata1 = &device;
    wait_for(instance, wgpuAdapterRequestDevice(adapter, &device_descriptor, device_callback));
    CHECK(device != NULL);
    WGPUQueue queue = wgpuDeviceGetQueue(device);

    WGPUSurfaceCapabilities capabilities = WGPU_SURFACE_CAPABILITIES_INIT;
    CHECK(wgpuSurfaceGetCapabilities(surface, adapter, &capabilities) == WGPUStatus_Success);
    CHECK(capabilities.formatCount != 0 && capabilities.alphaModeCount != 0);
    WGPUTextureFormat format = capabilities.formats[0];
    WGPUCompositeAlphaMode alpha = capabilities.alphaModes[0];
    wgpuSurfaceCapabilitiesFreeMembers(capabilities);
    configure(surface, device, format, alpha);
    printf("drawing on the window\n");

    static const WGPUColor kColors[FRAMES] = { { 1, 0, 0, 1 }, { 0, 1, 0, 1 }, { 0, 0, 1, 1 } };
    uint64_t started = now();
    int presented = 0;
    while (presented < FRAMES) {
        pump();
        if (unseen) {
            break;
        }
        if (now() - started > PATIENCE_NS) {
            fprintf(stderr, "no frame to draw to within ten seconds\n");
            return 1;
        }
        if (resized && width != 0 && height != 0) {
            configure(surface, device, format, alpha);
        }

        WGPUSurfaceTexture frame = WGPU_SURFACE_TEXTURE_INIT;
        wgpuSurfaceGetCurrentTexture(surface, &frame);
        if (frame.status == WGPUSurfaceGetCurrentTextureStatus_Timeout) {
            // Nothing to draw to right now. The host has already held this
            // thread back.
            continue;
        }
        if (frame.status == WGPUSurfaceGetCurrentTextureStatus_Outdated ||
            frame.status == WGPUSurfaceGetCurrentTextureStatus_Lost) {
            if (frame.texture) wgpuTextureRelease(frame.texture);
            configure(surface, device, format, alpha);
            continue;
        }
        CHECK(frame.status == WGPUSurfaceGetCurrentTextureStatus_SuccessOptimal ||
              frame.status == WGPUSurfaceGetCurrentTextureStatus_SuccessSuboptimal);

        WGPUTextureView view = wgpuTextureCreateView(frame.texture, NULL);
        WGPURenderPassColorAttachment attachment = WGPU_RENDER_PASS_COLOR_ATTACHMENT_INIT;
        attachment.view = view;
        attachment.loadOp = WGPULoadOp_Clear;
        attachment.storeOp = WGPUStoreOp_Store;
        attachment.clearValue = kColors[presented];
        WGPURenderPassDescriptor pass_descriptor = WGPU_RENDER_PASS_DESCRIPTOR_INIT;
        pass_descriptor.colorAttachmentCount = 1;
        pass_descriptor.colorAttachments = &attachment;
        WGPUCommandEncoder encoder = wgpuDeviceCreateCommandEncoder(device, NULL);
        WGPURenderPassEncoder pass = wgpuCommandEncoderBeginRenderPass(encoder, &pass_descriptor);
        wgpuRenderPassEncoderEnd(pass);
        wgpuRenderPassEncoderRelease(pass);
        WGPUCommandBuffer commands = wgpuCommandEncoderFinish(encoder, NULL);
        wgpuQueueSubmit(queue, 1, &commands);
        wgpuCommandBufferRelease(commands);
        wgpuCommandEncoderRelease(encoder);
        wgpuTextureViewRelease(view);
        CHECK(wgpuSurfacePresent(surface) == WGPUStatus_Success);
        wgpuTextureRelease(frame.texture);
        presented++;
    }
    if (presented == FRAMES) {
        printf("presented %d frames\n", presented);
    } else {
        printf("unseen: the window cannot be seen, so not every frame was presented\n");
    }

    wgpuSurfaceUnconfigure(surface);
    wgpuSurfaceRelease(surface);
    wgpuQueueRelease(queue);
    wgpuDeviceRelease(device);
    wgpuAdapterRelease(adapter);
    wgpuInstanceRelease(instance);
    wguiWindowDestroy(window);
    wguiInstanceDestroy(gui);
    printf("done\n");
    return 0;
}

# Architecture

```
 guest (wasm32-wasix)          host
 ───────────────────           ────────────────────────────────────────────
 #include <wasmer/gui.h>       src/abi      generated glue: decodes arguments
   imports wasmer_gui_v0  ───▶ src/host     the API: handles, queue, rules
                                  │  ▲
                        changes   ▼  │  reports (WindowSink)
                               src/system.rs  the WindowSystem trait
                                  │
                 headless ── winit ── the page of a browser ── an embedder's own
```

## One spec, generated

`spec/gui.json` describes the API in the schema `webgpu.h` is described in.
`codegen/gen.py` turns it into the header, the host's readers and import
glue, the event records, the JavaScript tables for the page half of a browser
host, and three C files that hold the header to its word: static assertions
on every size and offset, a translation unit compiled as C99, C11 and C++,
and a program that calls every function. It also writes the header for Rust
(`sys/`, the `wasmer-gui-sys` crate), with the same names and the same
assertions, checked by the Rust compiler on a 32-bit target.

The generator refuses what would break the API's own rules: a function that
returns something other than a status, a struct passed by value, an event
record with implicit padding.

## The host

`src/host` implements the API once, for every window system. A `Host` is one
scope: the guests an embedder starts and stops together. It owns the handle
table; handles are 32-bit, typed and generation-tagged, so a stale or
mistyped one is a status and never a wrong object.

What happens to a window travels from the window system to the guest through
two halves that share only lock-free state:

- A **`WindowSink`** belongs to the thread that reports: a native event
  loop's, the page's in a browser. It turns plain reports ("this key went
  down") into records, and keeps the API's promises about them: presses and
  releases pair up, a window losing focus first sees its keys released, a
  change of modifiers follows the key that made it.
- The guest's threads **poll**. Most records come through a bounded queue.
  Motion, wheel deltas, sizes and redraws sit in one-element mailboxes and
  are merged there while unread, so a guest that falls behind is not buried.
  Completions of requests come in order with everything else, and beside the
  queue when it is full, so they are never lost.

The producer never takes a lock. In a browser it is the page's own thread,
which may not block, and which would freeze the tab if it waited for a lock
whose holder had been terminated.

When the queue does fill up, the guest is told (`EventsLost`) at the point
where records are missing, and then sent what makes its view right again:
releases for what it believes held, and each window's present size and flags.

## Signals and stopping

WASIX delivers a signal, and ends a program it was told to end, where the
program enters the system. A program with a window may do that nowhere: one
that draws frame after frame only asks for events and frames, and one that
waits for its user sleeps inside this host.

So the two calls such a program keeps coming back to are where the system
reaches it. The hooks find the runtime's own `sched_yield` among the imports
a program is given, and the host calls it on the program's behalf: before
`wguiInstancePollEvents` answers (every hundredth of a second at most), and
when `wguiInstanceWaitEvents` returns. A handler the program installed runs
there, inside the call, and what ends the program ends it there, with the
runtime's own error. A wait returns after a quarter of a second with
`WGUIWaitReasons_Interrupted`, so that a signal does not wait for input that
never comes. Programs do nothing for any of this.

What a signal then does is the program's business, and its C library's.
wasix-libc carries out the default actions itself, and does it badly: until a
program has called `sigaction` once, any signal that arrives is answered by a
call through a null pointer, which ends the program without a word (status
27), also for the signal a resized terminal sends; some builds drop the
signal instead. A program that should end on Ctrl-C installs a handler for
SIGINT, as `examples/triangle.c` does. SDL installs its own, and the winit
backend one that ends the program ([integrations](../integrations/README.md)).

Every wait also sleeps in short slices and looks, between them, at whether
its scope was closed, and (natively) at whether its process was killed: WASIX
disables the atomics of a killed process's memory, and the host can see that.

What a guest leaves open goes when it does: an instance is destroyed with the
wasm instance that created it.

## Window systems

`src/system.rs` is the whole interface: create a window, apply a change to
it, report what happens. The headless system (`src/headless.rs`) implements
it with a model and no screen; tests feed it input through
`src/script.rs`, one step at a time, each when the guest has dealt with the
one before, which is what makes the transcripts in `tests/expected`
repeatable.

A guest that asks to redraw is told when the display is ready for its next
frame, once per refresh, on every window system, and not while its window
cannot be seen: that is what a guest without a GPU paces itself by. A
browser has animation frames for it. A page can have a request that answers
something the user did answered at once instead (`eagerRedraw`, see
`createGuiPage` in js/gui_page.js): what a guest draws in answer to the user
is then on screen about a frame sooner. Only a request that follows input
since the window's last redraw is answered so, which keeps a guest drawing
frame after frame on its own to the frames, however slow its frames. While
the user does something, a request the guest makes before it waits for
events again, after reading its last redraw (`WindowChange::Redraw`'s
`while_drawing`), keeps to the frames too. That covers a guest that reads,
draws and asks on one thread; one that waits between its frames, or on
another thread, may be answered at once after each input, until a guest can
say why it asks. Natively, winit answers a redraw request at once on
most platforms, so the native loop counts the refreshes of the window's
display itself: a request is answered at the refresh after the one the last
request was for, and dropped while the window is known to be out of sight
(the window is told to redraw when it is seen again). This follows the
display's rate, not its phase: a clock, where the window system has none to
offer. On a 120 Hz display a guest drawing every frame runs at 120 frames a
second, however much of each frame its drawing takes. The headless system
answers at once by default, and can be told to keep to a refresh rate
(`HeadlessRedraws::Paced`); on its manual clock a refresh happens when a
script says so (`{ "frames": 3 }`), which walks a guest that draws frame
after frame through them one at a time. `{ "screenshot": "name" }` writes
what a window shows to a PNG, for looking at.

### Native windows

`src/native/` is the only code that uses winit. Its loop owns the process's
main thread and starts when a guest first needs a window system; guests'
threads never touch a window. What they ask for is a command sent to the
loop, what they want to know they read from what the loop last reported, and
only creating a window and attaching a surface wait for an answer. A WebGPU
surface is attached on the loop's thread and released there, wherever the
guest drops it.

### Title bars the program draws

A program can ask for its window's title bar to be its own
(`WGUIWindowDescriptor.titleBar`), as browsers and editors draw tabs and
toolbars there:

- `Overlay`: the content area takes the whole window and the system's window
  controls stay, over it. Only macOS has it: a transparent title bar over a
  full-size content view, with the traffic lights moved to the middle of the
  title bar the program draws (`titleBarHeight`). AppKit puts them back in
  their usual place whenever it lays the title bar out (a resize, leaving
  fullscreen), so the loop moves them again then.
- `Custom`: no title bar or window controls of the system's; the program
  draws its own. Windows and Linux have it as a window without decorations,
  which the host resizes from a band along its edges; macOS as `Overlay` with
  the traffic lights hidden. Windows cannot keep its own caption buttons over
  content drawn with a GPU (it draws them under the client area), and X11
  and Wayland draw none inside it, which is why they have no `Overlay`.

A host without the style asked for gives the window the system's title bar,
and the window's state says which style it got and what part of the title bar
is the program's to draw in (empty in fullscreen, where the title bar hides).
A browser page offers neither: the tab has the title bar.

The program marks where its title bar moves the window
(`wguiWindowSetDragRegions`). The host hit-tests presses against those
rectangles on the loop's thread and starts the move itself, while the press
is the event being handled: winit's `drag_window` takes the current event, so
a request coming back from a guest's thread would be too late. A double click
there zooms, maximizes or minimizes as the user's settings say, but does
not zoom a window that is not resizable: winit would, by setting its frame.
A press finds the edge it resizes a `Custom` window by from the window's
size and state at the press, not from the cursor shown, as the window may
have changed under a pointer that has not moved since.

### The program's icon

`wguiInstanceSetIcon` takes the program's icon as square images, raw or PNG
files, and each image may name the platforms it is made for: icons differ
from one platform to the next in more than their size. A macOS icon keeps a
margin around its shape for its shadow, an iOS one fills its square and lets
the system round it, a favicon may want fewer details. The host checks every
image on the guest's thread, keeps those made for its window system's
platform (`SystemInfo::platform`), or else those made for every platform,
and decodes them from one copy of each file, which the guest's other threads
cannot change under the decoder. A window system is handed only that
(`system::Icon`), the largest image first. Wherever it shows the icon it
takes the smallest image at least as large as the icon is drawn there, or
else the largest (`Icon::image_for`), and scales it.

The icon is the instance's, for the windows it has open and those it opens
later (`WindowAttributes::icon`, then `WindowChange::Icon`):

- macOS has one icon for the process, in the Dock and the app switcher. The
  loop makes the icon given last to a window that is still open the
  application's (`NSApp.applicationIconImage`), with every image as a
  representation of one picture, and puts the application's own back when no
  window has one: a launcher that runs one program after another does not
  show the last one's.
- Windows has a small icon and a large one for each window: the title bar's,
  and the task bar's and the window switcher's. Each gets the image for the
  size Windows draws it at on the window's display (16 and 32 pixels times
  the scale factor), again when the window moves to a display of another
  scale.
- X11 has one icon for each window (`_NET_WM_ICON`), which gets the image
  for 128 pixels. winit sets none on Wayland, so it is not offered there.
- In a browser the page decides: the `icon` option of `createGuiPage` is
  handed the images as PNG pictures with the canvas, and `setPageIcon` makes
  them the tab's icon on behalf of that canvas, until no canvas wants one.
  Without that option the feature is not offered.
- The headless system keeps the icon on the window's model, and stands in for
  the platform its options name (`HeadlessOptions::platform`).

### Frames without a GPU

`wguiWindowPresentPixels` copies the guest's frame out of its memory during
the call and hands it to the window system as a `PixelFrame`. Frames weigh
megabytes and a guest can draw faster than anything shows, so no window
system queues them: each keeps a slot for the newest one and is told once
that there is something in it. A frame that replaces one nobody saw loses its
list of what changed, since that was said of a frame that was never shown.

Natively the loop shows frames through softbuffer and keeps the one on show,
to stretch it over the window when that changes size, until the guest has
drawn one for the new size. In a browser the canvas takes the frame's size
and the page's layout does the stretching. The headless system keeps the
frame for whoever asks.

### Views: a user interface that may not wait

`src/views.rs` is for embedders whose windows are views of their own
interface: the canvases of a page, the views of an app. That interface has a
thread that must never wait, for a lock or for a guest. So what guests ask
for is queued (`ViewCommand`), the embedder is told that the queue has
something in it, and its interface's thread takes the commands when it gets
to it. Reporting through a sink never waits either. A guest's thread waits
for the interface exactly once per window, while the window is made, with a
deadline and a way out if the guest is stopped meanwhile.

### The browser

In a browser the embedder is itself WebAssembly: the Wasmer SDK's module,
with one memory shared by the page's thread and every worker a guest runs
in. That is what makes input cheap. `src/web.rs` runs on the page's thread:
`js/gui_page.js` listens to the DOM and calls it, it tells the window's sink,
and the sink writes a record into the queue and wakes the guest's worker
where it sleeps in `wguiInstanceWaitEvents`. No message is posted, and the
guest needs no JSPI to wait.

That sleep blocks the worker, though, and whatever else lives in it waits
too: WebGPU's callbacks, messages, timers. In a browser one wait blocks it
for 8 ms at most rather than a quarter of a second, then returns
`WGUIWaitReasons_Interrupted`. That bounds a single call; it does not let the
worker's event loop run, which happens only when the program suspends
between its waits, or returns to its embedder. A program that waits again at
once keeps the worker. An embedder that enters its guests
through JSPI (`Function::call_async`) can let the wait suspend instead
(`GuiCtxBuilder::suspending_waits`, `host::web_wait`). The guest then waits
with `Atomics.waitAsync` on a counter that every notification of its
instance bumps: input still reaches it without a message, and work done in
the worker ends the wait through `GuiCtx::event_waker`.

The other direction does post a message, a single empty one: a worker that
queued a command asks the page to look at the queue. The page half carries
the command out on the canvas and reports what came of it the same way as
everything else.

The page's thread is not allowed to block: a futex wait there traps, and
takes the embedder down. Three things keep it from ever trying. Everything a
sink touches is lock-free; the few locks the two sides share are taken by
trying again instead of waiting in this build (`host::lock`), and are never
held across anything that waits; and the one wait there is, for a window to
be made, is the guest's.

Since every import of a guest would otherwise bring a typed wrapper of its
own into the SDK's module, the browser build registers one function for all
of them and tells them apart by number (`abi::api::call`), and the crate is
built for size there.

//! The browser's window system: canvases of the page as windows.
//!
//! [`views`](crate::views) is the half that guests talk to, on their
//! workers' threads. This is the half on the page's thread. It takes what
//! guests ask for out of the queue and has `js/gui_page.js` carry it out on
//! the DOM, and it tells the windows' sinks what that file reports of the
//! user. The two halves share the embedder's memory, so what the user does
//! reaches a guest that waits for it without a message in between: the page
//! writes a record and wakes the worker.
//!
//! Everything here runs on the thread [`open`] was called on, which has to
//! be the one that owns the page's canvases. That thread may not wait, and
//! nothing here does.

use std::cell::RefCell;

use js_sys::{Float64Array, Int32Array, Object, Reflect, Uint8Array, Uint8ClampedArray};
use wasm_bindgen::prelude::*;

use crate::abi::AbiEnum;
use crate::system::*;
use crate::views::{ViewCommand, ViewId, ViewOptions, ViewSystem};

#[wasm_bindgen(module = "/js/gui_page.js")]
extern "C" {
    /// The page object `js/gui_page.js` makes: its methods are what guests
    /// ask of the DOM.
    type GuiPage;

    #[wasm_bindgen(js_name = createGuiPage, catch)]
    fn create_gui_page(core: &JsValue, options: &JsValue) -> Result<GuiPage, JsValue>;

    #[wasm_bindgen(method, js_name = createWindow)]
    fn create_window(this: &GuiPage, view: f64, request: &JsValue) -> JsValue;
    #[wasm_bindgen(method, js_name = destroyWindow)]
    fn destroy_window(this: &GuiPage, view: f64);
    #[wasm_bindgen(method, js_name = setTitle)]
    fn set_title(this: &GuiPage, view: f64, title: &str);
    #[wasm_bindgen(method, js_name = setSize)]
    fn set_size(this: &GuiPage, view: f64, width: u32, height: u32);
    #[wasm_bindgen(method, js_name = setCursorShape)]
    fn set_cursor_shape(this: &GuiPage, view: f64, shape: u32);
    #[wasm_bindgen(method, js_name = setCustomCursor)]
    fn set_custom_cursor(
        this: &GuiPage,
        view: f64,
        rgba: &JsValue,
        width: u32,
        height: u32,
        hotspot_x: u32,
        hotspot_y: u32,
        scale: f64,
    );
    #[wasm_bindgen(method, js_name = setCursorVisible)]
    fn set_cursor_visible(this: &GuiPage, view: f64, visible: bool);
    #[wasm_bindgen(method, js_name = setCursorGrab)]
    fn set_cursor_grab(this: &GuiPage, view: f64, grab: u32, request: u32);
    #[wasm_bindgen(method, js_name = setFullscreen)]
    fn set_fullscreen(this: &GuiPage, view: f64, on: bool, request: u32, wait: bool);
    #[wasm_bindgen(method, js_name = setTextInput)]
    #[allow(clippy::too_many_arguments)]
    fn set_text_input(
        this: &GuiPage,
        view: f64,
        on: bool,
        purpose: u32,
        hints: u32,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        keyboard: u32,
    );
    #[wasm_bindgen(method, js_name = resetTextInput)]
    fn reset_text_input(this: &GuiPage, view: f64);
    #[wasm_bindgen(method)]
    fn focus(this: &GuiPage, view: f64);
    #[wasm_bindgen(method, js_name = requestRedraw)]
    fn request_redraw(this: &GuiPage, view: f64, while_drawing: bool);
    #[wasm_bindgen(method, js_name = setDefaultActions)]
    fn set_default_actions(this: &GuiPage, view: f64, actions: u32);
    #[wasm_bindgen(method, js_name = presentPixels)]
    fn present_pixels(
        this: &GuiPage,
        view: f64,
        rgba: &Uint8ClampedArray,
        width: u32,
        height: u32,
        damage: &Int32Array,
    );
    #[wasm_bindgen(method, js_name = writeClipboard)]
    fn write_clipboard(this: &GuiPage, request: u32, text: &str);
    #[wasm_bindgen(method, js_name = readClipboard)]
    fn read_clipboard(this: &GuiPage, request: u32);
    #[wasm_bindgen(method)]
    fn start(this: &GuiPage);
    #[wasm_bindgen(method)]
    fn dispose(this: &GuiPage);
}

// Importing from the generated tables makes wasm-bindgen ship that file next
// to the page half, which imports it; the value doubles as a check that both
// were generated from the same description of the API.
#[wasm_bindgen(module = "/js/gui_abi.gen.js")]
extern "C" {
    #[wasm_bindgen(thread_local_v2, js_name = IMPORT_MODULE)]
    static JS_IMPORT_MODULE: JsValue;
}

#[wasm_bindgen(inline_js = "
export function wasmer_gui_notify(scope) {
  // On the page's own thread the queue is looked at as soon as what is
  // running now is done. From a worker the page has to be told, and the
  // worker pool of the Wasmer SDK offers every worker's message to
  // `__wasmerHandleGuiRpc`.
  const serve = globalThis.__wasmerGuiServe;
  if (typeof serve === 'function') queueMicrotask(() => serve(scope));
  else globalThis.postMessage({ type: 'wasmer-gui', scope });
}
export function wasmer_gui_install(serve) {
  globalThis.__wasmerGuiServe = serve;
  globalThis.__wasmerHandleGuiRpc = (message) => {
    if (typeof message !== 'object' || message === null || message.type !== 'wasmer-gui') {
      return false;
    }
    serve(message.scope);
    return true;
  };
}
export function wasmer_gui_shortcut_is_command() {
  const platform = globalThis.navigator?.userAgentData?.platform ?? globalThis.navigator?.platform ?? '';
  return /mac|iphone|ipad|ipod/i.test(platform);
}
export function wasmer_gui_input_devices() {
  const any = (query) => globalThis.matchMedia?.(query)?.matches === true;
  let devices = 1;
  if (any('(any-pointer: fine)')) devices |= 2;
  if ((globalThis.navigator?.maxTouchPoints ?? 0) > 0) devices |= 4;
  return devices;
}
")]
extern "C" {
    #[wasm_bindgen(js_name = wasmer_gui_notify)]
    fn notify(scope: u32);
    #[wasm_bindgen(js_name = wasmer_gui_install)]
    fn install(serve: &JsValue);
    #[wasm_bindgen(js_name = wasmer_gui_shortcut_is_command)]
    fn shortcut_is_command() -> bool;
    #[wasm_bindgen(js_name = wasmer_gui_input_devices)]
    fn input_devices_of_the_page() -> u32;
}

/// The one display a page knows of: the screen its window is on.
const DISPLAY: u64 = 1;

/// The flags and positions of `core.key` and `core.pointer` in
/// `js/gui_page.js`.
const KEY_PRESSED: u32 = 1;
const KEY_COMPOSING: u32 = 2;
const POINTER_VALUES: usize = 17;

/// A request the page half is carrying out, and will name by a number.
enum Waiting {
    Request(PendingRequest),
    ClipboardRead {
        mime_type: String,
        request: PendingRequest,
    },
}

/// A few things by number. There are never many of them, and three hash
/// maps would be a good part of this module's weight in the embedder's wasm.
struct Few<T>(Vec<(u64, T)>);

impl<T> Few<T> {
    const fn new() -> Self {
        Self(Vec::new())
    }

    fn get_mut(&mut self, key: u64) -> Option<&mut T> {
        self.0
            .iter_mut()
            .find_map(|(known, value)| (*known == key).then_some(value))
    }

    fn insert(&mut self, key: u64, value: T) {
        self.remove(key);
        self.0.push((key, value));
    }

    fn remove(&mut self, key: u64) -> Option<T> {
        let at = self.0.iter().position(|(known, _)| *known == key)?;
        Some(self.0.swap_remove(at).1)
    }

    fn values_mut(&mut self) -> impl Iterator<Item = &mut T> {
        self.0.iter_mut().map(|(_, value)| value)
    }
}

/// The page's half of one sandbox's window system.
struct Page {
    views: ViewSystem,
    /// The page object of `js/gui_page.js`.
    js: JsValue,
    sinks: Few<Box<WindowSink>>,
    instances: Vec<InstanceSink>,
    waiting: Few<Waiting>,
    next_request: u32,
    /// What the page half calls. Kept for as long as it may.
    _core: Vec<Box<dyn std::any::Any>>,
}

/// What the page calls to have a sandbox's queue looked at.
type Serve = Closure<dyn Fn(u32)>;

thread_local! {
    static PAGES: RefCell<Few<Page>> = const { RefCell::new(Few::new()) };
    static SERVE: RefCell<Option<Serve>> = const { RefCell::new(None) };
}

/// Looks at one page's state. `f` must not call into JavaScript: the page
/// half may call straight back, and would find the state taken.
fn with_page<R>(scope: u32, f: impl FnOnce(&mut Page) -> R) -> Option<R> {
    PAGES.with(|pages| pages.borrow_mut().get_mut(u64::from(scope)).map(f))
}

fn page_of(scope: u32) -> Option<GuiPage> {
    with_page(scope, |page| page.js.clone()).map(JsCast::unchecked_into)
}

fn with_sink(scope: u32, view: f64, f: impl FnOnce(&mut WindowSink)) {
    with_page(scope, |page| {
        if let Some(sink) = page.sinks.get_mut(view as ViewId) {
            f(sink);
        }
    });
}

fn instances(scope: u32) -> Vec<InstanceSink> {
    with_page(scope, |page| {
        page.instances.retain(|instance| !instance.is_closed());
        page.instances.clone()
    })
    .unwrap_or_default()
}

/// What the page can do, as guests are told.
fn info() -> SystemInfo {
    use FeatureName::*;
    SystemInfo {
        host_kind: HostKind::Browser,
        // No windows of the guest's own, no positions, no window states: a
        // canvas is where the page put it. No cursor confinement or warping.
        features: vec![
            Windows,
            FullscreenBorderless,
            Displays,
            TextInput,
            ScreenKeyboard,
            Touch,
            Pen,
            RawInput,
            CursorShapes,
            CustomCursors,
            CursorLock,
            ClipboardText,
            Theme,
            PixelPresent,
        ],
        shortcut_modifier: if shortcut_is_command() {
            modifiers::SUPER
        } else {
            modifiers::CONTROL
        },
        input_devices: input_devices_of_the_page(),
        theme: crate::system::Theme::Unknown,
        max_cursor_size: 128,
    }
}

/// Makes the window system of one sandbox, whose windows are canvases of
/// the page. `scope` names the sandbox among those of this page; `options`
/// is handed to `createGuiPage` of `js/gui_page.js`, which says what it
/// may hold.
///
/// Call it on the thread that owns the canvases, and [`close`] there too.
pub fn open(scope: u32, options: &JsValue) -> Result<ViewSystem, JsValue> {
    let tables = JS_IMPORT_MODULE.with(JsValue::as_string);
    if tables.as_deref() != Some(crate::abi::IMPORT_MODULE) {
        return Err(JsValue::from_str(&format!(
            "the JavaScript GUI tables are for {tables:?}, this build speaks `{}`",
            crate::abi::IMPORT_MODULE
        )));
    }
    if with_page(scope, |_| ()).is_some() {
        return Err(JsValue::from_str("this sandbox already has a GUI"));
    }
    SERVE.with(|installed| {
        let mut installed = installed.borrow_mut();
        if installed.is_none() {
            let serve = Closure::<dyn Fn(u32)>::new(serve);
            install(serve.as_ref());
            *installed = Some(serve);
        }
    });

    let views = ViewSystem::new(
        ViewOptions {
            info: info(),
            ..ViewOptions::default()
        },
        move || notify(scope),
    );
    let (core, closures) = core(scope);
    let js = create_gui_page(&core, options)?;
    let started: GuiPage = js.clone().unchecked_into();
    PAGES.with(|pages| {
        pages.borrow_mut().insert(
            u64::from(scope),
            Page {
                views: views.clone(),
                js: js.into(),
                sinks: Few::new(),
                instances: Vec::new(),
                waiting: Few::new(),
                next_request: 1,
                _core: closures,
            },
        );
    });
    // Only now is there somebody for the page half to report to: it tells of
    // the display and the theme straight away.
    started.start();
    Ok(views)
}

/// The sandbox is gone: its canvases go back to the page, guests that wait
/// for the page are answered, and nothing more is asked of it.
pub fn close(scope: u32) {
    let Some(page) = PAGES.with(|pages| pages.borrow_mut().remove(u64::from(scope))) else {
        return;
    };
    page.views.shut_down();
    let js: GuiPage = page.js.clone().unchecked_into();
    // Dropped only after the page half has let go of every canvas: it may
    // still report what that does to them.
    js.dispose();
    drop(page);
}

/// Carries out what guests have asked for since the last look.
fn serve(scope: u32) {
    while let Some(command) = with_page(scope, |page| page.views.next_command()).flatten() {
        carry_out(scope, command);
    }
}

fn hold(scope: u32, waiting: Waiting) -> u32 {
    with_page(scope, |page| {
        let id = page.next_request;
        page.next_request = page.next_request.wrapping_add(1).max(1);
        page.waiting.insert(u64::from(id), waiting);
        id
    })
    .unwrap_or(0)
}

fn field(object: &JsValue, name: &str) -> JsValue {
    Reflect::get(object, &JsValue::from_str(name)).unwrap_or(JsValue::UNDEFINED)
}

fn number(object: &JsValue, name: &str) -> f64 {
    field(object, name).as_f64().unwrap_or(0.0)
}

fn carry_out(scope: u32, command: ViewCommand) {
    let Some(js) = page_of(scope) else {
        // Dropped: whatever the command carried is answered by that.
        return;
    };
    match command {
        ViewCommand::Attach(instance) => {
            with_page(scope, |page| {
                page.instances.retain(|instance| !instance.is_closed());
                page.instances.push(instance);
            });
        }
        ViewCommand::Create {
            view,
            attributes,
            mut sink,
            answer,
        } => {
            let request = Object::new();
            let set = |name: &str, value: JsValue| {
                let _ = Reflect::set(&request, &JsValue::from_str(name), &value);
            };
            set("target", JsValue::from_str(&attributes.target));
            set("title", JsValue::from_str(&attributes.title));
            let (width, height) = attributes.size.unwrap_or((0, 0));
            set("width", JsValue::from(width));
            set("height", JsValue::from(height));
            set("logical", JsValue::from(attributes.logical_units));
            let made = js.create_window(view as f64, &request);

            let status = Status::from_u32(number(&made, "status") as u32).unwrap_or(Status::Error);
            if status != Status::Success {
                answer.refused(match status {
                    Status::Unimplemented => Status::Error,
                    status => status,
                });
                return;
            }
            let selector = field(&made, "selector").as_string().unwrap_or_default();
            let (width, height) = (number(&made, "width"), number(&made, "height"));
            let scale = number(&made, "scale");
            sink.display_changed(DISPLAY);
            sink.theme_changed(
                crate::system::Theme::from_u32(number(&made, "theme") as u32)
                    .unwrap_or(crate::system::Theme::Unknown),
            );
            sink.flags(window_flags::REQUESTABLE, window_flags::VISIBLE);
            if field(&made, "occluded").is_truthy() {
                sink.flags(window_flags::OCCLUDED, window_flags::OCCLUDED);
            }
            // The size is what tells the guest that the window is there.
            sink.resized(
                (width as u32).max(1),
                (height as u32).max(1),
                if scale > 0.0 { scale } else { 1.0 },
            );
            if field(&made, "focused").is_truthy() {
                sink.focused(true);
            }
            with_page(scope, |page| page.sinks.insert(view, sink));
            if answer.created(selector).is_err() {
                // The guest stopped waiting: the canvas is the page's again.
                with_page(scope, |page| page.sinks.remove(view));
                js.destroy_window(view as f64);
            }
        }
        ViewCommand::Change { view, change } => change_view(scope, &js, view, change),
        ViewCommand::Frame { view, frame } => {
            let Some(frame) = frame.take() else {
                return;
            };
            // A canvas takes red, green, blue, alpha, in memory that is the
            // page's own: the frame is copied out of the shared one here.
            let rgba = Uint8ClampedArray::from(&frame.to_rgba()[..]);
            let damage: Vec<i32> = frame
                .damage
                .iter()
                .flat_map(|rect| [rect.x, rect.y, rect.width as i32, rect.height as i32])
                .collect();
            js.present_pixels(
                view as f64,
                &rgba,
                frame.width,
                frame.height,
                &Int32Array::from(&damage[..]),
            );
        }
        ViewCommand::Destroy { view } => {
            let sink = with_page(scope, |page| page.sinks.remove(view));
            js.destroy_window(view as f64);
            drop(sink);
        }
        ViewCommand::ClipboardRequest {
            selection,
            mime_type,
            request,
        } => {
            if selection != ClipboardSelection::Clipboard {
                return request.complete(Status::Unsupported);
            }
            let id = hold(scope, Waiting::ClipboardRead { mime_type, request });
            js.read_clipboard(id);
        }
        ViewCommand::ClipboardWrite {
            selection,
            items,
            request,
        } => {
            let text = items
                .iter()
                .find(|item| item.mime_type == "text/plain")
                .and_then(|item| std::str::from_utf8(&item.data).ok());
            match (selection, text) {
                (ClipboardSelection::Clipboard, Some(text)) => {
                    let id = hold(scope, Waiting::Request(request));
                    js.write_clipboard(id, text);
                }
                _ => request.complete(Status::Unsupported),
            }
        }
    }
}

fn change_view(scope: u32, js: &GuiPage, view: ViewId, change: WindowChange) {
    let id = view as f64;
    match change {
        WindowChange::Title(title) => js.set_title(id, &title),
        WindowChange::Size { width, height } => js.set_size(id, width, height),
        WindowChange::Fullscreen { options, request } => {
            let wait = options.as_ref().is_none_or(|options| !options.no_deferral);
            let on = options.is_some();
            let request = hold(scope, Waiting::Request(request));
            js.set_fullscreen(id, on, request, wait);
        }
        WindowChange::Focus => js.focus(id),
        WindowChange::Redraw { while_drawing } => js.request_redraw(id, while_drawing),
        WindowChange::CursorShape(shape) => js.set_cursor_shape(id, shape.as_u32()),
        WindowChange::CustomCursor(None) => {
            js.set_custom_cursor(id, &JsValue::NULL, 0, 0, 0, 0, 1.0);
        }
        WindowChange::CustomCursor(Some(image)) => {
            let rgba = Uint8Array::from(&image.rgba[..]);
            js.set_custom_cursor(
                id,
                &rgba,
                image.width,
                image.height,
                image.hotspot_x,
                image.hotspot_y,
                image.scale_factor,
            );
        }
        WindowChange::CursorVisible(visible) => js.set_cursor_visible(id, visible),
        WindowChange::CursorGrab { grab, request } => {
            let request = hold(scope, Waiting::Request(request));
            js.set_cursor_grab(id, grab.as_u32(), request);
        }
        WindowChange::TextInput(Some(input)) => {
            let area = input.cursor_area;
            js.set_text_input(
                id,
                true,
                input.purpose.as_u32(),
                input.hints,
                f64::from(area.x),
                f64::from(area.y),
                f64::from(area.width),
                f64::from(area.height),
                input.screen_keyboard.as_u32(),
            );
        }
        WindowChange::TextInput(None) => {
            js.set_text_input(id, false, 0, 0, 0.0, 0.0, 0.0, 0.0, 0);
        }
        WindowChange::ResetTextInput => js.reset_text_input(id),
        WindowChange::DefaultActions(actions) => js.set_default_actions(id, actions),
        // A canvas is where the page put it, as big as its layout makes it
        // and as visible as the page is: none of these is the guest's.
        WindowChange::SizeLimits { .. }
        | WindowChange::Position { .. }
        | WindowChange::Flags { .. }
        | WindowChange::Attention(_)
        | WindowChange::CursorPosition { .. } => {}
        // Frames come by themselves (`ViewCommand::Frame`).
        WindowChange::Pixels(_) => {}
    }
}

/// Makes the object whose functions the page half calls, and what keeps them
/// alive.
fn core(scope: u32) -> (JsValue, Vec<Box<dyn std::any::Any>>) {
    let core = Object::new();
    let mut kept: Vec<Box<dyn std::any::Any>> = Vec::new();
    macro_rules! on {
        ($name:literal, $closure:expr) => {{
            let closure = $closure;
            let _ = Reflect::set(&core, &JsValue::from_str($name), closure.as_ref());
            kept.push(Box::new(closure));
        }};
    }

    on!(
        "resized",
        Closure::<dyn Fn(f64, f64, f64, f64)>::new(
            move |view, width: f64, height: f64, scale: f64| {
                with_sink(scope, view, |sink| {
                    sink.resized(
                        (width as u32).max(1),
                        (height as u32).max(1),
                        if scale > 0.0 { scale } else { 1.0 },
                    );
                });
            }
        )
    );
    on!(
        "focused",
        Closure::<dyn Fn(f64, bool)>::new(move |view, focused| {
            with_sink(scope, view, |sink| sink.focused(focused));
        })
    );
    on!(
        "flags",
        Closure::<dyn Fn(f64, f64, f64)>::new(move |view, mask: f64, values: f64| {
            with_sink(scope, view, |sink| sink.flags(mask as u64, values as u64));
        })
    );
    on!(
        "fullscreen",
        Closure::<dyn Fn(f64, bool)>::new(move |view, on| {
            with_sink(scope, view, |sink| {
                sink.fullscreen(if on {
                    FullscreenMode::Borderless
                } else {
                    FullscreenMode::None
                });
            });
        })
    );
    on!(
        "requestDone",
        Closure::<dyn Fn(u32, u32)>::new(move |request, status| {
            let waiting =
                with_page(scope, |page| page.waiting.remove(u64::from(request))).flatten();
            if let Some(Waiting::Request(request) | Waiting::ClipboardRead { request, .. }) =
                waiting
            {
                request.complete(Status::from_u32(status).unwrap_or(Status::Error));
            }
        })
    );
    on!(
        "grabChanged",
        Closure::<dyn Fn(f64, u32, u32)>::new(move |view, grab, reason| {
            let grab = CursorGrab::from_u32(grab).unwrap_or(CursorGrab::None);
            let reason = CursorGrabReason::from_u32(reason).unwrap_or(CursorGrabReason::Host);
            with_sink(scope, view, |sink| sink.cursor_grab_changed(grab, reason));
        })
    );
    on!(
        "key",
        Closure::<dyn Fn(f64, u32, u32, u32, u32, u32, String, String)>::new(
            move |view, flags: u32, physical, key, location, modifiers, key_string, text| {
                let input = KeyInput {
                    physical_key: PhysicalKey::from_u32(physical).unwrap_or_default(),
                    key,
                    key_string,
                    location: KeyLocation::from_u32(location).unwrap_or_default(),
                    pressed: flags & KEY_PRESSED != 0,
                    composing: flags & KEY_COMPOSING != 0,
                    text,
                    modifiers: Some(modifiers),
                    ..KeyInput::default()
                };
                with_sink(scope, view, |sink| sink.key(input));
            }
        )
    );
    on!(
        "modifiers",
        Closure::<dyn Fn(f64, u32)>::new(move |view, modifiers| {
            with_sink(scope, view, |sink| sink.modifiers(modifiers));
        })
    );
    on!(
        "pointer",
        Closure::<dyn Fn(f64, Float64Array)>::new(move |view, values: Float64Array| {
            if values.length() as usize != POINTER_VALUES {
                return;
            }
            let mut v = [0.0_f64; POINTER_VALUES];
            values.copy_to(&mut v);
            let kind = PointerKind::from_u32(v[1] as u32).unwrap_or(PointerKind::Mouse);
            let measures = kind != PointerKind::Mouse;
            let input = PointerInput {
                kind,
                source: v[2] as i64 as u64,
                phase: match v[0] as u32 {
                    0 => PointerPhase::Enter,
                    1 => PointerPhase::Leave,
                    3 => PointerPhase::Down,
                    4 => PointerPhase::Up,
                    5 => PointerPhase::Cancel,
                    _ => PointerPhase::Motion,
                },
                button: v[3] as u32,
                x: v[4],
                y: v[5],
                delta: (v[6] != 0.0).then_some((v[7], v[8])),
                pressure: (v[9] >= 0.0).then_some(v[9] as f32),
                tangential_pressure: v[16] as f32,
                tilt: measures.then_some((v[10] as f32, v[11] as f32)),
                twist: v[12] as f32,
                contact_size: (v[13] as f32, v[14] as f32),
                tool: PointerTool::from_u32(v[15] as u32).unwrap_or(PointerTool::Unknown),
                ..PointerInput::default()
            };
            with_sink(scope, view, |sink| sink.pointer(input));
        })
    );
    on!(
        "wheel",
        Closure::<dyn Fn(f64, u32, u32, f64, f64, f64, f64)>::new(
            move |view, unit, phase, delta_x, delta_y, x, y| {
                let input = WheelInput {
                    unit: WheelUnit::from_u32(unit).unwrap_or(WheelUnit::Pixel),
                    phase: WheelPhase::from_u32(phase).unwrap_or(WheelPhase::None),
                    flags: 0,
                    delta_x,
                    delta_y,
                    x,
                    y,
                    device: 0,
                };
                with_sink(scope, view, |sink| sink.wheel(input));
            }
        )
    );
    on!(
        "rawMotion",
        Closure::<dyn Fn(f64, f64)>::new(move |delta_x, delta_y| {
            for instance in instances(scope) {
                instance.raw_motion(0, delta_x, delta_y);
            }
        })
    );
    on!(
        "textState",
        Closure::<dyn Fn(f64, bool, bool)>::new(move |view, active, keyboard_visible| {
            with_sink(scope, view, |sink| {
                sink.text_input_state(active, keyboard_visible, Rect::default());
            });
        })
    );
    on!(
        "composition",
        Closure::<dyn Fn(f64, String, f64, f64)>::new(
            move |view, text: String, start: f64, end: f64| {
                let selection = (start >= 0.0 && end >= 0.0).then_some((start as u32, end as u32));
                with_sink(scope, view, |sink| sink.text_composition(&text, selection));
            }
        )
    );
    on!(
        "commit",
        Closure::<dyn Fn(f64, String)>::new(move |view, text: String| {
            with_sink(scope, view, |sink| sink.text_commit(&text));
        })
    );
    on!(
        "redraw",
        Closure::<dyn Fn(f64)>::new(move |view| {
            with_sink(scope, view, WindowSink::redraw_requested);
        })
    );
    on!(
        "theme",
        Closure::<dyn Fn(u32)>::new(move |theme| {
            let theme =
                crate::system::Theme::from_u32(theme).unwrap_or(crate::system::Theme::Unknown);
            with_page(scope, |page| {
                page.views.set_theme(theme);
                for sink in page.sinks.values_mut() {
                    sink.theme_changed(theme);
                }
            });
            for instance in instances(scope) {
                instance.theme_changed(theme);
            }
        })
    );
    on!(
        "display",
        Closure::<dyn Fn(f64, f64, f64, f64, f64, f64, f64)>::new(
            move |width: f64,
                  height: f64,
                  left: f64,
                  top: f64,
                  work_width: f64,
                  work_height: f64,
                  scale: f64| {
                let display = DisplayData {
                    id: DISPLAY,
                    name: "Screen".to_owned(),
                    x: 0,
                    y: 0,
                    width: width as u32,
                    height: height as u32,
                    work_area: (
                        left as i32,
                        top as i32,
                        work_width as u32,
                        work_height as u32,
                    ),
                    scale_factor: if scale > 0.0 { scale } else { 1.0 },
                    refresh_rate: 0,
                    primary: true,
                };
                let changed = with_page(scope, |page| {
                    let before = page.views.displays();
                    let changed = before.first() != Some(&display);
                    if changed {
                        page.views.set_displays(vec![display]);
                    }
                    changed && !before.is_empty()
                })
                .unwrap_or(false);
                if changed {
                    for instance in instances(scope) {
                        instance.displays_changed();
                    }
                }
            }
        )
    );
    on!(
        "clipboardWritten",
        Closure::<dyn Fn(u32, bool)>::new(move |request, written: bool| {
            let waiting =
                with_page(scope, |page| page.waiting.remove(u64::from(request))).flatten();
            let Some(Waiting::Request(request)) = waiting else {
                return;
            };
            request.complete(if written {
                Status::Success
            } else {
                Status::Denied
            });
            if written {
                let state = with_page(scope, |page| {
                    page.views.clipboard_state(ClipboardSelection::Clipboard)
                });
                if let Some(state) = state {
                    for instance in instances(scope) {
                        instance.clipboard_changed(ClipboardSelection::Clipboard, state);
                    }
                }
            }
        })
    );
    on!(
        "clipboardFetched",
        Closure::<dyn Fn(u32, Option<String>, bool)>::new(
            move |request, text: Option<String>, changed: bool| {
                let waiting =
                    with_page(scope, |page| page.waiting.remove(u64::from(request))).flatten();
                let found = text.is_some();
                let state = with_page(scope, |page| {
                    let items = text
                        .map(|text| ClipboardData {
                            mime_type: "text/plain".to_owned(),
                            data: text.into_bytes(),
                        })
                        .into_iter()
                        .collect();
                    page.views
                        .clipboard_fetched(ClipboardSelection::Clipboard, items, changed)
                });
                if changed && let Some(state) = state {
                    for instance in instances(scope) {
                        instance.clipboard_changed(ClipboardSelection::Clipboard, state);
                    }
                }
                if let Some(Waiting::ClipboardRead { mime_type, request }) = waiting {
                    request.complete(if found && mime_type == "text/plain" {
                        Status::Success
                    } else {
                        Status::NotFound
                    });
                }
            }
        )
    );
    (core.into(), kept)
}

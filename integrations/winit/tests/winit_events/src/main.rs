//! winit on WASIX: opens a window, draws it with softbuffer whenever winit
//! says to, and prints every event it gets, one line each, until the window
//! is asked to close. It sleeps in between, as winit's programs do unless
//! they ask to be polled.
//!
//! The function keys make it use the rest of winit's window API, so that a
//! script of input can walk it through all of it:
//!
//! ```text
//! F1  fullscreen on and off       F2  lock the cursor      F3  let it go
//! F4  input methods on and off    F7  ask for 400 by 300   F8  maximize and restore
//! F9  quit                        F10 the text cursor, then one of the program's own
//! ```
//!
//! Arguments: `--target <name>` (the target the window adopts),
//! `--quit-after <milliseconds>` (another thread ends the wait),
//! `--animate <milliseconds>` (draw frame after frame for that long, and say
//! how many there were), `--interrupt-after <milliseconds>` (the program is
//! sent SIGINT, as by Ctrl-C in its terminal).

use std::num::NonZeroU32;
use std::rc::Rc;
use std::time::{Duration, Instant};

use softbuffer::{Context, Surface};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalPosition, LogicalSize, PhysicalSize};
use winit::event::{
    DeviceEvent, DeviceId, ElementState, Ime, KeyEvent, MouseScrollDelta, StartCause, WindowEvent,
};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, OwnedDisplayHandle};
use winit::keyboard::{Key, KeyCode, PhysicalKey};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
use winit::platform::wasix::{WindowAttributesExtWasix, WindowExtWasix};
use winit::window::{
    CursorGrabMode, CursorIcon, CustomCursor, Fullscreen, Window, WindowId,
};

#[derive(Debug)]
enum User {
    Quit,
}

struct Options {
    target: Option<String>,
    quit_after: Option<u64>,
    animate: Option<u64>,
    interrupt_after: Option<u64>,
}

struct App {
    options: Options,
    context: Context<OwnedDisplayHandle>,
    window: Option<Rc<Window>>,
    surface: Option<Surface<OwnedDisplayHandle, Rc<Window>>>,
    frames: u32,
    ime: bool,
    maximized: bool,
    cursors: u32,
    /// When the frames of `--animate` started, and how many there were.
    animating: Option<(Instant, u32)>,
}

fn key_name(key: &Key) -> String {
    match key {
        Key::Named(named) => format!("{named:?}"),
        Key::Character(text) => format!("{:?}", text.as_str()),
        Key::Dead(adds) => format!("Dead({adds:?})"),
        Key::Unidentified(_) => "Unidentified".to_owned(),
    }
}

impl App {
    fn window(&self) -> &Rc<Window> {
        self.window.as_ref().expect("a window")
    }

    /// What the window shows: a colour that says how many frames there
    /// were.
    fn draw(&mut self) {
        let size = self.window().inner_size();
        let (Some(width), Some(height)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
        else {
            return;
        };
        let surface = self.surface.as_mut().expect("a surface");
        surface.resize(width, height).expect("a frame of the window's size");
        let mut buffer = surface.buffer_mut().expect("a frame to draw in");
        let shade = 40 + (self.frames * 16) % 200;
        buffer.fill((shade << 16) | (60 << 8) | 90);
        buffer.present().expect("the frame shown");
        self.frames += 1;
    }

    fn on_key(&mut self, event_loop: &ActiveEventLoop, code: KeyCode) {
        let window = self.window().clone();
        match code {
            KeyCode::F1 => {
                let on = window.fullscreen().is_none();
                println!("fullscreen {}", if on { "on" } else { "off" });
                window.set_fullscreen(on.then_some(Fullscreen::Borderless(None)));
            },
            KeyCode::F2 => {
                println!("lock: {:?}", window.set_cursor_grab(CursorGrabMode::Locked).map_err(|e| e.to_string()));
            },
            KeyCode::F3 => {
                println!("release: {:?}", window.set_cursor_grab(CursorGrabMode::None).map_err(|e| e.to_string()));
            },
            KeyCode::F4 => {
                self.ime = !self.ime;
                println!("input methods {}", if self.ime { "on" } else { "off" });
                window.set_ime_cursor_area(LogicalPosition::new(10, 20), LogicalSize::new(2, 16));
                window.set_ime_allowed(self.ime);
            },
            KeyCode::F7 => {
                println!("size 400x300: {:?}", window.request_inner_size(LogicalSize::new(400, 300)));
            },
            KeyCode::F8 => {
                self.maximized = !self.maximized;
                println!("{}", if self.maximized { "maximize" } else { "restore" });
                window.set_maximized(self.maximized);
            },
            KeyCode::F9 => event_loop.exit(),
            KeyCode::F10 => {
                self.cursors += 1;
                if self.cursors % 2 == 1 {
                    window.set_cursor(CursorIcon::Text);
                    println!("cursor: text");
                } else {
                    // Sixteen by sixteen of red, pointing with (3, 5).
                    let rgba = [255, 0, 0, 255].repeat(16 * 16);
                    let source = CustomCursor::from_rgba(rgba, 16, 16, 3, 5).expect("an image");
                    window.set_cursor(event_loop.create_custom_cursor(source));
                    println!("cursor: the program's own");
                }
            },
            _ => {},
        }
    }
}

impl ApplicationHandler<User> for App {
    fn new_events(&mut self, event_loop: &ActiveEventLoop, _cause: StartCause) {
        // A loop that never waits for its user: every turn asks for the next
        // redraw, which comes when the display is ready for it.
        let Some((started, frames)) = self.animating else {
            return;
        };
        let limit = Duration::from_millis(self.options.animate.unwrap_or(0));
        if started.elapsed() >= limit {
            println!("animated {frames} frames in {} ms", limit.as_millis());
            event_loop.exit();
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        println!("resumed");
        for monitor in event_loop.available_monitors() {
            let size = monitor.size();
            println!(
                "monitor {:?} {}x{} scale {} at {} mHz{}",
                monitor.name().unwrap_or_default(),
                size.width,
                size.height,
                monitor.scale_factor(),
                monitor.refresh_rate_millihertz().unwrap_or(0),
                if event_loop.primary_monitor().as_ref() == Some(&monitor) { ", primary" } else { "" },
            );
        }
        let mut attributes = Window::default_attributes()
            .with_title("winit on Wasmer")
            .with_inner_size(LogicalSize::new(640, 480));
        if let Some(target) = &self.options.target {
            attributes = attributes.with_target(target.clone());
        }
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Rc::new(window),
            Err(error) => {
                println!("no window: {error}");
                event_loop.exit();
                return;
            },
        };
        let size = window.inner_size();
        let selector = window.surface_selector();
        println!(
            "window {}x{} scale {} theme {:?}, surface selector {}",
            size.width,
            size.height,
            window.scale_factor(),
            window.theme(),
            match selector.strip_prefix("wgui:") {
                Some(rest) => format!("wgui: and {} more", rest.len()),
                None => format!("{selector:?}"),
            },
        );
        self.surface = Some(Surface::new(&self.context, window.clone()).expect("a surface"));
        self.window = Some(window);
        if self.options.animate.is_some() {
            event_loop.set_control_flow(ControlFlow::Poll);
            self.animating = Some((Instant::now(), 0));
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: User) {
        println!("user event {event:?}");
        event_loop.exit();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::RedrawRequested => {
                self.draw();
                if let Some((_, frames)) = &mut self.animating {
                    *frames += 1;
                    self.window().request_redraw();
                } else {
                    let size = self.window().inner_size();
                    println!("RedrawRequested: drew {}x{}", size.width, size.height);
                }
            },
            WindowEvent::Resized(PhysicalSize { width, height }) => println!("Resized {width}x{height}"),
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                println!("ScaleFactorChanged {scale_factor}")
            },
            WindowEvent::Moved(position) => println!("Moved {},{}", position.x, position.y),
            WindowEvent::Focused(focused) => println!("Focused {focused}"),
            WindowEvent::Occluded(occluded) => println!("Occluded {occluded}"),
            WindowEvent::ThemeChanged(theme) => println!("ThemeChanged {theme:?}"),
            WindowEvent::CloseRequested => {
                println!("CloseRequested");
                event_loop.exit();
            },
            WindowEvent::Destroyed => println!("Destroyed"),
            WindowEvent::ModifiersChanged(modifiers) => {
                println!("ModifiersChanged {:?} left shift {:?}", modifiers.state(), modifiers.lshift_state())
            },
            WindowEvent::KeyboardInput { event, is_synthetic, .. } => {
                let unmodified = event.key_without_modifiers();
                let all = event.text_with_all_modifiers().map(str::to_owned);
                let KeyEvent { physical_key, logical_key, text, location, state, repeat, .. } = event;
                println!(
                    "KeyboardInput {state:?} {} key={} unmodified={} text={:?} all={:?} {location:?}{}{}",
                    match physical_key {
                        PhysicalKey::Code(code) => format!("{code:?}"),
                        PhysicalKey::Unidentified(_) => "Unidentified".to_owned(),
                    },
                    key_name(&logical_key),
                    key_name(&unmodified),
                    text.as_deref(),
                    all,
                    if repeat { " repeat" } else { "" },
                    if is_synthetic { " synthetic" } else { "" },
                );
                if let (ElementState::Pressed, false, PhysicalKey::Code(code)) = (state, repeat, physical_key) {
                    self.on_key(event_loop, code);
                }
            },
            WindowEvent::Ime(Ime::Enabled) => println!("Ime Enabled"),
            WindowEvent::Ime(Ime::Disabled) => println!("Ime Disabled"),
            WindowEvent::Ime(Ime::Preedit(text, cursor)) => println!("Ime Preedit {text:?} {cursor:?}"),
            WindowEvent::Ime(Ime::Commit(text)) => println!("Ime Commit {text:?}"),
            WindowEvent::CursorEntered { .. } => println!("CursorEntered"),
            WindowEvent::CursorLeft { .. } => println!("CursorLeft"),
            WindowEvent::CursorMoved { position, .. } => println!("CursorMoved {},{}", position.x, position.y),
            WindowEvent::MouseInput { state, button, .. } => println!("MouseInput {state:?} {button:?}"),
            WindowEvent::MouseWheel { delta, phase, .. } => match delta {
                MouseScrollDelta::LineDelta(x, y) => println!("MouseWheel lines {x},{y} {phase:?}"),
                MouseScrollDelta::PixelDelta(by) => println!("MouseWheel pixels {},{} {phase:?}", by.x, by.y),
            },
            WindowEvent::Touch(touch) => println!(
                "Touch {:?} id={} at {},{} force={:?}",
                touch.phase,
                touch.id,
                touch.location.x,
                touch.location.y,
                touch.force.map(|force| force.normalized()),
            ),
            other => println!("{other:?}"),
        }
    }

    fn device_event(&mut self, _event_loop: &ActiveEventLoop, _device: DeviceId, event: DeviceEvent) {
        match event {
            DeviceEvent::MouseMotion { delta } => println!("device MouseMotion {},{}", delta.0, delta.1),
            other => println!("device {other:?}"),
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        let size = self.window.as_ref().map(|window| window.inner_size()).unwrap_or_default();
        println!("exiting at {}x{} after {} frames", size.width, size.height, self.frames);
        // The surface draws on the window: it goes first.
        self.surface = None;
        self.window = None;
    }
}

fn main() {
    let mut options =
        Options { target: None, quit_after: None, animate: None, interrupt_after: None };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--target" => options.target = args.next(),
            "--quit-after" => options.quit_after = args.next().and_then(|ms| ms.parse().ok()),
            "--animate" => options.animate = args.next().and_then(|ms| ms.parse().ok()),
            "--interrupt-after" => {
                options.interrupt_after = args.next().and_then(|ms| ms.parse().ok())
            },
            _ => {},
        }
    }

    let event_loop = match EventLoop::<User>::with_user_event().build() {
        Ok(event_loop) => event_loop,
        Err(error) => {
            println!("no event loop: {error}");
            std::process::exit(1);
        },
    };
    if let Some(milliseconds) = options.quit_after {
        // The event it sends has to end the loop's wait in the host.
        let proxy = event_loop.create_proxy();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(milliseconds));
            let _ = proxy.send_event(User::Quit);
        });
    }
    if let Some(milliseconds) = options.interrupt_after {
        extern "C" {
            fn getpid() -> i32;
            fn kill(pid: i32, signal: i32) -> i32;
        }
        const SIGINT: i32 = 2;
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(milliseconds));
            println!("interrupting");
            unsafe { kill(getpid(), SIGINT) };
        });
    }
    let context = Context::new(event_loop.owned_display_handle()).expect("a display to draw on");
    let mut app = App {
        options,
        context,
        window: None,
        surface: None,
        frames: 0,
        ime: false,
        maximized: false,
        cursors: 0,
        animating: None,
    };
    event_loop.run_app(&mut app).expect("the loop to run");
    println!("done");
}

//! Scripted input for guests on a headless window system.
//!
//! A script is a JSON file: how the headless system is set up, and the steps
//! of a user at it. Each step is applied once the guest has dealt with the
//! one before and sits waiting for more, so a run comes out the same every
//! time, however fast the machine is.
//!
//! ```json
//! {
//!   "options": { "size": [640, 480] },
//!   "steps": [
//!     { "focus": true },
//!     { "down": { "code": "KeyA", "text": "a" } },
//!     { "up": { "code": "KeyA" } },
//!     { "move": [10, 20] },
//!     { "press": 1 },
//!     { "release": 1 },
//!     "close"
//!   ]
//! }
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;

use crate::abi::AbiEnum;
use crate::abi::enums::NAMED_KEYS;
use crate::headless::{
    HeadlessClock, HeadlessOptions, HeadlessRedraws, HeadlessSystem, HeadlessWindow,
};
use crate::system::*;
use crate::{ClipboardRead, GuiLimits, GuiPolicy};

/// How long a guest gets to deal with a step before the run is given up.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(20);

/// The headless system a script runs on. What is left out is the default.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScriptOptions {
    pub size: Option<(u32, u32)>,
    pub scale: Option<f64>,
    pub display: Option<(u32, u32)>,
    /// How often the display refreshes, in hertz.
    pub refresh: Option<f64>,
    /// `"immediate"` (the default): a window asked to redraw is told at once
    /// that it can be. `"paced"`: it is told when the display refreshes,
    /// which it does when a `frames` step says so.
    pub redraws: Option<String>,
    pub targets: Vec<String>,
    pub top_level: Option<bool>,
    pub theme: Option<String>,
    /// `"control"` or `"super"`.
    pub shortcut: Option<String>,
    pub clipboard_sync: Option<bool>,
    /// `"deny"`, `"paste"` (the default) or `"allow"`.
    pub clipboard_read: Option<String>,
    /// Unread records the guest may hold.
    pub max_pending_events: Option<usize>,
    pub max_windows: Option<usize>,
}

/// A key, as a script names it.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScriptKey {
    /// The KeyboardEvent `code`: `"KeyA"`, `"ShiftLeft"`.
    pub code: String,
    /// The KeyboardEvent `key`: a character or a name (`"Enter"`). Left out,
    /// it is the first character of `text`.
    pub key: Option<String>,
    /// The key without modifiers.
    pub base: Option<String>,
    pub text: Option<String>,
    /// The modifiers after the event: `["shift", "left_shift"]`.
    pub mods: Option<Vec<String>>,
    /// `"left"`, `"right"` or `"numpad"`.
    pub location: Option<String>,
    pub composing: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptTouch {
    pub id: u64,
    /// `"down"`, `"move"`, `"up"` or `"cancel"`.
    pub phase: String,
    pub at: (f64, f64),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptScroll {
    /// `"began"`, `"changed"`, `"ended"` or `"cancelled"`.
    pub phase: String,
    #[serde(default)]
    pub delta: (f64, f64),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptCompose {
    pub text: String,
    /// The input method's cursor in the text, in bytes.
    pub cursor: Option<(u32, u32)>,
}

/// One thing a user, or the system around the guest, does.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Step {
    /// The steps that follow are for this window: 0 is the oldest open one.
    Window(usize),
    Focus(bool),
    Resize((u32, u32)),
    /// The user asks to close the window.
    Close,
    /// The user leaves fullscreen and any cursor grab, as Escape does in a
    /// browser.
    Escape,
    Down(ScriptKey),
    Up(ScriptKey),
    /// Types text: for each character, the key that makes it goes down and up.
    Type(String),
    /// The modifiers changed without a key the window saw.
    Mods(Vec<String>),
    Enter((f64, f64)),
    Leave((f64, f64)),
    Move((f64, f64)),
    /// The mouse moves by this much from where it is: what a locked cursor
    /// reports.
    MoveBy((f64, f64)),
    /// A mouse button goes down where the cursor is.
    Press(u32),
    Release(u32),
    Touch(ScriptTouch),
    /// A wheel turns by so many notches (x, y).
    Wheel((f64, f64)),
    /// A touchpad scrolls by pixels.
    Scroll(ScriptScroll),
    Compose(ScriptCompose),
    /// An input method commits text.
    Commit(String),
    /// Another program copies this text.
    Clipboard(String),
    /// Time passes: milliseconds on the system's clock.
    Advance(u64),
    /// The display refreshes this many times, the guest drawing in between:
    /// with `"redraws": "paced"`, a guest that draws frame after frame is
    /// walked through that many of them.
    Frames(u32),
    /// Writes what the window shows to `<name>.png`, where the run keeps
    /// such files. Nothing is written by a run that has no place for them.
    Screenshot(String),
    /// A mouse moves, as the device reports it.
    RawMotion((f64, f64)),
    /// `"light"` or `"dark"`: the window's theme changes.
    Theme(String),
    /// What the guest must have asked of the window since the last such
    /// step, in the headless system's words (`"fullscreen Borderless"`).
    Expect(Vec<String>),
}

/// A script: a system, and what happens on it.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Script {
    pub options: ScriptOptions,
    pub steps: Vec<Step>,
}

impl Script {
    pub fn parse(json: &str) -> Result<Self> {
        serde_json::from_str(json).context("the script is not valid")
    }

    pub fn load(path: &Path) -> Result<Self> {
        let json = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        Self::parse(&json).with_context(|| format!("in {}", path.display()))
    }

    /// The system the script asks for. Its clock only moves when the script
    /// says so.
    pub fn system(&self) -> Result<HeadlessSystem> {
        let options = &self.options;
        let default = HeadlessOptions::default();
        Ok(HeadlessSystem::new(HeadlessOptions {
            size: options.size.unwrap_or(default.size),
            scale_factor: options.scale.unwrap_or(default.scale_factor),
            display: options.display.unwrap_or(default.display),
            refresh_rate: match options.refresh {
                None => default.refresh_rate,
                Some(hertz) if (1.0..=1000.0).contains(&hertz) => (hertz * 1000.0).round() as u32,
                Some(hertz) => bail!("a display does not refresh at {hertz} Hz"),
            },
            redraws: match options.redraws.as_deref() {
                None | Some("immediate") => HeadlessRedraws::Immediate,
                Some("paced") => HeadlessRedraws::Paced,
                Some(other) => bail!("unknown way to redraw `{other}`"),
            },
            // Frames are only kept for a script that looks at them.
            capture_frames: self
                .steps
                .iter()
                .any(|step| matches!(step, Step::Screenshot(_))),
            targets: options.targets.clone(),
            top_level: options.top_level.unwrap_or(default.top_level),
            theme: match options.theme.as_deref() {
                None => default.theme,
                Some(theme) => parse_theme(theme)?,
            },
            shortcut_modifier: match options.shortcut.as_deref() {
                None | Some("control") => modifiers::CONTROL,
                Some("super") => modifiers::SUPER,
                Some(other) => bail!("unknown shortcut modifier `{other}`"),
            },
            clipboard_sync: options.clipboard_sync.unwrap_or(default.clipboard_sync),
            clock: HeadlessClock::Manual,
        }))
    }

    pub fn policy(&self) -> Result<GuiPolicy> {
        Ok(GuiPolicy {
            clipboard_read: match self.options.clipboard_read.as_deref() {
                None | Some("paste") => ClipboardRead::OnPaste,
                Some("deny") => ClipboardRead::Deny,
                Some("allow") => ClipboardRead::Allow,
                Some(other) => bail!("unknown clipboard policy `{other}`"),
            },
            ..GuiPolicy::default()
        })
    }

    pub fn limits(&self) -> GuiLimits {
        let default = GuiLimits::default();
        GuiLimits {
            max_pending_events: self
                .options
                .max_pending_events
                .or(default.max_pending_events),
            max_windows: self.options.max_windows.or(default.max_windows),
            ..default
        }
    }
}

fn parse_theme(name: &str) -> Result<Theme> {
    Ok(match name {
        "light" => Theme::Light,
        "dark" => Theme::Dark,
        "unknown" => Theme::Unknown,
        other => bail!("unknown theme `{other}`"),
    })
}

fn parse_modifiers(names: &[String]) -> Result<u32> {
    let mut bits = 0;
    for name in names {
        let wanted = name.replace('_', "").to_ascii_lowercase();
        let (_, bit) = modifiers::NAMES
            .iter()
            .find(|(known, _)| known.to_ascii_lowercase() == wanted)
            .with_context(|| format!("unknown modifier `{name}`"))?;
        bits |= bit;
    }
    Ok(bits)
}

fn parse_code(code: &str) -> Result<PhysicalKey> {
    PhysicalKey::ALL
        .iter()
        .copied()
        .find(|key| key.name() == code)
        .with_context(|| format!("unknown key code `{code}`"))
}

/// A KeyboardEvent `key` value as a `WGUIKey`, and as the string a key event
/// carries for it.
fn parse_key(key: &str) -> Result<(u32, String)> {
    // "Dead" is a dead key, and "Dead ^" one that is known to add a circumflex.
    if let Some(adds) = key.strip_prefix("Dead") {
        let adds = adds.trim().chars().next().map_or(0, u32::from);
        return Ok((crate::abi::enums::KEY_DEAD | adds, String::new()));
    }
    let mut chars = key.chars();
    match (chars.next(), chars.next()) {
        (None, _) => Ok((0, String::new())),
        (Some(only), None) => Ok((u32::from(only), key.to_owned())),
        _ => match NAMED_KEYS.iter().find(|(name, _)| *name == key) {
            Some((_, value)) => Ok((*value, String::new())),
            // More than one character and not a name: a sequence.
            None => Ok((
                crate::abi::enums::KEY_SEQUENCE | u32::from(key.chars().next().unwrap_or('\0')),
                key.to_owned(),
            )),
        },
    }
}

fn key_input(key: &ScriptKey, pressed: bool) -> Result<KeyInput> {
    let text = key.text.clone().unwrap_or_default();
    let (value, key_string) = match &key.key {
        Some(name) => parse_key(name)?,
        None => parse_key(&text.chars().next().map(String::from).unwrap_or_default())?,
    };
    Ok(KeyInput {
        physical_key: parse_code(&key.code)?,
        native_scancode: 0,
        key: value,
        key_unmodified: match &key.base {
            Some(base) => parse_key(base)?.0,
            None => 0,
        },
        key_string,
        location: match key.location.as_deref() {
            None | Some("standard") => KeyLocation::Standard,
            Some("left") => KeyLocation::Left,
            Some("right") => KeyLocation::Right,
            Some("numpad") => KeyLocation::Numpad,
            Some(other) => bail!("unknown key location `{other}`"),
        },
        pressed,
        composing: key.composing,
        text: if pressed { text } else { String::new() },
        text_with_all_modifiers: String::new(),
        modifiers: key.mods.as_deref().map(parse_modifiers).transpose()?,
        device: 0,
    })
}

/// The key of a US keyboard that types a character, for [`Step::Type`].
fn code_for(character: char) -> PhysicalKey {
    let name = match character {
        'a'..='z' | 'A'..='Z' => format!("Key{}", character.to_ascii_uppercase()),
        '0'..='9' => format!("Digit{character}"),
        ' ' => "Space".to_owned(),
        '.' => "Period".to_owned(),
        ',' => "Comma".to_owned(),
        '-' => "Minus".to_owned(),
        '/' => "Slash".to_owned(),
        _ => return PhysicalKey::Unidentified,
    };
    parse_code(&name).unwrap_or(PhysicalKey::Unidentified)
}

/// Plays a script to the guests of a headless system.
pub struct Player<'a> {
    system: &'a HeadlessSystem,
    /// Which of the open windows the steps are for.
    current: usize,
    /// Where each window's mouse cursor is.
    cursors: HashMap<u64, (f64, f64)>,
    /// Where `screenshot` steps write.
    screenshots: Option<PathBuf>,
}

impl<'a> Player<'a> {
    pub fn new(system: &'a HeadlessSystem) -> Self {
        Self {
            system,
            current: 0,
            cursors: HashMap::new(),
            screenshots: None,
        }
    }

    /// The directory `screenshot` steps write their files to.
    pub fn screenshots(mut self, directory: Option<PathBuf>) -> Self {
        self.screenshots = directory;
        self
    }

    /// Waits until the guest has dealt with everything so far and waits for
    /// more itself.
    pub fn settle(&self) -> Result<()> {
        if self.system.wait_idle(SETTLE_TIMEOUT) {
            Ok(())
        } else {
            Err(anyhow!(
                "the guest did not come to rest: it has to drain its events and call \
                 wguiInstanceWaitEvents"
            ))
        }
    }

    fn window(&self) -> Result<HeadlessWindow> {
        self.system
            .windows()
            .into_iter()
            .nth(self.current)
            .with_context(|| format!("the guest has no window {}", self.current))
    }

    /// Applies every step, letting the guest settle before each and after
    /// the last.
    pub fn play(&mut self, steps: &[Step]) -> Result<()> {
        for (index, step) in steps.iter().enumerate() {
            self.settle()
                .with_context(|| format!("before step {index}"))?;
            self.step(step)
                .with_context(|| format!("in step {index}: {step:?}"))?;
        }
        Ok(())
    }

    fn step(&mut self, step: &Step) -> Result<()> {
        match step {
            Step::Window(index) => {
                self.current = *index;
                self.window()?;
            }
            Step::Focus(focused) => self.window()?.focus(*focused),
            Step::Resize((width, height)) => self.window()?.resize(*width, *height),
            Step::Close => self.window()?.close(),
            Step::Escape => self.window()?.escape(),
            Step::Down(key) => self.window()?.key(key_input(key, true)?),
            Step::Up(key) => self.window()?.key(key_input(key, false)?),
            Step::Type(text) => {
                let window = self.window()?;
                for character in text.chars() {
                    let key = KeyInput {
                        physical_key: code_for(character),
                        key: u32::from(character),
                        key_string: character.to_string(),
                        pressed: true,
                        text: character.to_string(),
                        ..KeyInput::default()
                    };
                    window.key(key.clone());
                    // Each character is its own exchange with the guest.
                    self.settle()?;
                    window.key(KeyInput {
                        pressed: false,
                        text: String::new(),
                        ..key
                    });
                    self.settle()?;
                }
            }
            Step::Mods(names) => {
                let bits = parse_modifiers(names)?;
                self.window()?.report(|sink| sink.modifiers(bits));
            }
            Step::Enter(at) | Step::Leave(at) | Step::Move(at) => {
                let window = self.window()?;
                self.cursors.insert(window.id(), *at);
                window.pointer(PointerInput {
                    phase: match step {
                        Step::Enter(_) => PointerPhase::Enter,
                        Step::Leave(_) => PointerPhase::Leave,
                        _ => PointerPhase::Motion,
                    },
                    x: at.0,
                    y: at.1,
                    ..PointerInput::default()
                });
            }
            Step::MoveBy(delta) => {
                let window = self.window()?;
                let at = self.cursors.entry(window.id()).or_default();
                at.0 += delta.0;
                at.1 += delta.1;
                window.pointer(PointerInput {
                    x: at.0,
                    y: at.1,
                    delta: Some(*delta),
                    ..PointerInput::default()
                });
            }
            Step::Press(button) | Step::Release(button) => {
                let window = self.window()?;
                let at = self.cursors.get(&window.id()).copied().unwrap_or_default();
                window.pointer(PointerInput {
                    phase: if matches!(step, Step::Press(_)) {
                        PointerPhase::Down
                    } else {
                        PointerPhase::Up
                    },
                    button: *button,
                    x: at.0,
                    y: at.1,
                    click_count: 1,
                    ..PointerInput::default()
                });
            }
            Step::Touch(touch) => self.window()?.pointer(PointerInput {
                kind: PointerKind::Touch,
                source: touch.id,
                phase: match touch.phase.as_str() {
                    "down" => PointerPhase::Down,
                    "move" => PointerPhase::Motion,
                    "up" => PointerPhase::Up,
                    "cancel" => PointerPhase::Cancel,
                    other => bail!("unknown touch phase `{other}`"),
                },
                x: touch.at.0,
                y: touch.at.1,
                ..PointerInput::default()
            }),
            Step::Wheel(delta) => {
                let window = self.window()?;
                let at = self.cursors.get(&window.id()).copied().unwrap_or_default();
                window.wheel(WheelInput {
                    unit: WheelUnit::Line,
                    phase: WheelPhase::None,
                    flags: 0,
                    delta_x: delta.0,
                    delta_y: delta.1,
                    x: at.0,
                    y: at.1,
                    device: 0,
                });
            }
            Step::Scroll(scroll) => {
                let window = self.window()?;
                let at = self.cursors.get(&window.id()).copied().unwrap_or_default();
                window.wheel(WheelInput {
                    unit: WheelUnit::Pixel,
                    phase: match scroll.phase.as_str() {
                        "began" => WheelPhase::Began,
                        "changed" => WheelPhase::Changed,
                        "ended" => WheelPhase::Ended,
                        "cancelled" => WheelPhase::Cancelled,
                        other => bail!("unknown scroll phase `{other}`"),
                    },
                    flags: 0,
                    delta_x: scroll.delta.0,
                    delta_y: scroll.delta.1,
                    x: at.0,
                    y: at.1,
                    device: 0,
                });
            }
            Step::Compose(compose) => self
                .window()?
                .report(|sink| sink.text_composition(&compose.text, compose.cursor)),
            Step::Commit(text) => self.window()?.report(|sink| sink.text_commit(text)),
            Step::Clipboard(text) => self.system.set_clipboard_text(text),
            Step::Advance(milliseconds) => self.system.advance(milliseconds * 1_000_000),
            Step::Frames(count) => {
                let interval = self.system.frame_interval().as_nanos() as u64;
                for frame in 0..*count {
                    if frame > 0 {
                        // The guest draws, and comes to rest waiting for the
                        // display again.
                        self.settle()?;
                    }
                    self.system.advance(interval);
                }
            }
            Step::Screenshot(name) => {
                if let Some(directory) = &self.screenshots {
                    let frame = self
                        .window()?
                        .last_frame()
                        .context("the guest has shown nothing in the window")?;
                    let png = frame.to_png().context("the window's frames are not kept")?;
                    std::fs::create_dir_all(directory)
                        .with_context(|| format!("failed to create {}", directory.display()))?;
                    let path = directory.join(format!("{name}.png"));
                    std::fs::write(&path, png)
                        .with_context(|| format!("failed to write {}", path.display()))?;
                }
            }
            Step::RawMotion(delta) => {
                for instance in self.system.instances() {
                    instance.raw_motion(0, delta.0, delta.1);
                }
            }
            Step::Theme(theme) => {
                let theme = parse_theme(theme)?;
                self.window()?.report(|sink| sink.theme_changed(theme));
            }
            Step::Expect(wanted) => {
                let asked = self.window()?.take_requests();
                if asked != *wanted {
                    bail!("the guest asked {asked:?} of the window, not {wanted:?}");
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_script_reads_as_it_is_written() {
        let script = Script::parse(
            r#"{
                "options": { "size": [640, 480], "clipboard_read": "allow", "shortcut": "super" },
                "steps": [
                    { "focus": true },
                    { "down": { "code": "KeyA", "text": "a", "mods": ["shift", "left_shift"] } },
                    { "up": { "code": "KeyA" } },
                    { "move": [10, 20.5] },
                    { "press": 1 },
                    { "touch": { "id": 3, "phase": "down", "at": [1, 2] } },
                    { "expect": ["title \"x\""] },
                    "escape",
                    "close"
                ]
            }"#,
        )
        .expect("parses");
        assert_eq!(script.steps.len(), 9);
        assert!(matches!(script.steps[8], Step::Close));
        let system = script.system().expect("valid options");
        assert_eq!(system.options().size, (640, 480));
        assert_eq!(system.options().shortcut_modifier, modifiers::SUPER);
        assert_eq!(
            script.policy().expect("valid").clipboard_read,
            ClipboardRead::Allow
        );
    }

    #[test]
    fn mistakes_in_a_script_are_named() {
        for (json, said) in [
            (r#"{ "steps": [ { "jump": 1 } ] }"#, "jump"),
            (r#"{ "steps": [], "option": {} }"#, "option"),
            (r#"{ "options": { "sise": [1, 2] } }"#, "sise"),
        ] {
            let error = format!("{:#}", Script::parse(json).expect_err("rejected"));
            assert!(error.contains(said), "{error}");
        }
        assert!(parse_code("KeyÄ").is_err());
        assert!(parse_modifiers(&["hyper".to_owned()]).is_err());
    }

    #[test]
    fn keys_are_named_as_in_the_browser() {
        assert_eq!(parse_code("KeyA").expect("known"), PhysicalKey::KeyA);
        assert_eq!(parse_key("a").expect("a character"), (0x61, "a".to_owned()));
        assert_eq!(
            parse_key("Enter").expect("a name"),
            (key::ENTER, String::new())
        );
        assert_eq!(
            parse_modifiers(&["shift".to_owned(), "left_shift".to_owned()]).expect("known"),
            modifiers::SHIFT | modifiers::LEFT_SHIFT
        );
    }
}

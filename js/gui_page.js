// The page's half of wasmer-gui in a browser.
//
// A guest's window is a canvas of the page. This file is everything that has
// to touch the DOM for that: it finds the canvas a guest asks for, listens to
// what the user does on it, and carries out what the guest asks of it. It
// knows nothing of guests, queues or records. What happens is told to a
// `core` by calling its functions (the Rust half in a build, a recorder in
// tests), and what is to be done arrives as calls of the methods of the page
// object made here. Both directions pass numbers and strings only.
//
// Nothing here waits, and nothing here is asked twice: a guest that polls
// cannot answer the browser in time, so what the browser wants decided on
// the spot (which default actions to prevent, where the caret is) was said
// by the guest beforehand and is kept here.

import {
  CursorGrab,
  CursorGrabReason,
  CursorShape,
  DefaultActions,
  KeyLocation,
  Modifiers,
  NAMED_KEYS,
  PHYSICAL_KEYS,
  PointerButton,
  PointerKind,
  PointerTool,
  ScreenKeyboardPolicy,
  Status,
  TextInputPurpose,
  Theme,
  WheelPhase,
  WheelUnit,
} from "./gui_abi.gen.js";

// `WGUIKey` tags (gui.h).
const KEY_NAMED = 0x01000000;
const KEY_DEAD = 0x02000000;
const KEY_SEQUENCE = 0x03000000;

// The `WGUIWindowFlags` bit this file reports. It fits a double.
const FLAG_OCCLUDED = 0x40000;

// The flags of `core.key`.
export const KEY_PRESSED = 1;
export const KEY_COMPOSING = 2;

// Where `core.pointer` finds what in the numbers it is handed.
export const POINTER = Object.freeze({
  Phase: 0,
  Kind: 1,
  Id: 2,
  Button: 3,
  X: 4,
  Y: 5,
  Measured: 6,
  DeltaX: 7,
  DeltaY: 8,
  Pressure: 9,
  TiltX: 10,
  TiltY: 11,
  Twist: 12,
  Width: 13,
  Height: 14,
  Tool: 15,
  TangentialPressure: 16,
  Length: 17,
});

// What a pointer did, as the core numbers it.
export const PointerPhase = Object.freeze({
  Enter: 0,
  Leave: 1,
  Motion: 2,
  Down: 3,
  Up: 4,
  Cancel: 5,
});

/** How long after the user's paste shortcut a paste event still belongs to it. */
const PASTE_KEY_WINDOW_MS = 1000;
/** How long a read waits for the paste event that may be on its way. */
const PASTE_EVENT_WAIT_MS = 500;
/** How long after a key press the field's own reaction to it is expected. */
const KEY_INPUT_WINDOW_MS = 100;

const SHAPE_NAMES = new Map(
  Object.entries(CursorShape).map(([name, value]) => [
    value,
    // The enum is named after CSS: `NeResize` is `ne-resize`.
    name.replace(/[A-Z]/g, (letter, at) => (at ? "-" : "") + letter.toLowerCase()),
  ]),
);

const POINTER_KINDS = { mouse: PointerKind.Mouse, touch: PointerKind.Touch, pen: PointerKind.Pen };

// MouseEvent.button -> WGUIPointerButton.
const BUTTONS = [
  PointerButton.Primary,
  PointerButton.Auxiliary,
  PointerButton.Secondary,
  PointerButton.Back,
  PointerButton.Forward,
  PointerButton.Eraser,
];

const INPUT_MODES = new Map([
  [TextInputPurpose.Number, "decimal"],
  [TextInputPurpose.Pin, "numeric"],
  [TextInputPurpose.Phone, "tel"],
  [TextInputPurpose.Url, "url"],
  [TextInputPurpose.Email, "email"],
  [TextInputPurpose.Search, "search"],
]);

const encoder = new TextEncoder();

/** Length of `text` in UTF-8, the unit the core counts text in. */
function utf8Length(text) {
  return encoder.encode(text).length;
}

function isOneCharacter(text) {
  if (text.length === 0 || text.length > 2) return false;
  return [...text].length === 1;
}

/**
 * `KeyboardEvent.key` as a `WGUIKey`, and the text of it when it is not a
 * named key.
 */
export function keyOf(key) {
  if (typeof key !== "string" || key === "" || key === "Unidentified") return [0, ""];
  if (key === "Dead") return [KEY_DEAD, ""];
  if (isOneCharacter(key)) return [key.codePointAt(0), key];
  const named = NAMED_KEYS[key];
  if (named !== undefined) return [named, ""];
  // More than one character and not a name: what a key types on some layouts.
  return [(KEY_SEQUENCE | (key.codePointAt(0) & 0xffffff)) >>> 0, key];
}

/** The `WGUIModifiers` a keyboard or pointer event says are in effect. */
export function modifiersOf(event) {
  if (typeof event.getModifierState !== "function") return 0;
  let bits = 0;
  if (event.shiftKey) bits |= Modifiers.Shift;
  if (event.ctrlKey) bits |= Modifiers.Control;
  if (event.altKey) bits |= Modifiers.Alt;
  if (event.metaKey) bits |= Modifiers.Super;
  if (event.getModifierState("AltGraph")) bits |= Modifiers.AltGraph;
  if (event.getModifierState("CapsLock")) bits |= Modifiers.CapsLock;
  if (event.getModifierState("NumLock")) bits |= Modifiers.NumLock;
  if (event.getModifierState("ScrollLock")) bits |= Modifiers.ScrollLock;
  return bits;
}

/** Whether a key press types `event.key` into a text field. */
function typesText(event) {
  if (!isOneCharacter(event.key)) return false;
  // AltGraph shows as Control and Alt on some systems, and types.
  const altGraph = event.getModifierState?.("AltGraph") ?? false;
  if (event.metaKey) return false;
  return altGraph || !event.ctrlKey;
}

function isPasteChord(event) {
  const key = typeof event.key === "string" ? event.key.toLowerCase() : "";
  return (
    ((event.ctrlKey || event.metaKey) && !event.altKey && key === "v") ||
    (event.shiftKey && event.key === "Insert") ||
    event.key === "Paste"
  );
}

/** One canvas that is a guest's window. */
class View {
  constructor(page, id, canvas, selector) {
    this.page = page;
    this.id = id;
    this.canvas = canvas;
    this.selector = selector;
    this.listeners = new AbortController();
    this.width = 0;
    this.height = 0;
    this.scale = 1;
    this.focused = false;
    this.occluded = false;
    this.onScreen = true;
    this.actions = DefaultActions.Default;
    this.cursor = "";
    this.cursorVisible = true;
    this.redrawAsked = false;
    /** The user did something to the window since the guest was last told to redraw it. */
    this.inputSinceRedraw = false;
    this.locked = false;
    // The text field that stands in for the canvas while text is entered.
    this.field = null;
    this.textInput = null;
    this.composing = false;
    /** Text a key press has already reported, which the field will insert. */
    this.typedByKey = "";
    this.pasteKeyAt = -Infinity;
    this.backspaceKeyAt = -Infinity;
    // What the canvas was like before it was a window, to give it back.
    this.before = {
      tabIndex: canvas.getAttribute("tabindex"),
      outline: canvas.style.outline,
      cursor: canvas.style.cursor,
      touchAction: canvas.style.touchAction,
      label: canvas.getAttribute("aria-label"),
      width: canvas.style.width,
      height: canvas.style.height,
    };
  }

  /** Physical pixels of the canvas for each CSS pixel, along each axis. */
  ratio() {
    const box = this.canvas.getBoundingClientRect();
    return [
      box.width > 0 ? this.width / box.width : this.scale,
      box.height > 0 ? this.height / box.height : this.scale,
      box,
    ];
  }

  /** Where an event happened in the window, in physical pixels. */
  position(event) {
    const [x, y, box] = this.ratio();
    return [(event.clientX - box.left) * x, (event.clientY - box.top) * y];
  }

  /** The element that has the keyboard while this window has focus. */
  keyboardTarget() {
    return this.field ?? this.canvas;
  }
}

class GuiPage {
  constructor(core, options = {}) {
    this.core = core;
    this.options = options;
    this.document = options.document ?? globalThis.document;
    this.window = this.document?.defaultView ?? globalThis;
    this.views = new Map();
    this.listeners = new AbortController();
    this.disposed = false;
    /** What waits for the user's next gesture, by view. */
    this.armed = new Map();
    /** The fullscreen request each view is owed an answer for. */
    this.fullscreenRequests = new Map();
    this.grabRequests = new Map();
    /** Clipboard reads waiting for a paste event. */
    this.readers = [];
    this.pasted = { text: null, at: -Infinity };
    this.installed = false;
    this.pointerValues = new Float64Array(POINTER.Length);
    /** The page is moving the focus for the guest (see `moveFocus`). */
    this.movingFocus = false;
  }

  // -- what the guest asks for --------------------------------------------------

  /**
   * Makes the canvas `request.target` names a window. Answers
   * `{ status, selector, width, height, scale, focused, occluded, theme }`.
   */
  createWindow(id, request) {
    if (this.disposed || !this.document) return { status: Status.Unsupported };
    let canvas = null;
    try {
      canvas = this.options.resolveCanvas?.(request.target ?? "") ?? null;
    } catch (error) {
      console.warn("[wasmer-gui] the canvas provider failed:", error);
      return { status: Status.Error };
    }
    if (!canvas || typeof canvas.getBoundingClientRect !== "function") {
      return { status: Status.NotFound };
    }
    for (const other of this.views.values()) {
      if (other.canvas === canvas) return { status: Status.Busy };
    }
    this.install();

    const selector = `wgui:${randomName()}`;
    const view = new View(this, id, canvas, selector);
    this.views.set(id, view);
    // WebGPU finds the canvas by the same name.
    try {
      this.options.alias?.(selector, canvas);
    } catch (error) {
      console.warn("[wasmer-gui] the canvas could not be named for drawing:", error);
    }

    if (view.before.tabIndex === null) canvas.tabIndex = 0;
    canvas.style.outline = "none";
    if (typeof request.title === "string" && request.title !== "") {
      this.setTitle(id, request.title);
    }
    if (request.width > 0 && request.height > 0 && this.options.resizable !== false) {
      // A size was asked for. It is the page's layout that decides; this is
      // what the canvas would like to be.
      const scale = request.logical ? 1 : this.devicePixelRatio();
      canvas.style.width = `${request.width / scale}px`;
      canvas.style.height = `${request.height / scale}px`;
    }
    this.applyActions(view);
    this.measure(view, true);
    this.listen(view);
    this.watchSize(view);
    this.watchVisibility(view);

    const focused = this.document.activeElement === canvas;
    view.focused = focused;
    return {
      status: Status.Success,
      selector,
      width: view.width,
      height: view.height,
      scale: view.scale,
      focused,
      occluded: view.occluded,
      theme: this.theme(),
    };
  }

  destroyWindow(id) {
    const view = this.views.get(id);
    if (!view) return;
    this.views.delete(id);
    this.armed.delete(id);
    this.fullscreenRequests.delete(id);
    this.grabRequests.delete(id);
    view.listeners.abort();
    view.resizeObserver?.disconnect();
    view.intersectionObserver?.disconnect();
    if (view.redrawFrame !== undefined) this.window.cancelAnimationFrame?.(view.redrawFrame);
    this.removeField(view);
    const canvas = view.canvas;
    if (this.document.pointerLockElement === canvas) this.document.exitPointerLock?.();
    if (fullscreenElement(this.document) === canvas) exitFullscreen(this.document);
    // The canvas goes back to what it was.
    if (view.before.tabIndex === null) canvas.removeAttribute("tabindex");
    if (view.before.label === null) canvas.removeAttribute("aria-label");
    else canvas.setAttribute("aria-label", view.before.label);
    canvas.style.outline = view.before.outline;
    canvas.style.cursor = view.before.cursor;
    canvas.style.touchAction = view.before.touchAction;
    canvas.style.width = view.before.width;
    canvas.style.height = view.before.height;
    try {
      this.options.alias?.(view.selector, undefined);
    } catch {
      // The registry is gone with the sandbox: nothing left to forget.
    }
    try {
      // The window is gone, and its title with it.
      this.options.title?.("", canvas);
    } catch (error) {
      console.warn("[wasmer-gui] the title handler failed:", error);
    }
  }

  setTitle(id, title) {
    const view = this.views.get(id);
    if (!view) return;
    // A canvas has no title bar. Its name is what assistive technology
    // reads, and what the page may show wherever it likes.
    view.canvas.setAttribute("aria-label", title);
    try {
      this.options.title?.(title, view.canvas);
    } catch (error) {
      console.warn("[wasmer-gui] the title handler failed:", error);
    }
  }

  /** The guest would like the window this big, in physical pixels. */
  setSize(id, width, height) {
    const view = this.views.get(id);
    if (!view || this.options.resizable === false) return;
    const scale = this.devicePixelRatio();
    view.canvas.style.width = `${width / scale}px`;
    view.canvas.style.height = `${height / scale}px`;
    // Whether the layout gave it that is told by the size observer.
  }

  setCursorShape(id, shape) {
    const view = this.views.get(id);
    if (!view) return;
    view.cursor = SHAPE_NAMES.get(shape) ?? "default";
    this.applyCursor(view);
  }

  /**
   * `rgba` is `width * height * 4` bytes, rows from the top, made for a
   * display of scale factor `scale`; null for the default cursor.
   */
  setCustomCursor(id, rgba, width, height, hotspotX, hotspotY, scale = 1) {
    const view = this.views.get(id);
    if (!view) return;
    if (!rgba) {
      view.cursor = "";
    } else {
      try {
        const image = this.document.createElement("canvas");
        image.width = width;
        image.height = height;
        const pixels = new ImageData(new Uint8ClampedArray(rgba), width, height);
        image.getContext("2d").putImageData(pixels, 0, 0);
        let shown = image;
        const factor = scale > 0 ? scale : 1;
        if (factor !== 1) {
          // A cursor image is as big as it has pixels. One made for a
          // denser display is drawn smaller, so that it is the same size.
          shown = this.document.createElement("canvas");
          shown.width = Math.max(1, Math.round(width / factor));
          shown.height = Math.max(1, Math.round(height / factor));
          shown.getContext("2d").drawImage(image, 0, 0, shown.width, shown.height);
        }
        const x = Math.min(shown.width - 1, Math.round(hotspotX / factor));
        const y = Math.min(shown.height - 1, Math.round(hotspotY / factor));
        view.cursor = `url(${shown.toDataURL()}) ${x} ${y}, auto`;
      } catch (error) {
        console.warn("[wasmer-gui] the cursor image could not be used:", error);
        view.cursor = "";
      }
    }
    this.applyCursor(view);
  }

  setCursorVisible(id, visible) {
    const view = this.views.get(id);
    if (!view) return;
    view.cursorVisible = visible;
    this.applyCursor(view);
  }

  /**
   * Locks the cursor to the window, or lets it go. The browser only locks
   * it in answer to something the user did, so a lock asked for at another
   * moment waits for the user's next press.
   */
  setCursorGrab(id, grab, request) {
    const view = this.views.get(id);
    if (!view) return this.core.requestDone(request, Status.InvalidHandle);
    this.finish(this.grabRequests, id, Status.Cancelled);
    const armed = this.armed.get(id);
    if (armed) armed.lock = false;
    if (grab === CursorGrab.None) {
      if (this.document.pointerLockElement === view.canvas) this.document.exitPointerLock?.();
      view.locked = false;
      this.core.requestDone(request, Status.Success);
      this.core.grabChanged(id, CursorGrab.None, CursorGrabReason.Requested);
      return;
    }
    if (grab !== CursorGrab.Locked || typeof view.canvas.requestPointerLock !== "function") {
      // A cursor cannot be confined here, and is not locked instead.
      this.core.requestDone(request, Status.Unsupported);
      return;
    }
    this.grabRequests.set(id, request);
    this.whenAllowed(view, "lock", () => this.lockPointer(view));
  }

  /**
   * Enters or leaves fullscreen. Like the lock, entering waits for the
   * user's next press, unless the guest would rather be refused (`wait`).
   */
  setFullscreen(id, on, request, wait = true) {
    const view = this.views.get(id);
    if (!view) return this.core.requestDone(request, Status.InvalidHandle);
    this.finish(this.fullscreenRequests, id, Status.Cancelled);
    const armed = this.armed.get(id);
    if (armed) armed.fullscreen = false;
    const now = fullscreenElement(this.document) === view.canvas;
    if (on === now) {
      this.core.requestDone(request, Status.Success);
      return;
    }
    if (!on) {
      this.fullscreenRequests.set(id, request);
      exitFullscreen(this.document)?.catch?.(() => this.finish(this.fullscreenRequests, id, Status.Error));
      return;
    }
    if (!canFullscreen(view.canvas)) {
      this.core.requestDone(request, Status.Unsupported);
      return;
    }
    if (!wait && !this.userIsActive()) {
      this.core.requestDone(request, Status.Denied);
      return;
    }
    this.fullscreenRequests.set(id, request);
    view.fullscreenWaits = wait;
    this.whenAllowed(view, "fullscreen", () => this.enterFullscreen(view));
  }

  /**
   * Text is being entered, or no longer. The caret is where the input
   * method's own window goes, in physical pixels of the canvas.
   */
  setTextInput(id, on, purpose, hints, x, y, width, height, keyboard) {
    const view = this.views.get(id);
    if (!view) return;
    if (!on) {
      if (!view.field) return;
      // The canvas takes the keyboard back before the field goes, so that
      // the window never is without it in between.
      if (this.document.activeElement === view.field) {
        this.moveFocus(() => view.canvas.focus({ preventScroll: true }));
      }
      this.removeField(view);
      view.textInput = null;
      this.core.textState(id, false, false);
      return;
    }
    view.textInput = { purpose, hints, x, y, width, height, keyboard };
    const created = !view.field;
    if (created) this.addField(view);
    const field = view.field;
    field.inputMode =
      keyboard === ScreenKeyboardPolicy.Hide ? "none" : (INPUT_MODES.get(purpose) ?? "text");
    this.placeField(view);
    // The field takes the keyboard from the canvas, and with it the focus
    // the window already had.
    if (view.focused || this.document.activeElement === view.canvas) {
      this.moveFocus(() => field.focus({ preventScroll: true }));
    }
    if (created) this.core.textState(id, true, false);
  }

  /** Drops what the input method is composing. */
  resetTextInput(id) {
    const view = this.views.get(id);
    if (!view?.field) return;
    view.composing = false;
    view.typedByKey = "";
    view.field.value = "";
    // The only way a page has of telling an input method to start over.
    if (this.document.activeElement === view.field) {
      this.moveFocus(() => {
        view.field.blur();
        view.field.focus({ preventScroll: true });
      });
    }
    this.core.composition(id, "", -1, -1);
  }

  focus(id) {
    const view = this.views.get(id);
    if (view) this.moveFocus(() => view.keyboardTarget().focus({ preventScroll: true }));
  }

  /** Moves the focus for the guest: what that changes is not the user's doing. */
  moveFocus(move) {
    const outer = this.movingFocus;
    this.movingFocus = true;
    try {
      move();
    } finally {
      this.movingFocus = outer;
    }
  }

  /**
   * The guest asks to be told to redraw a window. `whileDrawing`: the host
   * saw it ask before it waited for events again, after it read its last
   * redraw of the window.
   */
  requestRedraw(id, whileDrawing = false) {
    const view = this.views.get(id);
    if (!view) return;
    // With `eagerRedraw`, a request that answers something the user did is
    // answered at once (after the call that made it, not inside it). The
    // browser's next frame is then mostly a whole frame away: the one in
    // which the input came has already run its callbacks when the guest
    // asks. A request with no input since the last redraw keeps to the
    // frames, so a guest that draws frame after frame on its own does,
    // however long its frames take. So does one the host saw ask while
    // still drawing, and a window nobody can see.
    const eager =
      this.options.eagerRedraw === true &&
      !view.occluded &&
      view.inputSinceRedraw &&
      !whileDrawing;
    if (view.redrawAsked) {
      // Waiting for a frame for a request that answered nothing the user
      // did: this one does, and answers both.
      if (eager && view.redrawFrame !== undefined) {
        this.window.cancelAnimationFrame?.(view.redrawFrame);
        view.redrawFrame = undefined;
        this.deliverSoon(view);
      }
      return;
    }
    view.redrawAsked = true;
    if (eager) {
      this.deliverSoon(view);
    } else if (typeof this.window.requestAnimationFrame === "function") {
      view.redrawFrame = this.window.requestAnimationFrame(() => this.deliverRedraw(view));
    } else {
      this.deliverRedraw(view);
    }
  }

  /** Answers a window's request after the call that made it. */
  deliverSoon(view) {
    queueMicrotask(() => {
      if (view.redrawAsked && view.redrawFrame === undefined) this.deliverRedraw(view);
    });
  }

  /**
   * Tells the guest to redraw a window, which answers the request it has
   * waiting, if any. `exposure`: the window has to be shown whole again,
   * rather than only answering the request.
   */
  deliverRedraw(view, exposure = false) {
    const answering = view.redrawAsked;
    if (view.redrawFrame !== undefined) this.window.cancelAnimationFrame?.(view.redrawFrame);
    view.redrawFrame = undefined;
    view.redrawAsked = false;
    view.inputSinceRedraw = false;
    if (this.views.get(view.id) !== view) return;
    // A core that tells the guest why (`redrawReasonsVersion` 1) hears which
    // redraws answer its request, and whether that answer is an exposure too:
    // in one call, so that the guest reads both in one redraw. Any other core
    // gets a plain redraw, which the guest takes as an exposure.
    if (this.core.redrawReasonsVersion === 1 && typeof this.core.redrawFrame === "function") {
      if (answering || !exposure) this.core.redrawFrame(view.id, exposure);
      else this.core.redraw(view.id);
    } else {
      this.core.redraw(view.id);
    }
  }

  /** The user did something to a window. */
  noteInput(view) {
    view.inputSinceRedraw = true;
  }

  setDefaultActions(id, actions) {
    const view = this.views.get(id);
    if (!view) return;
    view.actions = actions;
    this.applyActions(view);
  }

  /**
   * Shows a frame the guest drew itself. `rgba` is `width * height * 4`
   * bytes, rows from the top; `damage` is the rectangles that changed, as
   * x, y, width, height one after another, or empty for all of it.
   *
   * The canvas has as many pixels as the frame: one drawn at another size
   * than the canvas is shown at is stretched by the page's layout, like any
   * image. That is also what makes resizing clean. The canvas is not cleared
   * when its box changes; it shows the last frame, stretched, until the
   * guest has drawn one at the new size.
   */
  presentPixels(id, rgba, width, height, damage) {
    const view = this.views.get(id);
    if (!view) return;
    const canvas = view.canvas;
    let image;
    try {
      image = new ImageData(rgba, width, height);
    } catch (error) {
      console.warn("[wasmer-gui] a frame could not be shown:", error);
      return;
    }
    if (view.pixels === undefined) {
      // From here on the canvas's size is the frames', not the layout's.
      let context = null;
      try {
        context = canvas.getContext("2d");
      } catch {
        // Reported below.
      }
      view.pixels = context ?? null;
    }
    if (view.pixels === null) {
      // The canvas already draws another way (a WebGPU guest presented to
      // it before): the frame goes the way those frames go.
      const shown = canvas.getContext("bitmaprenderer");
      if (shown && typeof createImageBitmap === "function") {
        // Bitmaps are made in their own time: one that is ready after a
        // newer frame was shown is not shown over it.
        const frame = (view.frames = (view.frames ?? 0) + 1);
        createImageBitmap(image).then(
          (bitmap) => {
            if (frame > (view.shownFrame ?? 0) && this.views.get(id) === view) {
              view.shownFrame = frame;
              shown.transferFromImageBitmap(bitmap);
            } else {
              bitmap.close();
            }
          },
          () => {},
        );
      }
      return;
    }
    const resized = canvas.width !== width || canvas.height !== height;
    if (resized) {
      // Assigning a size clears the canvas: everything has to be drawn.
      canvas.width = width;
      canvas.height = height;
    }
    if (resized || damage.length === 0) {
      view.pixels.putImageData(image, 0, 0);
    } else {
      for (let at = 0; at + 3 < damage.length; at += 4) {
        view.pixels.putImageData(image, 0, 0, damage[at], damage[at + 1], damage[at + 2], damage[at + 3]);
      }
    }
  }

  /** Puts text on the system's clipboard, and says how that went. */
  writeClipboard(request, text) {
    const done = (ok) => this.core.clipboardWritten(request, ok);
    const clipboard = this.window.navigator?.clipboard;
    const fallback = () => done(this.copyByCommand(text));
    if (clipboard && typeof clipboard.writeText === "function") {
      let written;
      try {
        written = clipboard.writeText(text);
      } catch {
        return fallback();
      }
      written.then(() => done(true), fallback);
    } else {
      fallback();
    }
  }

  /**
   * Finds out what the clipboard holds: from the paste the user just made,
   * if there was one, otherwise by asking the browser, which may ask the
   * user.
   */
  readClipboard(request) {
    const fresh = this.now() - this.pasted.at < PASTE_KEY_WINDOW_MS;
    if (fresh && this.pasted.text !== null) {
      this.core.clipboardFetched(request, this.pasted.text, false);
      return;
    }
    const reader = { request, done: false };
    const answer = (text) => {
      if (reader.done) return;
      reader.done = true;
      this.readers = this.readers.filter((other) => other !== reader);
      this.core.clipboardFetched(request, text, false);
    };
    reader.answer = answer;
    this.readers.push(reader);
    const clipboard = this.window.navigator?.clipboard;
    const wait = () => this.window.setTimeout(() => answer(null), PASTE_EVENT_WAIT_MS);
    if (clipboard && typeof clipboard.readText === "function") {
      let read;
      try {
        read = clipboard.readText();
      } catch {
        return void wait();
      }
      // A paste event that arrives first answers; so does a refusal, after
      // the event has had its chance.
      read.then((text) => answer(text), wait);
    } else {
      wait();
    }
  }

  /** The sandbox is gone: every canvas goes back to the page. */
  dispose() {
    if (this.disposed) return;
    this.disposed = true;
    for (const id of [...this.views.keys()]) this.destroyWindow(id);
    this.listeners.abort();
    for (const reader of this.readers) reader.done = true;
    this.readers = [];
  }

  // -- the page -----------------------------------------------------------------

  now() {
    return this.window.performance?.now?.() ?? Date.now();
  }

  devicePixelRatio() {
    const ratio = this.window.devicePixelRatio;
    return typeof ratio === "number" && ratio > 0 ? ratio : 1;
  }

  theme() {
    const dark = this.window.matchMedia?.("(prefers-color-scheme: dark)");
    if (!dark || dark.media === "not all") return Theme.Unknown;
    return dark.matches ? Theme.Dark : Theme.Light;
  }

  reportDisplay() {
    const screen = this.window.screen;
    if (!screen) return;
    const scale = this.devicePixelRatio();
    const px = (value) => Math.round((value ?? 0) * scale);
    this.core.display(
      px(screen.width),
      px(screen.height),
      px(screen.availLeft),
      px(screen.availTop),
      px(screen.availWidth ?? screen.width),
      px(screen.availHeight ?? screen.height),
      scale,
    );
  }

  /**
   * Starts telling the core about the page: its display and its theme now,
   * and what becomes of them. The core calls this once it is ready to hear.
   */
  start() {
    this.install();
  }

  /** Listeners that are the page's, not one canvas's. */
  install() {
    if (this.installed || this.disposed || !this.document) return;
    this.installed = true;
    const signal = this.listeners.signal;
    const document = this.document;
    const on = (target, type, handler, options) =>
      target?.addEventListener?.(type, handler, { signal, ...options });

    for (const type of ["fullscreenchange", "webkitfullscreenchange"]) {
      on(document, type, () => this.fullscreenChanged());
    }
    for (const type of ["fullscreenerror", "webkitfullscreenerror"]) {
      on(document, type, () => this.fullscreenFailed());
    }
    on(document, "pointerlockchange", () => this.pointerLockChanged());
    on(document, "pointerlockerror", () => this.pointerLockFailed());
    on(document, "visibilitychange", () => {
      for (const view of this.views.values()) this.updateOccluded(view);
    });
    // The device pixel ratio changes with the browser's zoom and with the
    // display the window is on. There is no event for it but a query that
    // stops matching.
    const watchRatio = () => {
      const query = this.window.matchMedia?.(`(resolution: ${this.devicePixelRatio()}dppx)`);
      if (!query?.addEventListener) return;
      query.addEventListener(
        "change",
        () => {
          for (const view of this.views.values()) this.measure(view, false);
          this.reportDisplay();
          watchRatio();
        },
        { once: true, signal },
      );
    };
    watchRatio();
    const dark = this.window.matchMedia?.("(prefers-color-scheme: dark)");
    dark?.addEventListener?.("change", () => this.core.theme(this.theme()), { signal });
    this.reportDisplay();
    this.core.theme(this.theme());
  }

  /** Works out the canvas's size in physical pixels, and says so if it changed. */
  measure(view, first, entry) {
    const canvas = view.canvas;
    const scale = this.devicePixelRatio();
    let width = 0;
    let height = 0;
    const box = entry?.contentBoxSize?.[0];
    const cssWidth = box ? box.inlineSize : canvas.clientWidth;
    const cssHeight = box ? box.blockSize : canvas.clientHeight;
    width = Math.round(cssWidth * scale);
    height = Math.round(cssHeight * scale);
    // Where the browser says exactly which device pixels the canvas covers,
    // that settles the rounding, which the product above gets wrong by one
    // at some zoom levels. It is not taken for more than that: a browser
    // made to pretend another pixel ratio reports the real pixels here.
    const exact = entry?.devicePixelContentBoxSize?.[0];
    if (exact && Math.abs(exact.inlineSize - width) <= 1 && Math.abs(exact.blockSize - height) <= 1) {
      width = exact.inlineSize;
      height = exact.blockSize;
    }
    if (width <= 0 || height <= 0) {
      if (!first) return;
      // Not laid out (yet): the canvas's own size is all there is to go by.
      width = Math.max(1, canvas.width | 0);
      height = Math.max(1, canvas.height | 0);
    }
    if (width === view.width && height === view.height && scale === view.scale) return;
    view.width = width;
    view.height = height;
    view.scale = scale;
    // What is drawn has this many pixels: the canvas is told, so that
    // whoever draws on it draws at the size the guest hears. A canvas whose
    // guest presents frames of its own gets its size from those.
    if (this.options.manageCanvasSize !== false && !view.pixels) {
      try {
        if (canvas.width !== width) canvas.width = width;
        if (canvas.height !== height) canvas.height = height;
      } catch {
        // A canvas whose drawing was handed to a worker keeps the size that
        // worker gives it.
      }
    }
    if (!first) {
      this.core.resized(view.id, width, height, scale);
      if (view.field) this.placeField(view);
    }
  }

  watchSize(view) {
    const Observer = this.window.ResizeObserver;
    if (typeof Observer !== "function") return;
    view.resizeObserver = new Observer((entries) => {
      const entry = entries[entries.length - 1];
      if (this.views.get(view.id) === view) this.measure(view, false, entry);
    });
    try {
      view.resizeObserver.observe(view.canvas, { box: "device-pixel-content-box" });
    } catch {
      view.resizeObserver.observe(view.canvas);
    }
  }

  watchVisibility(view) {
    const Observer = this.window.IntersectionObserver;
    if (typeof Observer === "function") {
      view.intersectionObserver = new Observer((entries) => {
        view.onScreen = entries[entries.length - 1].isIntersecting;
        this.updateOccluded(view);
      });
      view.intersectionObserver.observe(view.canvas);
    }
    view.occluded = this.document.visibilityState === "hidden";
  }

  updateOccluded(view) {
    const occluded = this.document.visibilityState === "hidden" || !view.onScreen;
    if (occluded === view.occluded) return;
    view.occluded = occluded;
    this.core.flags(view.id, FLAG_OCCLUDED, occluded ? FLAG_OCCLUDED : 0);
    // What was drawn while nobody could see it went nowhere.
    if (!occluded) this.deliverRedraw(view, true);
  }

  applyCursor(view) {
    view.canvas.style.cursor = view.cursorVisible ? view.cursor : "none";
  }

  applyActions(view) {
    // Panning and zooming by touch are decided before the event is seen.
    view.canvas.style.touchAction =
      view.actions & DefaultActions.TouchGestures ? "none" : view.before.touchAction;
  }

  // -- gestures the browser wants a user behind ---------------------------------

  /** Whether the browser would let the page do now what needs a user's gesture. */
  userIsActive() {
    const activation = this.window.navigator?.userActivation;
    return activation ? activation.isActive : true;
  }

  whenAllowed(view, what, act) {
    if (this.userIsActive()) {
      act();
      return;
    }
    const armed = this.armed.get(view.id) ?? {};
    armed[what] = true;
    this.armed.set(view.id, armed);
  }

  /** Called inside the handler of something the user did. */
  fire(view) {
    const armed = this.armed.get(view.id);
    if (!armed) return;
    this.armed.delete(view.id);
    if (armed.fullscreen) this.enterFullscreen(view);
    if (armed.lock) this.lockPointer(view);
  }

  arm(view, what) {
    const armed = this.armed.get(view.id) ?? {};
    armed[what] = true;
    this.armed.set(view.id, armed);
  }

  enterFullscreen(view) {
    const canvas = view.canvas;
    const refused = () => {
      if (!this.fullscreenRequests.has(view.id)) return;
      // Not now. The user's next press tries again, if the guest waits.
      if (view.fullscreenWaits) this.arm(view, "fullscreen");
      else this.finish(this.fullscreenRequests, view.id, Status.Denied);
    };
    try {
      const entering = canvas.requestFullscreen
        ? canvas.requestFullscreen({ navigationUI: "hide" })
        : canvas.webkitRequestFullscreen();
      entering?.catch?.(refused);
    } catch {
      refused();
    }
  }

  fullscreenChanged() {
    const element = fullscreenElement(this.document);
    for (const view of this.views.values()) {
      const on = element === view.canvas;
      if (on === (view.fullscreen ?? false)) continue;
      view.fullscreen = on;
      this.core.fullscreen(view.id, on);
      this.finish(this.fullscreenRequests, view.id, Status.Success);
    }
  }

  fullscreenFailed() {
    // The browser names no element. Whoever was waiting may have been
    // refused for want of a gesture: the next one tries again.
    for (const id of [...this.fullscreenRequests.keys()]) {
      const view = this.views.get(id);
      if (!view || fullscreenElement(this.document) === view.canvas) continue;
      if (view.fullscreenWaits) this.arm(view, "fullscreen");
      else this.finish(this.fullscreenRequests, id, Status.Denied);
    }
  }

  lockPointer(view) {
    const refused = () => {
      if (this.grabRequests.has(view.id)) this.arm(view, "lock");
    };
    try {
      // Movement as the device reports it, where the browser can.
      const locking = view.canvas.requestPointerLock({ unadjustedMovement: true });
      locking?.catch?.(() => {
        try {
          view.canvas.requestPointerLock()?.catch?.(refused);
        } catch {
          refused();
        }
      });
    } catch {
      refused();
    }
  }

  pointerLockChanged() {
    const element = this.document.pointerLockElement;
    for (const view of this.views.values()) {
      const locked = element === view.canvas;
      if (locked === view.locked) continue;
      view.locked = locked;
      if (locked) {
        this.finish(this.grabRequests, view.id, Status.Success);
        this.core.grabChanged(view.id, CursorGrab.Locked, CursorGrabReason.Requested);
      } else {
        // The guest letting go is reported where it asks. This is the
        // user (Escape) or the browser taking the cursor back.
        this.core.grabChanged(view.id, CursorGrab.None, CursorGrabReason.User);
      }
    }
  }

  pointerLockFailed() {
    for (const id of this.grabRequests.keys()) {
      const view = this.views.get(id);
      if (view && this.document.pointerLockElement !== view.canvas) this.arm(view, "lock");
    }
  }

  finish(requests, id, status) {
    const request = requests.get(id);
    if (request === undefined) return;
    requests.delete(id);
    this.core.requestDone(request, status);
  }

  // -- input --------------------------------------------------------------------

  listen(view) {
    const { canvas, id } = view;
    const signal = view.listeners.signal;
    const on = (target, type, handler, options) =>
      target.addEventListener(type, handler, { signal, ...options });

    on(canvas, "keydown", (event) => this.key(view, event, true));
    on(canvas, "keyup", (event) => this.key(view, event, false));
    on(canvas, "focus", (event) => this.focusChanged(view, event));
    on(canvas, "blur", (event) => this.focusChanged(view, event));

    on(canvas, "pointerenter", (event) => this.pointer(view, event, PointerPhase.Enter));
    on(canvas, "pointerleave", (event) => this.pointer(view, event, PointerPhase.Leave));
    on(canvas, "pointermove", (event) => this.pointer(view, event, PointerPhase.Motion));
    on(canvas, "pointerdown", (event) => {
      // The canvas has to have the keyboard for keys to arrive at all.
      const target = view.keyboardTarget();
      if (this.document.activeElement !== target) target.focus({ preventScroll: true });
      try {
        // A drag goes on when the pointer leaves the canvas.
        canvas.setPointerCapture(event.pointerId);
      } catch {
        // Not every pointer can be captured (a locked one cannot).
      }
      this.fire(view);
      this.pointer(view, event, PointerPhase.Down);
      // The field must keep the keyboard, which a press on the canvas
      // would take from it.
      if (view.field) event.preventDefault();
    });
    on(canvas, "pointerup", (event) => {
      this.fire(view);
      this.pointer(view, event, PointerPhase.Up);
    });
    on(canvas, "pointercancel", (event) => this.pointer(view, event, PointerPhase.Cancel));
    on(
      canvas,
      "wheel",
      (event) => {
        if (view.actions & DefaultActions.Wheel) event.preventDefault();
        this.noteInput(view);
        const [x, y] = view.position(event);
        const [ratioX, ratioY] = view.ratio();
        // Lines stay lines; pixels are the page's, and become the canvas's.
        const lines = event.deltaMode === 1;
        const pages = event.deltaMode === 2;
        const dx = pages ? event.deltaX * view.width : lines ? event.deltaX : event.deltaX * ratioX;
        const dy = pages ? event.deltaY * view.height : lines ? event.deltaY : event.deltaY * ratioY;
        this.core.modifiers(id, modifiersOf(event));
        this.core.wheel(id, lines ? WheelUnit.Line : WheelUnit.Pixel, WheelPhase.None, dx, dy, x, y);
      },
      { passive: false },
    );
    on(canvas, "contextmenu", (event) => {
      if (view.actions & DefaultActions.ContextMenu) event.preventDefault();
    });
    for (const type of ["paste", "copy", "cut"]) {
      on(canvas, type, (event) => this.clipboardEvent(view, event));
    }
  }

  focusChanged(view, event) {
    // Focus moving between the canvas and its own text field is no change.
    // When one of them loses it, the event says who gets it; the document
    // does not know yet.
    const holder = event.type === "blur" ? event.relatedTarget : event.target;
    const focused = holder !== null && (holder === view.canvas || holder === view.field);
    if (focused === view.focused) return;
    view.focused = focused;
    if (!this.movingFocus) this.noteInput(view);
    if (!focused) {
      view.typedByKey = "";
      if (view.composing) {
        view.composing = false;
        this.core.composition(view.id, "", -1, -1);
      }
    }
    this.core.focused(view.id, focused);
  }

  key(view, event, pressed) {
    this.noteInput(view);
    // Chrome marks the events an input method swallows; Firefox and Safari
    // report key 229 for them.
    const composing = event.isComposing || event.keyCode === 229 || view.composing;
    const physical = PHYSICAL_KEYS[event.code] ?? 0;
    const [key, keyString] = keyOf(event.key);
    let text = "";
    if (pressed && !composing && typesText(event)) text = event.key;
    if (pressed && !composing && event.key === "Enter") text = "\r";

    if (pressed) {
      this.fire(view);
      if (isPasteChord(event)) view.pasteKeyAt = this.now();
      if (event.key === "Backspace") view.backspaceKeyAt = this.now();
      if (this.preventsKey(view, event, composing)) event.preventDefault();
      // The field will insert what this press types. It was reported here.
      view.typedByKey = view.field && !composing ? text : "";
    }
    this.core.key(
      view.id,
      (pressed ? KEY_PRESSED : 0) | (composing ? KEY_COMPOSING : 0),
      physical,
      key,
      LOCATIONS[event.location] ?? KeyLocation.Standard,
      modifiersOf(event),
      keyString,
      text,
    );
  }

  /** Whether the browser is kept from acting on a key press itself. */
  preventsKey(view, event, composing) {
    if (composing) return false;
    const combination = event.ctrlKey || event.metaKey || event.altKey;
    if (combination) {
      // Pasting has to go through the browser: its paste event is the one
      // way to read what the user copied elsewhere without asking again.
      if (isPasteChord(event)) return false;
      return (view.actions & DefaultActions.Shortcuts) !== 0;
    }
    if (!(view.actions & DefaultActions.Keys)) return false;
    // While text is entered the field needs its keys: they are what makes
    // input events. Tab would take the focus away, though.
    if (view.field) return event.key === "Tab";
    return true;
  }

  pointer(view, event, phase) {
    this.noteInput(view);
    const kind = POINTER_KINDS[event.pointerType] ?? PointerKind.Mouse;
    const [ratioX, ratioY] = view.ratio();
    const [x, y] = view.position(event);
    const down = phase === PointerPhase.Down || phase === PointerPhase.Up;
    let button = down ? (BUTTONS[event.button] ?? PointerButton.None) : PointerButton.None;
    let tool = PointerTool.Unknown;
    if (kind === PointerKind.Pen) {
      // The eraser end of a pen is button 5, or bit 32 of what is held.
      const eraser = event.button === 5 || (event.buttons & 32) !== 0;
      tool = eraser ? PointerTool.Eraser : PointerTool.Pen;
    }
    if (kind === PointerKind.Touch && down) button = PointerButton.Primary;
    // A locked cursor does not move: its movement is all there is.
    const locked = view.locked && kind === PointerKind.Mouse;
    const measured = locked && phase === PointerPhase.Motion;
    this.core.modifiers(view.id, modifiersOf(event));
    // One array for every event: the core copies what it is handed.
    const values = this.pointerValues;
    values[POINTER.Phase] = phase;
    values[POINTER.Kind] = kind;
    values[POINTER.Id] = event.pointerId;
    values[POINTER.Button] = button;
    values[POINTER.X] = x;
    values[POINTER.Y] = y;
    values[POINTER.Measured] = measured ? 1 : 0;
    values[POINTER.DeltaX] = measured ? event.movementX * ratioX : 0;
    values[POINTER.DeltaY] = measured ? event.movementY * ratioY : 0;
    // A mouse has no pressure to speak of; the browser says 0.5 while a
    // button is held.
    values[POINTER.Pressure] = kind === PointerKind.Mouse ? -1 : event.pressure;
    values[POINTER.TiltX] = event.tiltX ?? 0;
    values[POINTER.TiltY] = event.tiltY ?? 0;
    values[POINTER.Twist] = event.twist ?? 0;
    values[POINTER.Width] = (event.width ?? 0) * ratioX;
    values[POINTER.Height] = (event.height ?? 0) * ratioY;
    values[POINTER.Tool] = tool;
    values[POINTER.TangentialPressure] = event.tangentialPressure ?? 0;
    this.core.pointer(view.id, values);
    if (measured && (event.movementX !== 0 || event.movementY !== 0)) {
      this.core.rawMotion(event.movementX * ratioX, event.movementY * ratioY);
    }
  }

  // -- text ---------------------------------------------------------------------

  addField(view) {
    const field = this.document.createElement("textarea");
    field.setAttribute("aria-hidden", "true");
    field.setAttribute("autocomplete", "off");
    field.setAttribute("autocorrect", "off");
    field.setAttribute("autocapitalize", "off");
    field.spellcheck = false;
    field.tabIndex = -1;
    field.rows = 1;
    Object.assign(field.style, {
      position: "fixed",
      opacity: "0",
      padding: "0",
      border: "0",
      margin: "0",
      outline: "none",
      resize: "none",
      overflow: "hidden",
      background: "transparent",
      color: "transparent",
      caretColor: "transparent",
      pointerEvents: "none",
      whiteSpace: "pre",
      // Smaller text makes iOS zoom into the field.
      fontSize: "16px",
      zIndex: "2147483647",
    });
    view.field = field;
    (view.canvas.parentNode ?? this.document.body).appendChild(field);

    const signal = view.listeners.signal;
    const on = (type, handler) => field.addEventListener(type, handler, { signal });
    on("keydown", (event) => this.key(view, event, true));
    on("keyup", (event) => this.key(view, event, false));
    on("focus", (event) => this.focusChanged(view, event));
    on("blur", (event) => this.focusChanged(view, event));
    on("compositionstart", () => {
      view.composing = true;
      view.typedByKey = "";
    });
    on("compositionupdate", (event) => {
      this.noteInput(view);
      const text = event.data ?? "";
      const end = utf8Length(text);
      this.core.composition(view.id, text, end, end);
    });
    on("compositionend", (event) => {
      this.noteInput(view);
      view.composing = false;
      const text = event.data ?? "";
      this.core.composition(view.id, "", -1, -1);
      if (text !== "") this.core.commit(view.id, text);
      view.field.value = "";
      // Some browsers follow up with an input event for what was composed.
      view.composed = true;
      this.window.setTimeout(() => (view.composed = false), 0);
    });
    on("beforeinput", (event) => this.beforeInput(view, event));
    on("input", () => {
      // Whatever got into the field has been reported: it stays empty, so
      // that there is nothing in it for the next input to be relative to.
      if (!view.composing) field.value = "";
    });
    for (const type of ["paste", "copy", "cut"]) {
      on(type, (event) => this.clipboardEvent(view, event));
    }
  }

  removeField(view) {
    if (!view.field) return;
    view.field.remove();
    view.field = null;
    view.composing = false;
    view.typedByKey = "";
  }

  /** Puts the field where the caret is, so the input method opens next to it. */
  placeField(view) {
    const field = view.field;
    const input = view.textInput;
    if (!field || !input) return;
    const [ratioX, ratioY, box] = view.ratio();
    const height = Math.max(1, input.height / ratioY);
    Object.assign(field.style, {
      left: `${box.left + input.x / ratioX}px`,
      top: `${box.top + input.y / ratioY}px`,
      width: "1px",
      height: `${height}px`,
      lineHeight: `${height}px`,
    });
  }

  beforeInput(view, event) {
    if (view.composing || event.isComposing || view.composed) return;
    this.noteInput(view);
    const data = event.data ?? "";
    switch (event.inputType) {
      case "insertText":
      case "insertReplacementText":
        if (data === "") return;
        if (data === view.typedByKey) {
          // The key press that types this said so already.
          view.typedByKey = "";
          return;
        }
        this.core.commit(view.id, data);
        return;
      case "insertLineBreak":
      case "insertParagraph":
        if (view.typedByKey === "\r") view.typedByKey = "";
        else this.typeKey(view, "Enter", "\r");
        return;
      case "deleteContentBackward":
        // A real Backspace was reported as a key. An on-screen keyboard
        // presses none, and the guest is owed one.
        if (this.now() - view.backspaceKeyAt > KEY_INPUT_WINDOW_MS) {
          this.typeKey(view, "Backspace", "");
        }
        return;
      default:
        // Pastes are the clipboard's business; the rest has no meaning for
        // a field that is always empty.
        return;
    }
  }

  /** A key an on-screen keyboard stands for, pressed and released. */
  typeKey(view, name, text) {
    const physical = PHYSICAL_KEYS[name] ?? 0;
    const [key] = keyOf(name);
    for (const pressed of [true, false]) {
      this.core.key(
        view.id,
        pressed ? KEY_PRESSED : 0,
        physical,
        key,
        KeyLocation.Standard,
        0,
        "",
        pressed ? text : "",
      );
    }
  }

  // -- the clipboard ------------------------------------------------------------

  clipboardEvent(view, event) {
    if (event.type !== "paste") {
      // Copying is the guest's doing, when it hears the shortcut. What the
      // browser would copy (the field's selection) is nothing.
      return;
    }
    const text = event.clipboardData?.getData("text/plain") ?? "";
    event.preventDefault();
    this.noteInput(view);
    this.pasted = { text, at: this.now() };
    const waiting = this.readers;
    this.readers = [];
    if (waiting.length > 0) {
      for (const reader of waiting) reader.answer(text);
    } else {
      // Nobody asked yet: the guest hears that the clipboard has something
      // new, and finds it there when it reads.
      this.core.clipboardFetched(0, text, true);
    }
    // A paste the user made with no key (the browser's menu) is text for a
    // field that is being typed in.
    const byKey = this.now() - view.pasteKeyAt < PASTE_KEY_WINDOW_MS;
    if (!byKey && view.field && text !== "") this.core.commit(view.id, text);
  }

  /** The old way of copying, for browsers that refuse the new one here. */
  copyByCommand(text) {
    const document = this.document;
    if (typeof document.execCommand !== "function") return false;
    const active = document.activeElement;
    const holder = document.createElement("textarea");
    holder.value = text;
    Object.assign(holder.style, { position: "fixed", opacity: "0", pointerEvents: "none" });
    document.body.appendChild(holder);
    let copied = false;
    try {
      holder.select();
      copied = document.execCommand("copy");
    } catch {
      copied = false;
    }
    holder.remove();
    active?.focus?.({ preventScroll: true });
    return copied;
  }
}

// KeyboardEvent.location -> WGUIKeyLocation.
const LOCATIONS = [KeyLocation.Standard, KeyLocation.Left, KeyLocation.Right, KeyLocation.Numpad];

function fullscreenElement(document) {
  return document.fullscreenElement ?? document.webkitFullscreenElement ?? null;
}

function exitFullscreen(document) {
  if (document.exitFullscreen) return document.exitFullscreen();
  return document.webkitExitFullscreen?.();
}

function canFullscreen(canvas) {
  return (
    typeof canvas.requestFullscreen === "function" ||
    typeof canvas.webkitRequestFullscreen === "function"
  );
}

/** 128 bits nobody can guess, as hex. */
function randomName() {
  const bytes = new Uint8Array(16);
  globalThis.crypto.getRandomValues(bytes);
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

/**
 * Makes the page's half for one sandbox.
 *
 * `core` is told what happens: see the calls of `this.core` above for its
 * functions. `options`:
 *
 * - `resolveCanvas(target)`: the canvas a guest's window is to be, for the
 *   target it named (`""` for the default one), or nothing.
 * - `alias(selector, canvas | undefined)`: makes a canvas known under the
 *   name its window gives out for drawing, and forgets it again.
 * - `title(title, canvas)`: a window was given a title, or (with `""`) the
 *   canvas is no longer a window.
 * - `resizable`: `false` keeps guests from asking the canvas to be another
 *   size; the page's layout alone decides.
 * - `manageCanvasSize`: `false` leaves `canvas.width` and `height` alone.
 * - `eagerRedraw`: `true` answers a window's request to redraw at once,
 *   instead of at the browser's next frame, when the user did something to
 *   the window since its last redraw (a key, the pointer, the wheel, text,
 *   a paste, or moving the focus). That frame is usually most of a frame
 *   away when the request answers what the user did, so it is shown about a
 *   frame sooner. A request already waiting for a frame is answered then
 *   too. Without input since the last redraw, requests keep to the frames:
 *   a guest drawing frame after frame on its own does, however long its
 *   frames take. While the user does something, the host keeps a request
 *   to the frames when the guest made it before waiting for events again
 *   since it read its last redraw. A guest that waits between its frames,
 *   or draws on one thread and waits on another, may then be answered at
 *   once after each input. A window nobody can see waits for the frames.
 *   Off by default.
 * - `document`: the document, when it is not `globalThis.document`.
 */
export function createGuiPage(core, options) {
  return new GuiPage(core, options);
}

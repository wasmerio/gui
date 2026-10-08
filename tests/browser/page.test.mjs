// The page half of wasmer-gui (js/gui_page.js) in a real browser, reporting
// to a core that only writes down what it is told. See README.md.

import assert from "node:assert/strict";
import { existsSync } from "node:fs";
import { readFile } from "node:fs/promises";
import { createServer } from "node:http";
import { createRequire } from "node:module";
import { extname, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

const root = resolve(fileURLToPath(new URL("../../", import.meta.url)));

/**
 * Playwright, from this directory's own `npm install`, or from a checkout
 * this one sits in (the Wasmer SDK has it for its own browser tests).
 */
function playwright() {
  const places = [
    ["playwright-core", import.meta.url],
    ["playwright-core", new URL("../../../webgpu/tests/browser/", import.meta.url)],
    ["playwright", new URL("../../../../js/", import.meta.url)],
  ];
  for (const [name, from] of places) {
    try {
      return createRequire(from)(name);
    } catch {
      // Not there: the next place.
    }
  }
  return null;
}

const { chromium } = playwright() ?? {};
const skip = !chromium && "run `npm install` in tests/browser first";

const PAGE = `<!doctype html>
<title>The page half</title>
<style>
  body { margin: 0; height: 3000px; }
  #view { display: block; margin: 20px; width: 300px; height: 200px; }
</style>
<canvas id="view"></canvas>
<canvas id="other" style="display: block; width: 100px; height: 50px"></canvas>
<script type="module">
  import { createGuiPage } from "/js/gui_page.js";
  import * as abi from "/js/gui_abi.gen.js";

  // A core that writes down what it is told, as [name, ...arguments].
  const calls = [];
  const core = new Proxy({}, {
    get: (_, name) => (...args) => {
      calls.push([name, ...args.map((arg) => (ArrayBuffer.isView(arg) ? Array.from(arg) : arg))]);
    },
  });
  const aliases = new Map();
  const titles = [];
  const canvases = { "": document.querySelector("#view"), other: document.querySelector("#other") };
  globalThis.harness = {
    abi,
    calls,
    aliases,
    titles,
    open: (options = {}) => {
      globalThis.gui = createGuiPage(core, {
        resolveCanvas: (target) => canvases[target] ?? null,
        alias: (selector, canvas) => (canvas ? aliases.set(selector, canvas.id) : aliases.delete(selector)),
        title: (title, canvas) => titles.push([title, canvas.id]),
        ...options,
      });
      globalThis.gui.start();
      return globalThis.gui;
    },
    /** The calls of one name since the last look, without the view's number. */
    take: (name) => {
      const found = calls.filter((call) => call[0] === name).map((call) => call.slice(1));
      for (let i = calls.length - 1; i >= 0; i--) if (calls[i][0] === name) calls.splice(i, 1);
      return found;
    },
  };
</script>`;

async function openPage(context) {
  const server = createServer(async (request, response) => {
    const pathname = new URL(request.url, "http://localhost").pathname;
    if (pathname === "/") {
      response.setHeader("Content-Type", "text/html");
      response.end(PAGE);
      return;
    }
    if (pathname === "/favicon.ico") {
      response.writeHead(204).end();
      return;
    }
    const file = resolve(root, `.${decodeURIComponent(pathname)}`);
    if (!file.startsWith(`${root}${sep}`) || !existsSync(file)) {
      response.writeHead(404).end();
      return;
    }
    response.setHeader("Content-Type", extname(file) === ".js" ? "text/javascript" : "text/plain");
    response.end(await readFile(file));
  });
  await new Promise((done) => server.listen(0, "127.0.0.1", done));
  const channel = process.env.WGUI_BROWSER_CHANNEL ?? "chrome";
  let browser;
  context.after(async () => {
    await browser?.close();
    server.closeAllConnections();
    await new Promise((done) => server.close(done));
  });
  try {
    browser = await chromium.launch({ headless: true, ...(channel === "bundled" ? {} : { channel }) });
  } catch (error) {
    return { unavailable: `the browser (${channel}) could not be started: ${error.message.split("\n")[0]}` };
  }
  const page = await browser.newPage({ viewport: { width: 800, height: 600 }, deviceScaleFactor: 2 });
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("console", (message) => {
    if (message.type() === "error") errors.push(`console: ${message.text()}`);
  });
  await page.goto(`http://127.0.0.1:${server.address().port}/`);
  await page.waitForFunction(() => globalThis.harness !== undefined);
  return { page, errors };
}

/** Resolves once `predicate` (run in the page) holds. */
function until(page, predicate, argument) {
  return page.waitForFunction(predicate, argument, { timeout: 10_000, polling: 20 });
}

test("a window is a canvas, sized in the pixels it shows, and given back as it was", { skip }, async (context) => {
  const { page, errors, unavailable } = await openPage(context);
  if (unavailable) return context.skip(unavailable);

  const result = await page.evaluate(() => {
    const { abi, aliases, titles } = harness;
    const gui = harness.open();
    const view = document.querySelector("#view");
    const made = gui.createWindow(1, { target: "", title: "One", width: 0, height: 0, logical: false });
    const during = {
      size: [view.width, view.height],
      tabIndex: view.tabIndex,
      label: view.getAttribute("aria-label"),
      alias: aliases.get(made.selector),
      titles: [...titles],
    };
    // The same canvas is not two windows, and a name nobody gave is none.
    const again = gui.createWindow(2, { target: "" }).status;
    const unknown = gui.createWindow(3, { target: "nowhere" }).status;
    const second = gui.createWindow(4, { target: "other" }).status;
    gui.destroyWindow(1);
    const after = {
      tabIndex: view.getAttribute("tabindex"),
      label: view.getAttribute("aria-label"),
      aliases: aliases.size,
      title: titles.at(-1),
    };
    const reopened = gui.createWindow(5, { target: "" }).status;
    gui.dispose();
    return { made, during, again, unknown, second, after, reopened, left: aliases.size, status: abi.Status };
  });

  const { Success, Busy, NotFound } = result.status;
  assert.equal(result.made.status, Success);
  assert.match(result.made.selector, /^wgui:[0-9a-f]{32}$/);
  // 300 by 200 CSS pixels on a display with two pixels for each.
  assert.deepEqual([result.made.width, result.made.height, result.made.scale], [600, 400, 2]);
  assert.deepEqual(result.during, {
    size: [600, 400],
    tabIndex: 0,
    label: "One",
    alias: "view",
    titles: [["One", "view"]],
  });
  assert.deepEqual([result.again, result.unknown, result.second], [Busy, NotFound, Success]);
  assert.deepEqual(result.after, { tabIndex: null, label: null, aliases: 1, title: ["", "view"] });
  assert.equal(result.reopened, Success);
  assert.equal(result.left, 0, "disposing gives every canvas back");
  assert.deepEqual(errors, []);
});

test("keys, the mouse and the wheel are told in the window's own pixels", { skip }, async (context) => {
  const { page, errors, unavailable } = await openPage(context);
  if (unavailable) return context.skip(unavailable);

  await page.evaluate(() => {
    harness.open().createWindow(1, { target: "" });
    harness.calls.length = 0;
    // What the browser itself says the wheel moved, in CSS pixels.
    globalThis.wheeled = [];
    document.querySelector("#view").addEventListener("wheel", (event) => wheeled.push(event.deltaY));
  });
  // (20, 20) is the canvas's corner: this is (100, 50) CSS pixels inside,
  // twice that in the pixels of the window.
  await page.mouse.move(120, 70);
  await page.mouse.down({ button: "right" });
  await page.mouse.up({ button: "right" });
  await page.keyboard.press("ShiftRight");
  await page.keyboard.press("NumpadAdd");
  await page.keyboard.press("ArrowLeft");
  await page.mouse.wheel(0, 30);
  await until(page, () => harness.calls.some((call) => call[0] === "wheel"));

  const seen = await page.evaluate(() => {
    const { abi, take } = harness;
    const pointers = take("pointer").map(([view, values]) => [view, ...values.slice(0, 6)]);
    return {
      pointers,
      keys: take("key").map(([view, flags, physical, key, location, modifiers, keyString, text]) => ({
        pressed: (flags & 1) !== 0, physical, key, location, modifiers, keyString, text,
      })),
      wheel: take("wheel"),
      wheeled,
      focused: take("focused"),
      abi: {
        phase: { Enter: 0, Motion: 2, Down: 3, Up: 4 },
        Mouse: abi.PointerKind.Mouse,
        Secondary: abi.PointerButton.Secondary,
        ShiftRight: abi.PHYSICAL_KEYS.ShiftRight,
        NumpadAdd: abi.PHYSICAL_KEYS.NumpadAdd,
        ArrowLeft: abi.PHYSICAL_KEYS.ArrowLeft,
        Shift: abi.NAMED_KEYS.Shift,
        ArrowLeftKey: abi.NAMED_KEYS.ArrowLeft,
        Right: abi.KeyLocation.Right,
        Numpad: abi.KeyLocation.Numpad,
        ShiftBit: abi.Modifiers.Shift,
        NumLock: abi.Modifiers.NumLock,
        Pixel: abi.WheelUnit.Pixel,
      },
    };
  });

  const { abi } = seen;
  // [view, phase, kind, pointer, button, x, y]
  assert.deepEqual(seen.pointers, [
    [1, abi.phase.Enter, abi.Mouse, 1, 0, 200, 100],
    [1, abi.phase.Motion, abi.Mouse, 1, 0, 200, 100],
    [1, abi.phase.Down, abi.Mouse, 1, abi.Secondary, 200, 100],
    [1, abi.phase.Up, abi.Mouse, 1, abi.Secondary, 200, 100],
  ]);
  assert.deepEqual(seen.focused, [[1, true]], "a press gives the canvas the keyboard");
  const locks = (modifiers) => modifiers & ~abi.NumLock;
  // (Where a key is on the keyboard is checked on its press: the driver of
  // this test reports releases of the numeric pad from somewhere else.)
  const keys = seen.keys.map((key) => ({
    ...key,
    modifiers: locks(key.modifiers),
    location: key.pressed ? key.location : undefined,
  }));
  assert.deepEqual(keys, [
    { pressed: true, physical: abi.ShiftRight, key: abi.Shift, location: abi.Right, modifiers: abi.ShiftBit, keyString: "", text: "" },
    { pressed: false, physical: abi.ShiftRight, key: abi.Shift, location: undefined, modifiers: 0, keyString: "", text: "" },
    { pressed: true, physical: abi.NumpadAdd, key: 0x2b, location: abi.Numpad, modifiers: 0, keyString: "+", text: "+" },
    { pressed: false, physical: abi.NumpadAdd, key: 0x2b, location: undefined, modifiers: 0, keyString: "+", text: "" },
    { pressed: true, physical: abi.ArrowLeft, key: abi.ArrowLeftKey, location: 0, modifiers: 0, keyString: "", text: "" },
    { pressed: false, physical: abi.ArrowLeft, key: abi.ArrowLeftKey, location: undefined, modifiers: 0, keyString: "", text: "" },
  ]);
  // [view, unit, phase, dx, dy, x, y]: CSS pixels become the window's.
  assert.equal(seen.wheeled.length, 1);
  assert.ok(seen.wheeled[0] > 0);
  assert.deepEqual(seen.wheel, [[1, abi.Pixel, 0, 0, seen.wheeled[0] * 2, 200, 100]]);
  assert.deepEqual(errors, []);
});

test("what the browser would do itself is prevented as the guest said beforehand", { skip }, async (context) => {
  const { page, errors, unavailable } = await openPage(context);
  if (unavailable) return context.skip(unavailable);

  await page.evaluate(() => {
    harness.open().createWindow(1, { target: "" });
    // What the browser still did, by event type.
    globalThis.allowed = [];
    for (const type of ["keydown", "wheel", "contextmenu"]) {
      document.addEventListener(type, (event) => {
        if (!event.defaultPrevented) allowed.push(`${type}:${event.key ?? ""}`);
      });
    }
  });
  await page.mouse.click(120, 70);
  const scrolled = () => page.evaluate(() => window.scrollY);

  // By default: the space bar does not scroll the page, the wheel neither,
  // and there is no menu; a shortcut is still the browser's.
  await page.keyboard.press("Space");
  await page.mouse.wheel(0, 200);
  await page.mouse.click(120, 70, { button: "right" });
  await page.keyboard.press("Control+f");
  await page.waitForTimeout(100);
  assert.equal(await scrolled(), 0);
  assert.deepEqual(await page.evaluate(() => allowed.splice(0)), ["keydown:Control", "keydown:f"]);

  // A guest that prevents nothing: all of it is the browser's again.
  await page.evaluate(() => gui.setDefaultActions(1, 0));
  await page.keyboard.press("Space");
  await page.mouse.wheel(0, 200);
  await until(page, () => window.scrollY > 0);
  // The page may still be on its way: browsers scroll smoothly, unless they
  // are saving energy, and then it is there at once. Either way the canvas
  // goes back under the mouse before the next click, which is meant for it.
  await page.evaluate(async () => {
    for (let last = -1; last !== window.scrollY; ) {
      last = window.scrollY;
      await new Promise((done) => setTimeout(done, 100));
    }
    window.scrollTo(0, 0);
  });
  await page.mouse.click(120, 70, { button: "right" });
  const now = await page.evaluate(() => allowed.splice(0));
  assert.ok(now.includes("keydown: ") && now.includes("wheel:") && now.includes("contextmenu:"), now.join());

  // And one that takes shortcuts too, as far as a page can.
  await page.evaluate(() => gui.setDefaultActions(1, harness.abi.DefaultActions.Default | harness.abi.DefaultActions.Shortcuts));
  await page.keyboard.press("Control+f");
  assert.deepEqual(await page.evaluate(() => allowed.splice(0)), []);
  assert.deepEqual(errors, []);
});

test("a cursor is the canvas's, in the names CSS has", { skip }, async (context) => {
  const { page, errors, unavailable } = await openPage(context);
  if (unavailable) return context.skip(unavailable);

  const cursors = await page.evaluate(() => {
    const { abi } = harness;
    const gui = harness.open();
    const view = document.querySelector("#view");
    view.style.cursor = "wait";
    gui.createWindow(1, { target: "" });
    const seen = [];
    gui.setCursorShape(1, abi.CursorShape.NeResize);
    seen.push(view.style.cursor);
    gui.setCursorShape(1, abi.CursorShape.Text);
    seen.push(view.style.cursor);
    gui.setCursorVisible(1, false);
    seen.push(view.style.cursor);
    gui.setCursorVisible(1, true);
    seen.push(view.style.cursor);
    // Four pixels, made for a display twice as dense: shown half the size.
    const rgba = new Uint8Array(4 * 4 * 4).fill(255);
    gui.setCustomCursor(1, rgba, 4, 4, 2, 2, 2);
    seen.push(view.style.cursor.replace(/data:[^")]*/, "data:"));
    gui.setCustomCursor(1, null, 0, 0, 0, 0, 1);
    seen.push(view.style.cursor);
    gui.destroyWindow(1);
    seen.push(view.style.cursor);
    return seen;
  });
  assert.deepEqual(cursors, [
    "ne-resize",
    "text",
    "none",
    "text",
    'url("data:") 1 1, auto',
    "",
    "wait",
  ]);
  assert.deepEqual(errors, []);
});

test("fullscreen and the cursor lock wait for something the user does", { skip }, async (context) => {
  const { page, errors, unavailable } = await openPage(context);
  if (unavailable) return context.skip(unavailable);

  // A browser lets a page go fullscreen once for each thing the user does.
  // The test's own way into the page counts as one, so the first request is
  // let through, and uses it up.
  const early = await page.evaluate(async () => {
    const gui = harness.open();
    gui.createWindow(1, { target: "" });
    harness.calls.length = 0;
    // A request is over when the core has been told, which is later than
    // the document changing.
    const answered = (count) =>
      new Promise((done) => {
        const check = () =>
          harness.calls.filter((call) => call[0] === "requestDone").length >= count
            ? done()
            : setTimeout(check, 10);
        check();
      });
    gui.setFullscreen(1, true, 5, true);
    await answered(1);
    gui.setFullscreen(1, false, 6, true);
    await answered(2);
    const used = harness.take("requestDone");
    harness.take("fullscreen");

    // Now nobody has done anything the browser would count. A guest that
    // does not want to wait is told so at once; one that does is not told
    // anything yet.
    const active = navigator.userActivation?.isActive;
    gui.setFullscreen(1, true, 7, false);
    const refused = harness.take("requestDone");
    gui.setFullscreen(1, true, 8, true);
    await new Promise((done) => setTimeout(done, 50));
    return {
      used,
      active,
      refused,
      waiting: harness.take("requestDone"),
      fullscreen: document.fullscreenElement !== null,
      status: harness.abi.Status,
    };
  });
  const { Success, Denied, Unsupported } = early.status;
  assert.deepEqual(early.used, [[5, Success], [6, Success]]);
  assert.equal(early.active, false);
  assert.deepEqual(early.refused, [[7, Denied]]);
  assert.deepEqual(early.waiting, []);
  assert.equal(early.fullscreen, false);

  // The user's next press is what the browser was waiting for.
  await page.mouse.click(120, 70);
  await until(page, () => document.fullscreenElement === document.querySelector("#view"));
  await until(page, () => harness.calls.some((call) => call[0] === "requestDone"));
  assert.deepEqual(await page.evaluate(() => [harness.take("fullscreen"), harness.take("requestDone")]), [
    [[1, true]],
    [[8, Success]],
  ]);

  // A request that is still waiting when the next one comes is answered:
  // cancelled.
  const replaced = await page.evaluate(async () => {
    gui.setFullscreen(1, false, 9, true);
    while (!harness.calls.some((call) => call[0] === "requestDone")) {
      await new Promise((done) => setTimeout(done, 10));
    }
    const left = harness.take("requestDone");
    gui.setFullscreen(1, true, 10, true);
    gui.setFullscreen(1, true, 11, true);
    const cancelled = harness.take("requestDone");
    // A cursor cannot be confined in a browser, and is not locked instead.
    gui.setCursorGrab(1, harness.abi.CursorGrab.Confined, 12);
    return { left, cancelled, confined: harness.take("requestDone"), Cancelled: harness.abi.Status.Cancelled };
  });
  assert.deepEqual(replaced.left, [[9, Success]]);
  assert.deepEqual(replaced.cancelled, [[10, replaced.Cancelled]]);
  assert.deepEqual(replaced.confined, [[12, Unsupported]]);
  assert.deepEqual(errors, []);
});

test("a canvas nobody can see says so, and asks to be drawn when it can be", { skip }, async (context) => {
  const { page, errors, unavailable } = await openPage(context);
  if (unavailable) return context.skip(unavailable);

  await page.evaluate(() => {
    harness.open().createWindow(1, { target: "" });
    harness.calls.length = 0;
  });
  const OCCLUDED = 0x40000;
  await page.evaluate(() => window.scrollTo(0, 2000));
  await until(page, () => harness.calls.some((call) => call[0] === "flags"));
  assert.deepEqual(await page.evaluate(() => harness.take("flags")), [[1, OCCLUDED, OCCLUDED]]);
  await page.evaluate(() => {
    harness.take("redraw");
    window.scrollTo(0, 0);
  });
  await until(page, () => harness.calls.some((call) => call[0] === "redraw"));
  assert.deepEqual(await page.evaluate(() => [harness.take("flags"), harness.take("redraw")]), [
    [[1, OCCLUDED, 0]],
    [[1]],
  ]);

  // A redraw the guest asks for comes with the browser's next frame, once.
  await page.evaluate(() => {
    gui.requestRedraw(1);
    gui.requestRedraw(1);
  });
  await until(page, () => harness.calls.some((call) => call[0] === "redraw"));
  await page.waitForTimeout(50);
  assert.deepEqual(await page.evaluate(() => harness.take("redraw")), [[1]]);
  assert.deepEqual(errors, []);
});

test("the page decides the size when it says so, and the guest may ask otherwise", { skip }, async (context) => {
  const { page, errors, unavailable } = await openPage(context);
  if (unavailable) return context.skip(unavailable);

  const fixed = await page.evaluate(() => {
    const gui = harness.open({ resizable: false });
    const view = document.querySelector("#view");
    const made = gui.createWindow(1, { target: "", width: 640, height: 480, logical: true });
    gui.setSize(1, 100, 100);
    const size = [made.width, made.height, view.style.width];
    gui.dispose();
    return size;
  });
  assert.deepEqual(fixed, [600, 400, ""]);

  await page.evaluate(() => {
    harness.open().createWindow(1, { target: "", width: 400, height: 100, logical: true });
    harness.calls.length = 0;
    // In the pixels of the window: 250 by 150 CSS pixels here.
    gui.setSize(1, 500, 300);
  });
  await until(page, () => harness.calls.some((call) => call[0] === "resized"));
  const asked = await page.evaluate(() => {
    const view = document.querySelector("#view");
    return [harness.take("resized").at(-1), view.width, view.height];
  });
  assert.deepEqual(asked, [[1, 500, 300, 2], 500, 300]);
  assert.deepEqual(errors, []);
});

test("a frame the guest drew is the canvas, and a part of one only that part", { skip }, async (context) => {
  const { page, errors, unavailable } = await openPage(context);
  if (unavailable) return context.skip(unavailable);

  const seen = await page.evaluate(() => {
    const gui = harness.open();
    const view = document.querySelector("#view");
    gui.createWindow(1, { target: "" });
    const at = (x, y) => [...view.getContext("2d").getImageData(x, y, 1, 1).data];
    const solid = (width, height, red, green, blue) => {
      const rgba = new Uint8ClampedArray(width * height * 4);
      for (let i = 0; i < rgba.length; i += 4) rgba.set([red, green, blue, 255], i);
      return rgba;
    };
    const none = new Int32Array(0);
    const seen = {};
    // A frame smaller than the canvas is shown at: the canvas has the
    // frame's pixels, and the page's layout stretches it.
    gui.presentPixels(1, solid(30, 20, 255, 0, 0), 30, 20, none);
    seen.first = { size: [view.width, view.height], shown: view.getBoundingClientRect().width, pixel: at(29, 19) };
    // Only the rectangle that changed is drawn over.
    gui.presentPixels(1, solid(30, 20, 0, 255, 0), 30, 20, new Int32Array([10, 5, 4, 3]));
    seen.part = { inside: at(10, 5), last: at(13, 7), beside: at(14, 5), below: at(10, 8), before: at(9, 5) };
    // A frame of another size replaces everything, whatever it says changed.
    gui.presentPixels(1, solid(8, 8, 0, 0, 255), 8, 8, new Int32Array([0, 0, 1, 1]));
    seen.resized = { size: [view.width, view.height], far: at(7, 7) };
    // The layout changing does not clear what is shown.
    view.style.width = "200px";
    return new Promise((done) =>
      requestAnimationFrame(() =>
        requestAnimationFrame(() => {
          seen.later = { size: [view.width, view.height], far: at(7, 7) };
          done(seen);
        }),
      ),
    );
  });
  const red = [255, 0, 0, 255];
  const green = [0, 255, 0, 255];
  const blue = [0, 0, 255, 255];
  assert.deepEqual(seen.first, { size: [30, 20], shown: 300, pixel: red });
  assert.deepEqual(seen.part, { inside: green, last: green, beside: red, below: red, before: red });
  assert.deepEqual(seen.resized, { size: [8, 8], far: blue });
  assert.deepEqual(seen.later, { size: [8, 8], far: blue });
  assert.deepEqual(errors, []);
});

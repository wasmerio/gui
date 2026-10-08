# The page half, on its own

`js/gui_page.js` is everything wasmer-gui does to a page: it makes canvases
windows, listens to the user on them and carries out what guests ask. These
tests run it in a real browser without the rest: the "core" it reports to is
an object that writes down every call, and the user is Playwright, with real
key, mouse and wheel input.

```bash
npm install          # playwright-core; the browser is the installed Chrome
npm test
```

`WGUI_BROWSER_CHANNEL` picks another Playwright channel (`msedge`,
`chrome-beta`), or `bundled` for Playwright's own Chromium.

What is tested here is what needs no guest: which canvas a window gets and
what is left of it afterwards, what the browser is kept from doing itself,
cursors, and requests that have to wait for something the user does. The
whole path, from a key press to a guest reading it in a worker, is tested in
the Wasmer SDK (`js/tests/browser-gui.test.mjs`).

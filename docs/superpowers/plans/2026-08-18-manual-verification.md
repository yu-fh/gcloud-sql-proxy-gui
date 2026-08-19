# Manual verification checklist

Everything below needs a real desktop session, the corporate VPN, and working
credentials. None of it could be automated: the agents that built this app had
no assistive-access or screen-recording permission, so **nobody has yet seen the
tray menu or the windows render**. The UI logic was verified by driving the page
in a headless browser with the backend stubbed, which caught several real layout
bugs but cannot exercise the Tauri IPC boundary, the CSP, or AppKit.

Prerequisites:

```bash
gcloud auth application-default login   # if it has been a while
# connect to the corporate VPN
npx tauri dev                           # from the repo root
```

## A note on checking for stray proxies

`pgrep -f cloud-sql-proxy` **gives false positives**: the app's own binary is
named `fh-cloud-sql-proxy-gui`, which contains that substring. Use:

```bash
pgrep -fl cloud-sql-proxy | grep -v fh-cloud-sql-proxy-gui
```

No output means no proxy children.

## 1. The app is a menu bar app

- [ ] A tray icon appears in the menu bar.
- [ ] No Dock icon, and the app does not appear in Cmd-Tab.
- [ ] Clicking the tray icon opens the menu on the **left** click, not only the
      right.

## 2. The menu reads correctly

- [ ] The status line says "Nothing running".
- [ ] dev, stg, and prd are listed, each showing `(15432/15433)`.
- [ ] prd is marked with `⚠`.

## 3. Connection names refresh

- [ ] **Profiles…** opens a window.
- [ ] The window looks like a macOS settings panel — grouped inset lists,
      hairline separators, system-sized text. If it reads as a web page, say so;
      that was the explicit design goal and it has never been seen rendered.
- [ ] Traffic-light buttons do not overlap the content. The title bar uses
      `TitleBarStyle::Overlay` with a 38px allowance in CSS that has never been
      checked against the real window.
- [ ] Selection and the default button use your **system accent colour**. If
      they are blue regardless of your accent, `AccentColor` did not resolve in
      WKWebView — a documented unknown, and a cosmetic-only failure.
- [ ] Each seeded profile shows the "no connection names yet" notice.
- [ ] **Refresh Connection Names** proposes changes and writes nothing until you
      approve.
- [ ] After approving, the connection names are populated:

```bash
cat "$HOME/Library/Application Support/ai.firsthand.fh-cloud-sql-proxy-gui/profiles.json"
```

## 4. Starting and connecting

- [ ] Click `dev` in the tray. It shows `(starting…)` briefly, then the status
      line reads `dev — 127.0.0.1:15432, :15433`.
- [ ] A real connection works:

```bash
psql -h 127.0.0.1 -p 15432 -U "$(gcloud config get-value account)" -d fh_ui_dev -c 'select 1'
```

Leave the password empty if prompted — the proxy injects the token.

## 5. Exclusive by default

- [ ] With dev running, click `stg`. dev stops, stg starts, and only stg appears
      in the status line.
- [ ] Exactly one proxy process is running:

```bash
pgrep -fl cloud-sql-proxy | grep -v fh-cloud-sql-proxy-gui
```

This is the path where a phantom "port in use" would show up: the kernel does
not always release a just-killed child's listener immediately, so
`start_profile` re-polls preflight after stopping something. If you see a
spurious port-in-use error here, that mitigation is insufficient.

## 6. Production is guarded

- [ ] Clicking `prd` asks for confirmation before starting, naming the
      environment and its ports.
- [ ] Cancelling does not start it.

## 7. The three diagnoses

**Port in use** — stop everything in the app, then:

```bash
python3 -m http.server 15432
```

- [ ] Clicking `dev` reports that port **15432** is in use. Specifically check
      it does not say "Port 0" or "Port 23" — extraction used to pick digits out
      of the log timestamp, and that bug is the reason this check is here.

**Off VPN** — disconnect the VPN, then click `dev`.

- [ ] The failure names the VPN rather than showing a raw timeout.

**Expired credentials** — hard to force safely; skip unless it happens
naturally. If it does:

- [ ] The message names `gcloud auth application-default login` and the copy
      button works. Clipboard access under the app's CSP is unverified; if the
      button fails it should show a visible error rather than doing nothing.

## 8. Profiles are user-defined

- [ ] Add an environment with **+**. It appears in the tray menu **without
      restarting** — within about a second.
- [ ] Rename it. The tray label updates.
- [ ] Delete it. It disappears from the tray menu.
- [ ] Delete an environment while it is running: it stops first, and no proxy
      process is left behind.

## 9. Quitting cleans up

- [ ] Start dev, confirm a proxy process exists, then **Quit** from the menu.
- [ ] No proxy process remains:

```bash
pgrep -fl cloud-sql-proxy | grep -v fh-cloud-sql-proxy-gui
```

A leaked child would keep holding 15432 and break the next launch, so this is
the single most important check on the list.

## 10. Logs

- [ ] **Logs…** opens a window showing the proxy's output, including the
      "ready for new connections" line.
- [ ] The log pane fills the window rather than collapsing to a strip.

## Known unknowns

Three things could not be verified without a desktop session, all cosmetic or
recoverable:

1. Overlay title-bar height versus the 38px CSS allowance.
2. Whether `AccentColor` resolves in WKWebView (falls back to blue if not).
3. Whether `navigator.clipboard.writeText` is permitted under the app's CSP.

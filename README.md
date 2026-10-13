# xdg-desktop-portal-niri

A custom `org.freedesktop.impl.portal.ScreenCast` backend for [niri](https://github.com/YaLTeR/niri).

## Why this exists

On niri, `ScreenCast` can be routed to either `xdg-desktop-portal-wlr` or
`xdg-desktop-portal-gnome`:

- **`-wlr`** has an unreleased 0.8.x regression that stalls PipeWire capture after
  one frame ([emersion/xdg-desktop-portal-wlr#369](https://github.com/emersion/xdg-desktop-portal-wlr/issues/369)).
- **`-gnome`** works reliably, but it's a compiled GTK dialog with no external
  chooser hook, so there's no way to plug in a fast, scriptable picker. Worse,
  webrtc-based clients like Slack issue a burst of `SelectSources` calls per
  share attempt (with different source-type masks), and without a cache each
  one pops its own native picker dialog.

This backend drives the actual capture through niri's **native**
`org.gnome.Mutter.ScreenCast` D-Bus interface — the same reliable path `-gnome`
uses internally — while handling source selection itself via a cached
[fuzzel](https://codeberg.org/dnkl/fuzzel) prompt. Bonus: since niri's
`RecordWindow` takes a window ID straight from niri's own IPC, this backend
supports real per-window capture, not just monitors.

## How it works

```
xdg-desktop-portal (frontend)
        │  org.freedesktop.impl.portal.ScreenCast
        ▼
xdg-desktop-portal-niri (this project)
        │  org.gnome.Mutter.ScreenCast
        ▼
niri
```

- **`src/portal.rs`** — implements `org.freedesktop.impl.portal.ScreenCast`
  (`CreateSession` / `SelectSources` / `Start`) and the companion
  `org.freedesktop.impl.portal.Session` object, exactly mirroring the backend
  contract the central `xdg-desktop-portal` dispatcher expects.
- **`src/mutter.rs`** — a thin `zbus` proxy for niri's
  `org.gnome.Mutter.ScreenCast` (+ `.Session` / `.Stream`): creates a Mutter
  session, calls `RecordMonitor`/`RecordWindow`, and waits for the resulting
  stream's `PipeWireStreamAdded(node_id)` signal. `OpenPipeWireRemote` itself
  is handled entirely by the central `xdg-desktop-portal` dispatcher using
  that `node_id` — this backend never touches PipeWire directly.
- **`src/chooser.rs`** — builds a combined list of monitors (from
  `niri msg outputs`) and windows (from `niri msg windows`, via the
  [`niri-ipc`](https://docs.rs/niri-ipc) crate talking to the niri socket
  directly) and pipes it through `fuzzel --dmenu`. Caches the pick for 10s so
  a burst of rapid `SelectSources` calls from one share attempt only prompts
  once.

## Requirements

- niri, with its D-Bus `org.gnome.Mutter.ScreenCast` interface available on
  the session bus (true by default on recent niri).
- `fuzzel` on `$PATH`.
- A Rust toolchain — see **Building** below if you don't want to install one
  on the host.

## Building

This repo ships a [devbox](https://www.jetify.com/devbox) environment
(`devbox.json`) that provides `rustup`, so no Rust install is needed on the
host at all:

```sh
make build
```

This idempotently sets a default `stable` toolchain inside the devbox
environment (first run only) and runs `cargo build --release` through
`devbox run`. If you already have a Rust toolchain on your host and don't
want to use devbox, `cargo build --release` works directly too.

## Installing

```sh
make install
```

This builds the binary, then:

- installs the binary to `~/.local/bin/xdg-desktop-portal-niri`
- installs the D-Bus activation file to
  `~/.local/share/dbus-1/services/org.freedesktop.impl.portal.desktop.niri.service`
- installs the systemd user unit to
  `~/.config/systemd/user/xdg-desktop-portal-niri.service` (bus-activated —
  `systemctl --user enable` is neither needed nor applicable)
- installs the portal registry file to
  `~/.local/share/xdg-desktop-portal/portals/niri.portal`
- ensures `~/.config/xdg-desktop-portal/niri-portals.conf` contains
  `org.freedesktop.impl.portal.ScreenCast=niri;` under `[preferred]` (creates
  the file if missing; only appends the line if it isn't already present —
  never touches any of your other entries in that file)
- reloads systemd and restarts both `xdg-desktop-portal-niri` and
  `xdg-desktop-portal` so the new routing takes effect immediately (this will
  interrupt any screen share in progress)

Install paths can be overridden, e.g. `make install BINDIR=/custom/path`. See
the top of the `Makefile` for the full list of variables.

`make uninstall` removes everything `make install` put down, except it
leaves `niri-portals.conf` alone (it just reminds you which line to remove by
hand, since that file may hold other settings you own).

## Configuration

There isn't much to configure day-to-day — the whole point is that it Just
Works once installed. Two things you might care about:

- **Picker cache TTL**: 10 seconds, hardcoded as `CACHE_TTL` in
  `src/chooser.rs`. The cache file lives at
  `$XDG_RUNTIME_DIR/niri-screencast-portal.cache`; delete it to force a fresh
  prompt immediately.
- **Logging**: set `RUST_LOG` (standard `tracing_subscriber::EnvFilter`
  syntax) before starting the service, e.g.
  `systemctl --user edit xdg-desktop-portal-niri` and add
  `Environment=RUST_LOG=debug` under `[Service]`. Defaults to `info`.

## Usage

Nothing to do — once installed, any app that requests screen sharing through
the portal (browsers, Slack, OBS, etc.) will trigger a fuzzel prompt listing
every monitor and window. Pick one, and it shares.

Note: the picker always shows **both** screens and windows, regardless of
which specific source types the calling app asked for in that particular
`SelectSources` call. This is deliberate — see the comment above
`chooser::choose` in `src/chooser.rs` for why (short version: some clients
like Slack issue several `SelectSources` calls per share attempt with
different type masks, and the cache is shared across that whole burst).

## Troubleshooting

**Check which backend is actually handling `ScreenCast`:**

```sh
busctl --user introspect org.freedesktop.portal.Desktop /org/freedesktop/portal/desktop | grep -i screencast
journalctl --user -u xdg-desktop-portal-niri -f   # this backend's own logs
journalctl --user -u xdg-desktop-portal -f       # the central dispatcher's logs
```

**Nothing happens / still getting GNOME's native dialog:**
Routing to a custom backend on current `xdg-desktop-portal` (≥ ~1.17) requires
*both* a `.portal` registry file (which just registers the backend as a named
candidate) *and* a `niri-portals.conf` `[preferred]` override (which actually
selects it) — `make install` sets up both, but if you're debugging by hand,
double check neither is missing. The legacy `UseIn=` field in the `.portal`
file alone is only consulted as a last-resort fallback and will silently lose
to any backend's "first found" default (historically `gnome`) if it's not
backed by a `portals.conf` entry.

**`"A backend call failed: Signature mismatch"` in the frontend's journal:**
This means our `org.freedesktop.impl.portal.ScreenCast` method signature
doesn't match what the frontend expects — check the D-Bus method signatures
with `busctl --user introspect org.freedesktop.impl.portal.desktop.niri /org/freedesktop/portal/desktop`
against the real spec (`CreateSession`/`SelectSources`: `oosa{sv}`, `Start`:
`oossa{sv}` — all three take a leading `handle` Request-object-path
parameter that's easy to forget). Calling the backend directly via `busctl`
with a hand-written signature won't catch this, since you'd just be matching
whatever (possibly wrong) signature the backend declares — always verify
through the real frontend dispatcher.

**Window captures show a `1x1` size:**
This is a known cosmetic niri quirk for window streams, not a bug here — see
[niri#4587](https://github.com/YaLTeR/niri/issues/4587). Monitor capture
reports correct geometry.

**Fuzzel pops up and instantly picks something without me choosing:**
fuzzel's layer-shell surface grabs exclusive keyboard focus the instant it
renders. If you're typing anywhere else at that exact moment, a stray Enter
keystroke can land on it and select the top entry. Not a bug, just means
don't type while it's open.

**Reverting to `-gnome`:** edit
`~/.config/xdg-desktop-portal/niri-portals.conf`, remove the
`org.freedesktop.impl.portal.ScreenCast=niri;` line, then
`systemctl --user restart xdg-desktop-portal`.
>>>>>>> 453471f (Add build/install tooling and README)

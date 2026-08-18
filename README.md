# Tether

Share one keyboard and mouse across several computers on a LAN — a software
KVM. No extra hardware, no cloud, and no screen streaming: only input,
clipboard and file events cross the wire.

**macOS and Windows.** Linux is out of scope — Wayland offers no legacy input
path at all, and supporting X11 alone would mean shipping something broken on
the default session of every current distribution.

There is a window on both platforms: start and stop a session, and drag your
screens into the arrangement they actually have on your desk.

## Install

Downloads are on the [releases page](https://github.com/Fini240/tether/releases).
Each release ships a macOS universal build (Apple Silicon and Intel in one
binary), a Linux x86_64 binary, and a Windows x86_64 binary, with a
`SHA256SUMS` file.

**Which one do you want?**

| | |
|---|---|
| `Tether-<version>.dmg` | **macOS app.** Drag to Applications and open it. |
| `tether-<version>-windows-x86_64.zip` | **Windows.** Contains `Tether.exe` (the app) and `tether-cli.exe` (the CLI). |
| `tether-<version>-macos-universal.tar.gz` | macOS command-line tool only. |

On Windows the app needs no permission grant — low-level hooks work for any
process at the same integrity level. Windows Defender Firewall will ask about
the network the first time you start a host; allow it for private networks.

> **While this repository is private, downloads need authentication.** GitHub
> serves a 404 rather than a useful error to an unauthenticated request, so the
> usual `curl | sh` one-liner will not work yet. Use the GitHub CLI route below
> until the repo goes public.

### macOS

**The app.** Open `Tether-<version>.dmg`, drag `Tether.app` to Applications,
and launch it. It lives in the menu bar — no Dock icon and no window, because
there is nothing to show until you pick a role:

```
Stopped
⚠ Accessibility not granted            <- click this, then relaunch
Start as Host   /   Start as Client
Pair with a new machine on next start
Open Log…   /   Quit Tether
```

Granting Accessibility to `Tether.app` also covers the daemon it launches:
macOS attributes a TCC grant to the *responsible* process, and the daemon runs
as its child. Approving the bare CLI instead means approving whichever terminal
you started it from — broader, and it comes unstuck more easily.

**The CLI**, if you would rather type:

```sh
gh auth login                 # once
sh install.sh                 # picks the right build, verifies the checksum
```

Then **System Settings → Privacy & Security → Accessibility → +** and add
`/usr/local/bin/tether`.

### "I granted Accessibility and it still says I did not"

The most confusing failure in this whole project, so it gets its own heading.

macOS ties an Accessibility grant to the app's **designated requirement**. With
an ad-hoc signature — which is what these releases use, because notarisation
needs a paid Apple Developer account — that requirement is a hash of the binary
itself:

```
$ codesign -d -r- /Applications/Tether.app
designated => cdhash H"7262c7a151084eccbbc20143f51dbc9454c1716a"
```

Every build changes that hash. So after an update the old grant no longer
matches: the row stays in System Settings, still switched on, and macOS denies
anyway. Toggling it off and on does not help, because the stale entry *is* the
problem. Worse, each update leaves another one behind.

Fix it from the menu: **⚠ Accessibility not granted → "Already switched on?
Reset it and re-ask"**. Or by hand:

```sh
tccutil reset Accessibility dev.tether.Tether
```

then grant it again.

**To stop it recurring**, sign with a certificate instead of ad-hoc. A
self-signed one is enough — the requirement then names the certificate rather
than a hash, and survives every rebuild:

```sh
packaging/macos/make-signing-cert.sh
MACOS_SIGN_IDENTITY="Tether Local Signing" packaging/macos/bundle.sh
```

That does not help with Gatekeeper — a self-signed certificate is still not a
Developer ID — but the permission stops falling off.

### Other things that trip people up

Both macOS being macOS rather than bugs:

- **A downloaded binary is quarantined.** Gatekeeper blocks it and the message
  blames "an unidentified developer", which sends you looking in the wrong
  place. `install.sh` clears the flag for you; if you unpacked by hand, run
  `xattr -dr com.apple.quarantine /usr/local/bin/tether`. Releases are ad-hoc
  signed, not notarised — that needs a paid Apple Developer ID.
- **Injection fails silently without Accessibility.** No error, no dialog, just
  nothing happening. Both roles check at startup and refuse to run rather than
  let you conclude the network is broken.

### Windows

Unzip and run `Tether.exe`. Two limits are Windows' own, not bugs:

- A program running **as administrator** cannot be driven by one that is not.
  If everything works except an elevated app, run Tether as administrator too.
- The **secure desktop** — UAC prompts, Ctrl+Alt+Del, the lock screen — accepts
  no simulated input from anything, at any privilege level.

### Run at login (macOS)

```sh
cp packaging/macos/dev.tether.daemon.plist ~/Library/LaunchAgents/
launchctl load ~/Library/LaunchAgents/dev.tether.daemon.plist
```

Edit the plist first to choose `host` or `client`. It is a LaunchAgent, not a
LaunchDaemon, on purpose: a daemon runs with no window server session, where
CoreGraphics reports zero displays and an event tap can never fire.

## Build from source

Needs Rust 1.82+.

```sh
cargo build --release
```

To produce the release artifacts yourself:

```sh
packaging/macos/bundle.sh          # Tether.app, .dmg, signed CLI, checksums
python3 packaging/macos/make_icon.py   # just the icon
```

Try it without a second computer — a host and a client in one terminal each,
both on synthetic screens:

```sh
# terminal 1
./target/release/tether --config /tmp/a/config.json --backend headless \
    host --pair --port 24800

# terminal 2
./target/release/tether --config /tmp/b/config.json --backend headless \
    client --pair --host 127.0.0.1:24800
```

Each prints a pairing fingerprint; they should match what the other reports.

On real machines, drop `--backend headless` and run the same command on every
one of them:

```sh
tether run --pair           # on every machine
```

Run it once with `--pair` on each, confirm the fingerprints match, then restart
without it — from then on only those exact machines are accepted.

There is no host to nominate. The machines find each other over mDNS and settle
between themselves which one arbitrates the shared pointer, by comparing
machine ids — lowest wins, so both ends reach the same answer without
negotiating. Whichever keyboard you touch drives, and the pointer crosses in
either direction, regardless of which machine that turned out to be. A machine
that finds nobody takes the job itself and picks up the others as they arrive;
one that is outranked stands down, but never while somebody is connected to it.

`tether host` and `tether client` still exist for pinning the roles down by
hand, which is what you want when discovery cannot work — machines on different
subnets, or mDNS blocked.

### Other commands

| | |
|---|---|
| `tether layout` | show the screen arrangement and which way each machine lies |
| `tether layout <a> left <b>` | put machine `a` to the left of `b` (also `right`, `above`, `below`) |
| `tether name <new>` | rename this machine |
| `tether doctor` | check permissions, capture, injection, and the handoff guard |
| `tether screens` | this machine's displays, as the layout engine sees them |
| `tether discover` | hosts advertising on this network |
| `tether id` | this machine's identity and pairing fingerprint |
| `tether config` | config file path and contents |

## Arranging your screens

Connect the machines once — each is added to the canvas automatically, to the
right of everything already there — then say where they really sit:

```sh
tether layout                    # what it thinks right now
tether layout pc left mac        # the PC is to the left of the Mac
```

```
MACHINE            ID                     POSITION
pc                 4293ca21    -1920,0       1920x1080    Windows
mac                fce7211d        0,0       1920x1080    macOS  <- this machine

move left     to reach pc
```

Machines are named by hostname and addressed by name, or by an id prefix when
two share one. Placement is flush by design: a gap between screens would be
canvas that belongs to no monitor, and the pointer would stop dead in it
instead of crossing.

## Following whichever keyboard you touch

By default, control moves to the machine you physically touch. Put a hand on
the Mac's trackpad and the Mac drives; touch the PC's mouse and the PC drives.
No hotkey, no switching.

The thing that makes this work is telling *injected* input apart from real
input. Without it, the moment the PC injects a mouse move into the Mac, the
Mac's event tap sees "input!" and grabs control straight back — the two ends
fight forever. macOS solves it by stamping `kCGEventSourceUserData` on every
synthesised event and ignoring stamped events in the tap; Windows exposes
`LLMHF_INJECTED` directly.

Check it on your machine before relying on it:

```sh
tether doctor
```

```
Platform            macOS
Input permission    granted
Displays            1 found
Input capture       started
Injection marking   working (5 of our own events filtered)

Automatic input handoff is safe to use on this machine.
```

Two deliberate properties:

- **A machine only suppresses its own input while it is the one driving** and
  the pointer is elsewhere. Any other time your keyboard reaches your own apps
  — which is what lets you take control back by touching it, and what stops a
  dropped connection leaving a machine unusable.
- **The machine being touched moves its own cursor natively.** Nothing is
  injected into it, so there is no round trip in the common case, and the link
  going down cannot freeze your pointer.

Turn it off with `auto_input_handoff: false` in the config; `cursor_follows_input`
controls whether the pointer jumps to the machine you touch or stays put and is
driven from afar.

## How it works

One machine is the **host**: it owns the physical keyboard and mouse. Every
other machine runs a client that injects what it is told.

Every monitor of every machine is placed on one **virtual canvas**. The host
keeps a single authoritative cursor position on that canvas and moves it by the
deltas it captures. Whichever monitor contains the resulting point decides who
receives the event — so crossing a screen edge is just a lookup, and a machine
with three displays is simply three rectangles. Clients never do layout maths;
they are handed absolute coordinates in their own space.

While a client holds the cursor, the host **suppresses** input locally, so
keystrokes do not land on both machines at once, and **pins** its own cursor so
it stays where it was left. Suppression and pinning are two different jobs:
suppressing stops applications from seeing the movement, but the cursor sprite
is drawn from the input stream underneath that, so without pinning the arrow
goes on gliding around the machine you walked away from — and turns up in the
wrong place when you come back. On macOS the pin is
`CGAssociateMouseAndMouseCursorPosition(false)`, which severs the physical
mouse from the cursor while still delivering the movement that drives the other
machine.

Keys travel as USB HID usage codes — physical key positions, not characters —
so the receiving machine's own keyboard layout applies. Crossing between macOS
and anything else swaps Control and Meta, which is what makes ⌘C work as Ctrl+C
and back again.

## Security

There is no certificate authority on a LAN, so trust is **fingerprint pinning**,
like SSH's `known_hosts`:

- Each machine generates a self-signed certificate once. Its SHA-256
  fingerprint is its identity.
- `--pair` accepts an unknown fingerprint once and records it. Both ends print
  it so you can compare.
- Afterwards only recorded fingerprints are accepted, in both directions.

All traffic is TLS 1.2/1.3 (rustls, `ring` provider). Both ends authenticate —
this connection carries every keystroke you type, including passwords, so
one-sided authentication would not be enough. The pairing window is the weak
point: someone on your network during it could interpose. Compare the
fingerprints.

## Implementation status

| | capture | inject | monitors | clipboard | lock |
|---|---|---|---|---|---|
| macOS | ✅ CGEventTap | ✅ CGEvent | ✅ | ✅ | ✅ |
| Windows | ✅ WH_*_LL hooks | ✅ SendInput | ✅ | ✅ | ✅ |
| headless | ✅ | ✅ | ✅ | ✅ | ✅ |


Done: edge switching, multi-monitor, relative screen arrangement from the CLI,
automatic input handoff to whichever keyboard you touch, cursor lock,
jump-to-machine hotkeys, modifier remapping, clipboard text and images, mDNS
discovery with manual fallback, TLS with pinning, config persistence, graceful
reconnect.

Not done yet: file transfer (frames exist, offers are refused), rich-text
clipboard (degrades to plain text), lazy clipboard pull, live re-layout when a
client's resolution changes, hotkey suppression while the cursor is on the
host, a tray icon, and run-at-login on Windows.

### Verification

`cargo test` covers the geometry, keymap, codec, TLS and discovery units, plus
an end-to-end test that runs a host and a client in one process over a real TLS
socket and asserts that the cursor crosses, coordinates translate into the
client's space, and keystrokes arrive.

The macOS backend is **not** exercised by that suite — an event tap needs a
logged-in graphical session, which CI does not have. It has been verified by
hand on an Apple Silicon Mac: `CGGetActiveDisplayList` enumerates displays with
the correct Retina scale factor, and `CGEventTapCreate` installs successfully
(the host reaching `ready` is the proof — it does not get there if the tap is
refused).

Still unverified by anything: the injection path on a second physical Mac, and
so a real two-machine crossing. The routing either side of it is covered by the
end-to-end test.

## Layout of the source

| crate | what lives there |
|---|---|
| `tether-proto` | wire types and framing — everything that crosses the network |
| `tether-core` | canvas, cursor router, keymap, hotkeys, config. No OS, no network |
| `tether-gui` | the window: arrangement canvas, controls, status. egui, one binary |
| `tether-platform` | the OS boundary: capture, injection, monitors, clipboard |
| `tether-net` | TLS with pinning, identity, mDNS |
| `tether-daemon` | the `tether` binary: host and client session loops |

The interesting logic is in `tether-core::transition` and it is entirely
unit-testable without a second computer, which is the point of keeping it there.

## macOS permissions

Both roles need **Accessibility** (System Settings → Privacy & Security), and
some versions also prompt for **Input Monitoring**. The grant is tied to the
binary's code signature, so an unsigned rebuild invalidates it and it must be
removed and re-added. Sign with a stable identity to avoid that.

Injection fails *silently* without the grant, which is why both roles check at
startup rather than letting you conclude the network is broken.

## Licence

MIT OR Apache-2.0.

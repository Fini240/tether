# Tether

Share one keyboard and mouse across several computers on a LAN — a software
KVM. No extra hardware, no cloud, and no screen streaming: only input,
clipboard and file events cross the wire.

- **Nothing to configure.** Run the same command everywhere. The machines find
  each other and sort out the rest.
- **Whichever keyboard you touch drives.** Put a hand on the other machine and
  it takes over. No hotkey.
- **macOS, Windows and Linux**, including Wayland.

```sh
tether run --pair      # on every machine, once
tether run             # from then on
```

---

## Contents

| | |
|---|---|
| [Install](#install) | get it onto each machine |
| [First run](#first-run) | pairing, and what to expect |
| [Arranging your screens](#arranging-your-screens) | say where each machine sits |
| [Commands](#commands) | the whole CLI, one table |
| [When something is wrong](#when-something-is-wrong) | the usual suspects |
| [How it works](#how-it-works) | canvas, routing, suppression |
| [Security](#security) | pairing and TLS |
| [Platform support](#platform-support) | what works where |
| [Build from source](#build-from-source) | Rust 1.82+ |

---

## Install

Grab the file for your machine from the
[releases page](https://github.com/Fini240/tether/releases/latest).

| Download | Machine |
|---|---|
| `Tether-<v>.dmg` | **macOS** — drag to Applications |
| `tether-<v>-windows-x86_64.zip` | **Windows** — `Tether.exe` and `tether-cli.exe` |
| `tether-<v>-linux-x86_64.tar.gz` | **Linux** — `tether`, `tether-gui`, udev rule |
| `tether-<v>-macos-universal.tar.gz` | macOS command line only |

On macOS and Linux the one-liner works too:

```sh
curl -fsSL https://raw.githubusercontent.com/Fini240/tether/main/install.sh | sh
```

Each platform needs one thing doing once. Pick yours.

### macOS — grant Accessibility

Open the `.dmg`, drag `Tether.app` to Applications, launch it, and click
**⚠ Accessibility not granted**. Then relaunch.

Granting it to `Tether.app` also covers the session it runs, because macOS
attributes the grant to the *responsible* process. Approving the bare CLI
instead approves whichever terminal you started it from — broader, and it comes
unstuck more easily.

<details>
<summary><b>"I granted Accessibility and it still says I did not"</b> — the most
confusing failure in this project</summary>

macOS ties the grant to the app's **designated requirement**. With an ad-hoc
signature — which is what these releases use, because notarisation needs a paid
Apple Developer account — that requirement is a hash of the binary itself:

```
$ codesign -d -r- /Applications/Tether.app
designated => cdhash H"7262c7a151084eccbbc20143f51dbc9454c1716a"
```

Every build changes that hash, so after an update the old grant no longer
matches. The row stays in System Settings, still switched on, and macOS denies
anyway — the stale entry *is* the problem, which is why toggling it does not
help. Each update leaves another one behind.

Fix from the menu: **⚠ Accessibility not granted → "Already switched on? Reset
it and re-ask"**. Or by hand:

```sh
tccutil reset Accessibility dev.tether.Tether
```

**To stop it recurring**, sign with a certificate rather than ad-hoc. Even a
self-signed one is enough — the requirement then names the certificate instead
of a hash and survives every rebuild:

```sh
packaging/macos/make-signing-cert.sh
MACOS_SIGN_IDENTITY="Tether Local Signing" packaging/macos/bundle.sh
```

That does nothing for Gatekeeper, which wants a real Developer ID, but the
permission stops falling off.
</details>

<details>
<summary>Other macOS things that are macOS, not bugs</summary>

- **A downloaded binary is quarantined.** Gatekeeper blames "an unidentified
  developer", which sends you looking in the wrong place. `install.sh` clears
  the flag; by hand it is
  `xattr -dr com.apple.quarantine /usr/local/bin/tether`.
- **Injection fails silently without Accessibility** — no error, no dialog.
  Every role checks at startup and refuses to run rather than let you conclude
  the network is broken.
</details>

### Windows — nothing to grant

Unzip somewhere permanent and run `Tether.exe`. Low-level hooks need no
permission. Windows Defender Firewall asks once — allow it on **private**
networks.

<details>
<summary>Two limits that are Windows' own</summary>

- A program running **as administrator** cannot be driven by one that is not.
  If everything works except an elevated app, run Tether as administrator too.
- The **secure desktop** — UAC prompts, Ctrl+Alt+Del, the lock screen — accepts
  no simulated input from anything, at any privilege level.
</details>

### Linux — join the `input` group

```sh
sudo cp 99-tether.rules /etc/udev/rules.d/
sudo udevadm control --reload-rules && sudo udevadm trigger
sudo modprobe uinput
echo uinput | sudo tee /etc/modules-load.d/uinput.conf
sudo usermod -aG input $USER
```

**Then log out and back in** — a full logout, not a screen lock. Group
membership only applies to new sessions, and this is the step people skip
before reporting that it does not work. Check with `groups | grep input`, then
`tether doctor`.

> Anyone in the `input` group can read every keystroke on the machine and
> synthesise input as any user. That is inherent to a software KVM — Synergy,
> Barrier and Deskflow need the same — but it is worth knowing before granting
> it to an account.

<details>
<summary>Why evdev rather than X11 or Wayland, and what it costs</summary>

X11 has XTEST and XInput2 and works beautifully until the machine boots a
Wayland session, which most distributions now do by default. Wayland has no
portable equivalent: capturing input globally is deliberately not something a
client may do. `evdev` and `uinput` sit below both, so one backend covers X11,
Wayland and a bare console.

Two things follow from that, and neither has a fix that would not mean giving
up on Wayland:

- **The cursor position is not readable.** Only the display server knows it.
  Movement is tracked as deltas — the path every platform already uses while
  suppressed. Invisible on one screen; across two screens of different heights
  the pointer can enter the next at the wrong height.
- **The screen arrangement is not readable.** `/sys/class/drm` gives each
  display's size but not where you put it. One screen is exact; several are
  guessed left to right and may need one drag.

One thing comes free: suppression is `EVIOCGRAB`, which takes the device for
this process alone, so a grabbed mouse is invisible to the display server and
the local cursor stops dead. macOS needs a whole extra mechanism for that.

`tether-gui` needs X11 or Wayland libraries present. The `tether` CLI needs
none of them and runs on a machine with no desktop at all.
</details>

---

## First run

Run this on **every** machine, once:

```sh
tether run --pair
```

Each prints a fingerprint. Check they match, then stop them and start again
without `--pair` — from then on only those exact machines are accepted.

> **Never leave `--pair` on.** While it is, anything on your network can
> connect.

In the window it is one button: **Connect**.

There is no host to nominate. The machines find each other over mDNS and settle
between themselves which one arbitrates the shared pointer, by comparing
machine ids — lowest wins, so both ends reach the same answer without
negotiating. A machine that finds nobody takes the job and picks the others up
as they arrive; one that is outranked stands down, but never while somebody is
connected to it.

`tether host` and `tether client` still exist for pinning the roles down by
hand, which is what you want when discovery cannot work — machines on different
subnets, or mDNS blocked.

### Run at login (macOS)

```sh
cp packaging/macos/dev.tether.daemon.plist ~/Library/LaunchAgents/
launchctl load ~/Library/LaunchAgents/dev.tether.daemon.plist
```

A LaunchAgent, not a LaunchDaemon, on purpose: a daemon runs with no window
server session, where CoreGraphics reports zero displays and an event tap can
never fire.

---

## Arranging your screens

Every machine is added to the canvas automatically, to the right of everything
already there. Then say where it really sits — by dragging in the window, or:

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

Machines are addressed by name, or by an id prefix when two share one.
Placement is flush by design: a gap would be canvas belonging to no monitor,
and the pointer would stop dead in it instead of crossing.

> The CLI writes the config file, and a **running** session only reads the
> layout when it starts — restart it for the change to take. The window applies
> it immediately.

---

## Commands

| | |
|---|---|
| `tether run` | join the network; the normal way to start |
| `tether run --pair` | …and trust a new machine, once |
| `tether doctor` | check permissions, capture, injection and the handoff guard |
| `tether layout` | show the screen arrangement |
| `tether layout <a> left <b>` | place `a` relative to `b` — also `right`, `above`, `below` |
| `tether discover` | machines advertising on this network |
| `tether id` | this machine's identity and pairing fingerprint |
| `tether screens` | this machine's displays, as the layout engine sees them |
| `tether name <new>` | rename this machine |
| `tether config` | config file path and contents |
| `tether host` / `tether client` | pin the role by hand |

---

## When something is wrong

Start here. It checks everything that fails silently:

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

**The pointer will not cross.** Check both machines are running, are on the
same subnet, and that the arrangement actually puts them next to each other on
the edge you are pushing against — `tether layout`.

**Control keeps snapping back to the other machine.** That is
`auto_input_handoff` working as designed: touching a machine's own keyboard or
mouse gives it control. Set `auto_input_handoff: false` in the config if it
gets in the way. `cursor_follows_input` controls whether the pointer jumps to
the machine you touch or stays put and is driven from afar.

**It says injection marking is BROKEN.** Either the machine cannot tell its own
injected input from a hand on the keyboard — in which case two machines will
fight over which is driving — or you touched the mouse during the two seconds
the test runs. Run it again without touching anything.

**Everything looks right and nothing happens.** Turn the log up; it names the
machine that claimed control and the event that did it:

```sh
tether run --log debug
```

---

## How it works

Every monitor of every machine is placed on one **virtual canvas**. Whichever
machine is arbitrating keeps a single authoritative cursor position on it and
moves that by the deltas it is given. Whichever monitor contains the resulting
point decides who receives the event — so crossing a screen edge is a lookup,
and a machine with three displays is simply three rectangles. Nobody else does
layout maths; they are handed absolute coordinates in their own space.

Input flows in both directions regardless of which machine arbitrates. That is
why the role can be elected rather than configured, and why it is invisible.

While the pointer is elsewhere, a machine **suppresses** its own input so
keystrokes do not land on two machines at once, and **pins** its own cursor so
it stays where it was left. Those are two different jobs: suppressing stops
applications from seeing the movement, but the cursor sprite is drawn from the
input stream underneath that, so without pinning the arrow goes on gliding
around the machine you walked away from — and turns up somewhere else when you
come back.

| | suppress | pin |
|---|---|---|
| macOS | `CGEventTap` returning NULL | `CGAssociateMouseAndMouseCursorPosition(false)` |
| Windows | hook returns non-zero | re-centre against an anchor |
| Linux | `EVIOCGRAB` | the same grab |

Keys travel as USB HID usage codes — physical key positions, not characters —
so the receiving machine's own keyboard layout applies. Crossing between macOS
and anything else swaps Control and Meta, which is what makes ⌘C arrive as
Ctrl+C and back again.

<details>
<summary>Telling injected input from a real hand</summary>

Without this, the moment one machine injects a mouse move into another, the
receiver sees "input!" and grabs control straight back — the two ends fight
forever.

macOS stamps `kCGEventSourceUserData` on every synthesised event and ignores
stamped events in the tap. Windows gets `LLMHF_INJECTED` for free. Linux needs
neither: it injects through its own `uinput` device nodes, which the capture
side never opens, so there is nothing to recognise and nothing that could leak.

`tether doctor` proves whichever applies before you rely on it.
</details>

---

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

---

## Platform support

| | capture | inject | monitors | clipboard | lock |
|---|---|---|---|---|---|
| macOS | ✅ CGEventTap | ✅ CGEvent | ✅ | ✅ | ✅ |
| Windows | ✅ WH_*_LL hooks | ✅ SendInput | ✅ | ✅ | ✅ |
| Linux | ✅ evdev | ✅ uinput | ⚠️ sizes only | ✅ | ⚠️ `loginctl` |
| headless | ✅ | ✅ | ✅ | ✅ | ✅ |

⚠️ Linux: the kernel knows each display's size but not how you arranged them,
and locking goes through `loginctl`, which not every session provides. Both are
explained [above](#linux--join-the-input-group).

**Working:** edge switching, multi-monitor, screen arrangement, automatic input
handoff, cursor lock, jump-to-machine hotkeys, modifier remapping, clipboard
text and images, mDNS discovery with manual fallback, TLS with pinning, config
persistence, graceful reconnect, automatic arbiter election.

**Not yet:** file transfer (frames exist, offers are refused), rich-text
clipboard (degrades to plain text), lazy clipboard pull, live re-layout when a
machine's resolution changes, hotkey suppression while the pointer is away, a
tray icon, run-at-login on Windows.

<details>
<summary>What is actually tested, and what is not</summary>

`cargo test` covers geometry, keymap, codec, TLS, discovery and the arbiter
election, plus an end-to-end test that runs two roles in one process over a
real TLS socket and asserts that the cursor crosses, coordinates translate, and
keystrokes arrive. CI runs the suite on macOS, Windows and Linux.

The **native backends are not exercised by that suite** — an event tap needs a
logged-in graphical session, and a uinput device needs real permissions;
neither is available to CI. They are verified by hand:

- macOS ⇄ Windows and macOS ⇄ Linux crossings, both directions, on real
  hardware.
- Linux: `tether doctor` on Garuda reports capture started, displays
  enumerated, and nothing leaking back.
- The keycode tables and DRM mode parsing do run in CI on Linux.

Not verified: a second physical Mac, and any BSD.
</details>

---

## Build from source

Rust 1.82+.

```sh
cargo build --release
```

Two machines in one terminal each, on synthetic screens, no second computer:

```sh
# terminal 1
./target/release/tether --config /tmp/a/config.json --backend headless \
    host --pair --port 24800

# terminal 2
./target/release/tether --config /tmp/b/config.json --backend headless \
    client --pair --host 127.0.0.1:24800
```

Release artifacts:

```sh
packaging/macos/bundle.sh              # Tether.app, .dmg, signed CLI, checksums
packaging/windows/cross-build.sh       # Windows .exe files, from a Mac or Linux
```

### Layout of the source

| crate | what lives there |
|---|---|
| `tether-proto` | wire types and framing — everything that crosses the network |
| `tether-core` | canvas, cursor router, keymap, hotkeys, config. No OS, no network |
| `tether-platform` | the OS boundary: capture, injection, monitors, clipboard |
| `tether-net` | TLS with pinning, identity, mDNS |
| `tether-daemon` | the `tether` binary: arbiter election and both session loops |
| `tether-gui` | the window: arrangement canvas, controls, status. egui |

The interesting logic is in `tether-core::transition`, and it is entirely
unit-testable without a second computer, which is the point of keeping it
there.

---

## Licence

MIT OR Apache-2.0.

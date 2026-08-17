# Tether

Share one keyboard and mouse across several computers on a LAN — a software
KVM. No extra hardware, no cloud, and no screen streaming: only input,
clipboard and file events cross the wire.

**Status: walking skeleton.** The end-to-end path works — pairing, TLS,
automatic layout, edge-based cursor switching, keyboard and mouse forwarding,
cross-platform modifier remapping, clipboard sync. Windows and Linux backends,
file transfer, and the arrangement UI are stubbed behind real interfaces. See
[Implementation status](#implementation-status) for exactly what is and is not
done.

## Install

Downloads are on the [releases page](https://github.com/Fini240/tether/releases).
Each release ships a macOS universal build (Apple Silicon and Intel in one
binary), a Linux x86_64 binary, and a Windows x86_64 binary, with a
`SHA256SUMS` file.

There is no `.app` or `.dmg`, on purpose. `tether` is a CLI that needs a
subcommand, so an app wrapper would run it with no arguments and exit
silently — an installer that appears to work and does nothing. A real bundle
comes with the tray UI.

> **While this repository is private, downloads need authentication.** GitHub
> serves a 404 rather than a useful error to an unauthenticated request, so the
> usual `curl | sh` one-liner will not work yet. Use the GitHub CLI route below
> until the repo goes public.

### macOS

```sh
gh auth login                 # once
sh install.sh                 # picks the right build, verifies the checksum
```

Then grant the permission it cannot work without:

**System Settings → Privacy & Security → Accessibility → +** and add
`/usr/local/bin/tether`.

Two things that trip people up here, both macOS being macOS rather than bugs:

- **A downloaded binary is quarantined.** Gatekeeper blocks it and the message
  blames "an unidentified developer", which sends you looking in the wrong
  place. `install.sh` clears the flag for you; if you unpacked by hand, run
  `xattr -dr com.apple.quarantine /usr/local/bin/tether`. Releases are ad-hoc
  signed, not notarised — that needs a paid Apple Developer ID.
- **Injection fails silently without Accessibility.** No error, no dialog, just
  nothing happening. Both roles check at startup and refuse to run rather than
  let you conclude the network is broken.

### Linux and Windows

The binaries build and run, but **the native input backends are not implemented
yet** — they start only with `--backend headless`, which is for testing the
network and routing, not for actually sharing a keyboard. See
[Implementation status](#implementation-status).

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
packaging/macos/bundle.sh          # signed universal binary + checksums
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

On real machines, drop `--backend headless` and let discovery find the host:

```sh
tether host --pair          # the machine with the keyboard and mouse
tether client --pair        # every other machine
```

Run both once with `--pair`, confirm the fingerprints match, then restart
without it — from then on only those exact machines are accepted.

### Other commands

| | |
|---|---|
| `tether screens` | this machine's displays, as the layout engine sees them |
| `tether discover` | hosts advertising on this network |
| `tether id` | this machine's identity and pairing fingerprint |
| `tether config` | config file path and contents |

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
keystrokes do not land on both machines at once.

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
| Windows | ⛔ | ⛔ | ⛔ | ✅ | ⛔ |
| Linux / X11 | ⛔ | ⛔ | ⛔ | ✅ | ⛔ |
| Linux / Wayland | ⛔ | ⛔ | ⛔ | ✅ | ⛔ |
| headless | ✅ | ✅ | ✅ | ✅ | ✅ |

Unimplemented backends fail at startup with the name of the API they need,
rather than appearing to work. `crates/platform/src/windows.rs` and
`linux.rs` carry the notes for building them.

Done: edge switching, multi-monitor, cursor lock, jump-to-machine hotkeys,
modifier remapping, clipboard text and images, mDNS discovery with manual
fallback, TLS with pinning, config persistence, graceful reconnect.

Not done yet: file transfer (frames exist, offers are refused), the arrangement
UI, rich-text clipboard (degrades to plain text), lazy clipboard pull, live
re-layout when a client's resolution changes, hotkey suppression while the
cursor is on the host, tray app and service packaging.

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

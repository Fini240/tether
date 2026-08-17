//! End-to-end: a host and a client in one process, talking over a real TLS
//! socket, with headless backends standing in for the operating system.
//!
//! This is the test that says the walking skeleton walks. It covers the whole
//! path — pairing, the TLS handshake, layout placement, edge detection,
//! coordinate translation, frame encoding — by asserting on what the *client's*
//! backend was told to inject. A unit test of the router could not catch a
//! coordinate space confused somewhere between the two.

use std::path::PathBuf;
use std::time::Duration;

use tether_core::config::Config;
use tether_daemon::{clientmode, host};
use tether_net::Identity;
use tether_platform::headless::{backend_with, fake_monitor, HeadlessHandle};
use tether_platform::LocalEvent;
use tether_proto::{InputEvent, KeyCode, Modifiers, MouseButton};

const HOST_WIDTH: i32 = 1920;
const HOST_HEIGHT: i32 = 1080;
const CLIENT_WIDTH: i32 = 1280;
const CLIENT_HEIGHT: i32 = 1024;

struct Harness {
    host_input: HeadlessHandle,
    client_output: HeadlessHandle,
    _dir: TempDir,
}

/// Removes itself on drop so a failing test does not litter /tmp.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let path = std::env::temp_dir().join(format!(
            "tether-e2e-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("could not create the temp dir");
        TempDir(path)
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Start a host and one client, both headless, and wait until they are joined.
async fn start(tag: &str) -> Harness {
    let dir = TempDir::new(tag);

    let (host_backend, host_input) =
        backend_with(vec![fake_monitor(0, 0, 0, HOST_WIDTH, HOST_HEIGHT, true)]);
    let (client_backend, client_output) = backend_with(vec![fake_monitor(
        0,
        0,
        0,
        CLIENT_WIDTH,
        CLIENT_HEIGHT,
        true,
    )]);

    let host_identity = Identity::generate().expect("host identity");
    let client_identity = Identity::generate().expect("client identity");

    let host_config = Config {
        name: "test-host".into(),
        ..Config::default()
    };
    let client_config = Config {
        name: "test-client".into(),
        ..Config::default()
    };

    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();

    tokio::spawn(host::run(
        host::Options {
            // Port 0: the OS picks a free one and `ready` reports it back, so
            // concurrent test runs cannot collide on a fixed port.
            bind: "127.0.0.1:0".into(),
            pairing: true,
            config_path: dir.join("host-config.json"),
            advertise: false,
            ready: Some(ready_tx),
            control: None,
        },
        host_config,
        host_identity,
        host_backend,
    ));

    let addr = tokio::time::timeout(Duration::from_secs(5), ready_rx)
        .await
        .expect("host did not start listening in time")
        .expect("host dropped the ready channel");

    tokio::spawn(clientmode::run(
        clientmode::Options {
            address: Some(addr.to_string()),
            pairing: true,
            config_path: dir.join("client-config.json"),
            control: None,
        },
        client_config,
        client_identity,
        client_backend,
    ));

    Harness {
        host_input,
        client_output,
        _dir: dir,
    }
}

/// Push the pointer off the host's right edge, then nudge it once more inside
/// the client.
///
/// Two details this encodes:
///
/// * The join is asynchronous, so early nudges land while no client exists yet.
///   A delta that lands in empty canvas is refused and the position is
///   unchanged, so retrying is safe and makes the test deterministic without
///   sleeping on a guess.
/// * Crossing itself injects nothing — the host sends `Enter`, which *warps*
///   the client's cursor rather than synthesising motion. Swallowing on the
///   host is the real crossing signal; the follow-up nudge is what produces
///   the first `MouseMove`.
async fn cross_to_client(harness: &Harness) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        harness.host_input.emit(LocalEvent::MouseDelta {
            dx: HOST_WIDTH,
            dy: 0,
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        if harness.host_input.is_swallowing() {
            harness
                .host_input
                .emit(LocalEvent::MouseDelta { dx: 10, dy: 10 });
            tokio::time::sleep(Duration::from_millis(50)).await;
            return;
        }
    }
    panic!("the cursor never crossed onto the client");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_cursor_crosses_the_edge_onto_a_client() {
    let harness = start("cursor").await;
    cross_to_client(&harness).await;

    let injected = harness.client_output.injected();
    let first_move = injected
        .iter()
        .find_map(|event| match event {
            InputEvent::MouseMove { x, y } => Some((*x, *y)),
            _ => None,
        })
        .expect("the client received no pointer motion");

    // The client's own coordinate space, not the shared canvas — the host is
    // 1920 wide and the client's origin sits at global x=1920, so anything
    // near or above 1920 here would mean the translation never happened.
    assert!(
        first_move.0 >= 0 && first_move.0 < CLIENT_WIDTH,
        "x {} is outside the client's 0..{CLIENT_WIDTH} space",
        first_move.0
    );
    assert!(
        first_move.1 >= 0 && first_move.1 < CLIENT_HEIGHT,
        "y {} is outside the client's 0..{CLIENT_HEIGHT} space",
        first_move.1
    );

    // The host must stop delivering input locally while the client owns it.
    assert!(
        harness.host_input.is_swallowing(),
        "the host is still delivering input to itself"
    );
    assert!(
        !harness.host_input.cursor_visible(),
        "the host's cursor should be hidden while a client has it"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn keystrokes_reach_the_client_that_holds_the_cursor() {
    let harness = start("keys").await;
    cross_to_client(&harness).await;
    harness.client_output.clear_injected();

    harness.host_input.emit(LocalEvent::Key {
        key: KeyCode::C,
        pressed: true,
        modifiers: Modifiers::META,
        repeat: false,
    });
    harness.host_input.emit(LocalEvent::Button {
        button: MouseButton::Left,
        pressed: true,
    });

    let injected = wait_for(&harness.client_output, 2).await;

    assert!(
        injected.iter().any(|event| matches!(
            event,
            InputEvent::Key {
                key: KeyCode::C,
                pressed: true,
                ..
            }
        )),
        "the client never got the key press: {injected:?}"
    );
    assert!(
        injected.iter().any(|event| matches!(
            event,
            InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed: true
            }
        )),
        "the client never got the click: {injected:?}"
    );
}

/// Crossing an edge the operating system clamps the cursor against, which is
/// the only kind a desktop's *outer* edge ever is.
///
/// `cross_to_client` uses bare deltas, which is what a machine reports only
/// while its own pointer is pinned. The path a host actually takes to leave its
/// own screen is `MouseMoved`: a position the OS has already decided on, plus
/// how far the device moved. Those two stop agreeing at the edge — the position
/// cannot go any further while the device keeps moving — and the difference is
/// the whole crossing.
///
/// Reported as "the mouse slides to the edge when I go from my PC to my Mac,
/// and I can't use my keyboard on the Mac". Both halves came from the same
/// place. The host applied the position *and* the delta on top of it, so it ran
/// a full event ahead of the pointer and handed over while the visible cursor
/// was still short of the edge — leaving it to finish sliding on its own. Then
/// the events captured in the instant before suppression started arrived, each
/// carrying a position back on the host, and each was believed: the pointer was
/// yanked back off the client, and every keystroke after that went to the host
/// while the cursor sat on the client's screen looking ready for it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_clamped_edge_crosses_and_the_keyboard_goes_with_it() {
    let harness = start("clamped").await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut crossed = false;
    while tokio::time::Instant::now() < deadline {
        // Up against the last column the host's desktop has.
        harness.host_input.emit(LocalEvent::MouseMoved {
            x: HOST_WIDTH - 1,
            y: 540,
            dx: 900,
            dy: 0,
        });
        tokio::time::sleep(Duration::from_millis(20)).await;

        // Still pushing right. The OS has nowhere left to put the pointer, so
        // it reports the same position it did last time and only the device
        // movement says anything at all.
        harness.host_input.emit(LocalEvent::MouseMoved {
            x: HOST_WIDTH - 1,
            y: 540,
            dx: 60,
            dy: 0,
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        if harness.host_input.is_swallowing() {
            crossed = true;
            break;
        }
    }
    assert!(
        crossed,
        "the pointer never left a host screen it was already clamped against"
    );

    // The tail of events captured before suppression began, arriving late.
    // Every one of them reports a position on the host, and a purely vertical
    // one carries nothing that would push it back over the edge.
    for _ in 0..3 {
        harness.host_input.emit(LocalEvent::MouseMoved {
            x: HOST_WIDTH - 1,
            y: 544,
            dx: 0,
            dy: 4,
        });
    }
    tokio::time::sleep(Duration::from_millis(150)).await;

    assert!(
        harness.host_input.is_swallowing(),
        "a stale position report took the pointer back off the client"
    );

    harness.client_output.clear_injected();
    harness.host_input.emit(LocalEvent::Key {
        key: KeyCode::C,
        pressed: true,
        modifiers: Modifiers::NONE,
        repeat: false,
    });

    let injected = wait_for(&harness.client_output, 1).await;
    assert!(
        injected.iter().any(|event| matches!(
            event,
            InputEvent::Key {
                key: KeyCode::C,
                pressed: true,
                ..
            }
        )),
        "the client holds the cursor but never got the keystroke: {injected:?}"
    );
}

/// The router must land where the OS put the pointer, not a whole event past it.
///
/// This is the "slides to the edge" half on its own. A fast flick reports a
/// large movement and the position the OS reached with it — the same motion,
/// described twice. Counting it twice puts the router a whole event ahead of
/// the cursor, so control leaves while the pointer the user is watching is
/// still well inside the screen, and it finishes the journey to the edge on its
/// own with nothing driving it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn motion_the_os_applied_does_not_cross_early() {
    let harness = start("noearly").await;

    // Cross and come back, which is the only way to know the client has
    // finished joining — until it has, there is nothing beyond the edge to
    // cross onto and this would pass for the wrong reason.
    cross_to_client(&harness).await;
    for _ in 0..6 {
        harness.host_input.emit(LocalEvent::MouseDelta {
            dx: -CLIENT_WIDTH,
            dy: 0,
        });
        tokio::time::sleep(Duration::from_millis(40)).await;
        if !harness.host_input.is_swallowing() {
            break;
        }
    }
    assert!(
        !harness.host_input.is_swallowing(),
        "the pointer never came back to the host"
    );

    // Settle: the pointer is here and nothing moved.
    harness.host_input.emit(LocalEvent::MouseMoved {
        x: 960,
        y: 540,
        dx: 0,
        dy: 0,
    });
    tokio::time::sleep(Duration::from_millis(60)).await;

    // Now one fast flick that the OS applied in full, ending 210px short of
    // the edge. Nothing here asks to leave the screen.
    harness.host_input.emit(LocalEvent::MouseMoved {
        x: HOST_WIDTH - 210,
        y: 540,
        dx: 750,
        dy: 0,
    });
    tokio::time::sleep(Duration::from_millis(150)).await;

    assert!(
        !harness.host_input.is_swallowing(),
        "control left the host while its cursor was still 210px inside the screen"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_cursor_comes_back_to_the_host() {
    let harness = start("return").await;
    cross_to_client(&harness).await;
    harness.client_output.clear_injected();

    // Walk back left across the boundary.
    for _ in 0..4 {
        harness.host_input.emit(LocalEvent::MouseDelta {
            dx: -CLIENT_WIDTH,
            dy: 0,
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        if !harness.host_input.is_swallowing() {
            break;
        }
    }

    assert!(
        !harness.host_input.is_swallowing(),
        "the host is still swallowing input after the cursor returned"
    );
    assert!(
        harness.host_input.cursor_visible(),
        "the host's cursor should be visible again"
    );
}

/// Poll until at least `count` events have been injected, or time out.
async fn wait_for(handle: &HeadlessHandle, count: usize) -> Vec<InputEvent> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let injected = handle.injected();
        if injected.len() >= count || tokio::time::Instant::now() > deadline {
            return injected;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Touching a client's keyboard should take control away from the host.
///
/// This is the half of input handoff that cannot be checked by inspection: it
/// spans a claim from the client, arbitration on the host, the cursor
/// following, and an `Enter` coming back. The marker that stops the two ends
/// fighting over control is platform-specific and is covered by
/// `tether doctor` instead — the headless backend injects nowhere, so there is
/// nothing for it to filter.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn touching_a_client_hands_control_to_it() {
    let harness = start("handoff").await;

    // The host starts out driving, with the cursor on its own screen.
    let client_start = harness.client_output.cursor();

    // Somebody puts a hand on the client's trackpad.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut moved = false;
    while tokio::time::Instant::now() < deadline {
        harness
            .client_output
            .emit(LocalEvent::MouseDelta { dx: 40, dy: 40 });
        tokio::time::sleep(Duration::from_millis(50)).await;

        // cursor_follows_input brings the pointer to the machine being
        // touched, which arrives as an Enter and warps the cursor to the
        // centre of its screen.
        let now = harness.client_output.cursor();
        if now != client_start && now.x > 0 && now.y > 0 {
            moved = true;
            break;
        }
    }

    assert!(
        moved,
        "the client touched its own trackpad and never got control"
    );

    // The host must not be suppressing its own input any more: it is no longer
    // the machine being touched, and its user has to be able to take control
    // back by touching it.
    assert!(
        !harness.host_input.is_swallowing(),
        "the host is still swallowing its own input after handing control over"
    );
}

/// ...and touching the host again takes it straight back.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn touching_the_host_takes_control_back() {
    let harness = start("handback").await;

    // Hand control to the client first.
    let client_start = harness.client_output.cursor();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        harness
            .client_output
            .emit(LocalEvent::MouseDelta { dx: 40, dy: 40 });
        tokio::time::sleep(Duration::from_millis(50)).await;
        if harness.client_output.cursor() != client_start {
            break;
        }
    }
    harness.client_output.clear_injected();

    // Now touch the host.
    harness
        .host_input
        .emit(LocalEvent::MouseDelta { dx: 40, dy: 40 });
    tokio::time::sleep(Duration::from_millis(400).min(Duration::from_millis(400))).await;

    // With the host driving and the cursor pulled back to it, the host handles
    // its own pointer natively and sends the client nothing further.
    harness.client_output.clear_injected();
    harness
        .host_input
        .emit(LocalEvent::MouseDelta { dx: 5, dy: 5 });
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert!(
        harness.client_output.injected().is_empty(),
        "the host is still driving the client after taking control back: {:?}",
        harness.client_output.injected()
    );
}

/// A client driving, with the pointer on the host, must move the host's cursor.
///
/// This is the case the original routing missed. The host special-cased itself
/// and returned early — correct only while the host was always the machine
/// being touched. Once a client could take over, the host stopped injecting
/// anything into itself: the pointer moved at the instant it crossed onto the
/// host and then sat still until the next crossing, which reads as severe lag
/// rather than as nothing arriving at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_client_can_drive_the_host() {
    let harness = start("clientdrives").await;

    // Hand control to the client by touching it. cursor_follows_input brings
    // the pointer over to the client's screen.
    let client_start = harness.client_output.cursor();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut took_over = false;
    while tokio::time::Instant::now() < deadline {
        harness
            .client_output
            .emit(LocalEvent::MouseDelta { dx: 40, dy: 40 });
        tokio::time::sleep(Duration::from_millis(50)).await;
        if harness.client_output.cursor() != client_start {
            took_over = true;
            break;
        }
    }
    assert!(took_over, "the client never took control");

    harness.host_input.clear_injected();

    // Now walk the pointer back onto the host — still driving from the client.
    for _ in 0..6 {
        harness
            .client_output
            .emit(LocalEvent::MouseDelta { dx: -400, dy: 0 });
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    // ...and keep moving once it is there.
    for _ in 0..4 {
        harness
            .client_output
            .emit(LocalEvent::MouseDelta { dx: -20, dy: 10 });
        tokio::time::sleep(Duration::from_millis(40)).await;
    }

    let injected = harness.host_input.injected();
    assert!(
        injected
            .iter()
            .any(|e| matches!(e, InputEvent::MouseMove { .. })),
        "the host was never told to move its own pointer while a client drove it: {injected:?}"
    );

    // Keystrokes have to arrive too, by the same path.
    harness.host_input.clear_injected();
    harness.client_output.emit(LocalEvent::Key {
        key: KeyCode::Z,
        pressed: true,
        modifiers: Modifiers::NONE,
        repeat: false,
    });
    let injected = wait_for(&harness.host_input, 1).await;
    assert!(
        injected.iter().any(|e| matches!(
            e,
            InputEvent::Key {
                key: KeyCode::Z,
                pressed: true,
                ..
            }
        )),
        "a keystroke from the driving client never reached the host: {injected:?}"
    );
}

/// With a client driving, the pointer must be able to come back to it.
///
/// Reported as "I can go from my Mac to my PC but not back". The Mac is the
/// client and the one being touched; once the pointer is on the host, walking
/// it back across the boundary has to hand the client its own cursor again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_driving_client_can_get_the_pointer_back() {
    let harness = start("roundtrip").await;

    // Take control from the client, which also brings the pointer over.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let start_cursor = harness.client_output.cursor();
    let mut took_over = false;
    while tokio::time::Instant::now() < deadline {
        harness
            .client_output
            .emit(LocalEvent::MouseDelta { dx: 40, dy: 40 });
        tokio::time::sleep(Duration::from_millis(50)).await;
        if harness.client_output.cursor() != start_cursor {
            took_over = true;
            break;
        }
    }
    assert!(took_over, "the client never took control");

    // Walk left onto the host.
    for _ in 0..8 {
        harness
            .client_output
            .emit(LocalEvent::MouseDelta { dx: -400, dy: 0 });
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    let on_host = wait_for(&harness.host_input, 1).await;
    assert!(
        !on_host.is_empty(),
        "the pointer never reached the host in the first place"
    );

    let before_return = harness.client_output.cursor();
    harness.host_input.clear_injected();

    // ...and back to the right, onto the client.
    for _ in 0..10 {
        harness
            .client_output
            .emit(LocalEvent::MouseDelta { dx: 400, dy: 0 });
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Crossing back sends Enter, which warps the client's cursor. After that
    // the client owns its own pointer again and the host stops being driven.
    let after = harness.client_output.cursor();
    assert_ne!(
        after, before_return,
        "the pointer never came back to the client that was driving"
    );

    harness.host_input.clear_injected();
    harness
        .client_output
        .emit(LocalEvent::MouseDelta { dx: 5, dy: 5 });
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(
        harness.host_input.injected().is_empty(),
        "the host is still being driven after the pointer returned to the client: {:?}",
        harness.host_input.injected()
    );
}

/// A connected client must stop when asked, promptly.
///
/// It did not. The only check for a stop request sat between connection
/// attempts, so a client with a healthy connection never looked at it — and a
/// window that joined the session thread on Stop froze for as long as that
/// took, which was forever.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_connected_client_stops_when_asked() {
    use tether_daemon::control;

    let dir = TempDir::new("stop");

    let (host_backend, _host_handle) =
        backend_with(vec![fake_monitor(0, 0, 0, HOST_WIDTH, HOST_HEIGHT, true)]);
    let (client_backend, _client_handle) = backend_with(vec![fake_monitor(
        0,
        0,
        0,
        CLIENT_WIDTH,
        CLIENT_HEIGHT,
        true,
    )]);

    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(host::run(
        host::Options {
            bind: "127.0.0.1:0".into(),
            pairing: true,
            config_path: dir.join("host.json"),
            advertise: false,
            ready: Some(ready_tx),
            control: None,
        },
        Config {
            name: "stop-host".into(),
            ..Config::default()
        },
        Identity::generate().unwrap(),
        host_backend,
    ));

    let addr = tokio::time::timeout(Duration::from_secs(5), ready_rx)
        .await
        .expect("host did not listen")
        .expect("ready channel dropped");

    let (daemon, ui) = control::channel();
    let client = tokio::spawn(clientmode::run(
        clientmode::Options {
            address: Some(addr.to_string()),
            pairing: true,
            config_path: dir.join("client.json"),
            control: Some(daemon),
        },
        Config {
            name: "stop-client".into(),
            ..Config::default()
        },
        Identity::generate().unwrap(),
        client_backend,
    ));

    // Wait until it is genuinely connected — stopping a client that never got
    // anywhere would pass for the wrong reason.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut connected = false;
    while tokio::time::Instant::now() < deadline {
        if !ui.status().peers.is_empty() {
            connected = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(connected, "the client never connected");

    assert!(
        ui.send(control::Command::Stop),
        "the daemon was already gone"
    );

    let stopped = tokio::time::timeout(Duration::from_secs(5), client).await;
    assert!(
        stopped.is_ok(),
        "a connected client ignored Stop and kept running"
    );
    stopped
        .unwrap()
        .expect("client task panicked")
        .expect("client returned an error");
}

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

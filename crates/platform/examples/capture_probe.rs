//! What does capture report, and does suppressing change it?
//!
//! Two kinds of movement event exist and the difference matters: while this
//! machine's own cursor is moving, its position is authoritative and is
//! reported; while the pointer is on another machine, the cursor is pinned and
//! only device movement means anything.
//!
//! `swallow` is what the daemon does while another machine has the pointer:
//! suppress the events *and* pin the cursor. Pass `nopin` alongside it to
//! suppress without pinning, which is what this program did before pinning
//! existed — the cursor drift it reports is the bug, visible as a mouse that
//! goes on sliding around this screen while you work on the other machine.
//!
//!   cargo run --example capture_probe -- [swallow] [nopin]

use std::time::Duration;

use tether_platform::LocalEvent;

fn main() {
    let swallow = std::env::args().any(|a| a == "swallow");
    let pin = swallow && !std::env::args().any(|a| a == "nopin");
    let mut backend = tether_platform::Backend::new(tether_platform::BackendKind::Native)
        .expect("native backend");

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    backend.capture.start(tx).expect("capture");

    let started_at = backend.pointer.position().ok();
    if pin {
        if let Err(err) = backend.pointer.set_pinned(true) {
            println!("could not pin the cursor: {err}");
        }
    }
    backend.capture.set_swallow(swallow);
    println!("capturing for 4s, suppressing={swallow}, pinned={pin}");
    println!("move the mouse now");

    let deadline = std::time::Instant::now() + Duration::from_secs(4);
    let (mut deltas, mut positioned, mut other, mut moved) = (0u32, 0u32, 0u32, 0i64);
    let (mut first, mut last) = (None, None);

    while std::time::Instant::now() < deadline {
        match rx.try_recv() {
            Ok(LocalEvent::MouseDelta { dx, dy }) => {
                deltas += 1;
                moved += (dx.abs() + dy.abs()) as i64;
            }
            Ok(LocalEvent::MouseMoved { x, y, dx, dy }) => {
                positioned += 1;
                moved += (dx.abs() + dy.abs()) as i64;
                first.get_or_insert((x, y));
                last = Some((x, y));
            }
            Ok(_) => other += 1,
            Err(_) => std::thread::sleep(Duration::from_millis(5)),
        }
    }

    // Read the cursor before letting go of it, or the answer is about the
    // release rather than about the run.
    let ended_at = backend.pointer.position().ok();
    backend.capture.set_swallow(false);
    let _ = backend.pointer.set_pinned(false);
    backend.capture.stop();

    println!("delta-only events : {deltas}");
    println!("positioned events : {positioned}");
    println!("other             : {other}");
    println!("total movement    : {moved}");
    println!("position first→last: {first:?} → {last:?}");
    println!(
        "own injections filtered: {}",
        backend.capture.injected_filtered()
    );

    if let (Some(from), Some(to)) = (started_at, ended_at) {
        let drift = (to.x - from.x).abs() + (to.y - from.y).abs();
        println!("cursor drift      : {drift} ({from:?} → {to:?})");
        if swallow {
            // The whole point of the exercise: movement was seen, and the
            // cursor stayed where it was.
            match (moved > 0, drift == 0) {
                (true, true) => {
                    println!("VERDICT: held still while movement kept arriving — correct")
                }
                (true, false) => println!(
                    "VERDICT: the cursor moved {drift}px while the pointer was supposed to be away"
                ),
                (false, _) => println!("VERDICT: inconclusive — no movement was captured at all"),
            }
        }
    }
}

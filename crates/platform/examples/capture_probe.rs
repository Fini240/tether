//! What does capture report, and does suppressing change it?
//!
//! Two kinds of movement event exist and the difference matters: while this
//! machine's own cursor is moving, its position is authoritative and is
//! reported; while the pointer is on another machine, the cursor is pinned and
//! only device movement means anything.
//!
//!   cargo run --example capture_probe -- [swallow]

use std::time::Duration;

use tether_platform::LocalEvent;

fn main() {
    let swallow = std::env::args().any(|a| a == "swallow");
    let mut backend = tether_platform::Backend::new(tether_platform::BackendKind::Native)
        .expect("native backend");

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    backend.capture.start(tx).expect("capture");
    backend.capture.set_swallow(swallow);
    println!("capturing for 4s, suppressing={swallow}");

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

    backend.capture.set_swallow(false);
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
}

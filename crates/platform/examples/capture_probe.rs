//! Does capture still report movement while it is suppressing?
//!
//! When the pointer is on another machine, this machine swallows its own input
//! and forwards the deltas instead. If suppression also stopped the deltas
//! arriving, the pointer could travel away and never be steered back — which
//! is exactly the "I can go one way but not back" symptom.
//!
//! Run with an argument to suppress:  cargo run --example capture_probe -- swallow

use std::time::Duration;

fn main() {
    let swallow = std::env::args().any(|a| a == "swallow");
    let mut backend = tether_platform::Backend::new(tether_platform::BackendKind::Native)
        .expect("native backend");

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    backend.capture.start(tx).expect("capture");
    backend.capture.set_swallow(swallow);
    println!("capturing for 4s, swallow={swallow}");

    let deadline = std::time::Instant::now() + Duration::from_secs(4);
    let (mut moves, mut other, mut total_dx) = (0u32, 0u32, 0i64);
    while std::time::Instant::now() < deadline {
        match rx.try_recv() {
            Ok(tether_platform::LocalEvent::MouseDelta { dx, dy }) => {
                moves += 1;
                total_dx += dx as i64 + dy as i64;
            }
            Ok(_) => other += 1,
            Err(_) => std::thread::sleep(Duration::from_millis(5)),
        }
    }

    backend.capture.set_swallow(false);
    backend.capture.stop();
    println!(
        "moves={moves} other={other} summed-delta={total_dx} filtered-own={}",
        backend.capture.injected_filtered()
    );
}

//! Which of our own injected events does the capture layer recognise as ours?
//!
//! `doctor` answers this for mouse *movement* only. If the mark is lost on any
//! other kind of event, the machine being driven reads it as a hand on its own
//! keyboard and grabs control back — so every kind that gets injected has to be
//! checked, not just the one that happens to be convenient to test.
//!
//! Clicks land somewhere, so the pointer is parked against the left edge of the
//! screen first and put back afterwards.
//!
//!   cargo run --example mark_probe

use std::time::Duration;

use tether_platform::LocalEvent;
use tether_proto::{InputEvent, KeyCode, Modifiers, MouseButton, Point};

fn main() {
    let mut backend = tether_platform::Backend::new(tether_platform::BackendKind::Native)
        .expect("native backend");

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    backend.capture.start(tx).expect("capture");

    let saved = backend.pointer.position().ok();
    let park = Point::new(3, 400);
    let _ = backend.pointer.warp(park);
    std::thread::sleep(Duration::from_millis(300));
    while rx.try_recv().is_ok() {}

    let cases: Vec<(&str, InputEvent)> = vec![
        ("MouseMove", InputEvent::MouseMove { x: 4, y: 400 }),
        (
            "MouseButton down",
            InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed: true,
            },
        ),
        (
            "MouseButton up",
            InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed: false,
            },
        ),
        ("MouseWheel", InputEvent::MouseWheel { dx: 0.0, dy: 1.0 }),
        (
            "Key down (F1)",
            InputEvent::Key {
                key: KeyCode::F1,
                pressed: true,
                modifiers: Modifiers::NONE,
                repeat: false,
            },
        ),
        (
            "Key up (F1)",
            InputEvent::Key {
                key: KeyCode::F1,
                pressed: false,
                modifiers: Modifiers::NONE,
                repeat: false,
            },
        ),
    ];

    println!("{:<18} {:>8}  leaked back as local", "injected", "filtered");
    println!("{}", "-".repeat(64));

    let mut leaks = 0;
    for (name, event) in cases {
        let before = backend.capture.injected_filtered();
        if let Err(err) = backend.inject.inject(&event) {
            println!("{name:<18} {:>8}  inject failed: {err}", "-");
            continue;
        }
        std::thread::sleep(Duration::from_millis(250));

        let filtered = backend.capture.injected_filtered() - before;
        let mut leaked = Vec::new();
        while let Ok(local) = rx.try_recv() {
            leaked.push(match local {
                LocalEvent::MouseMoved { .. } => "MouseMoved".to_string(),
                LocalEvent::MouseDelta { .. } => "MouseDelta".to_string(),
                LocalEvent::Button { pressed, .. } => format!("Button(pressed={pressed})"),
                LocalEvent::Wheel { .. } => "Wheel".to_string(),
                LocalEvent::Key { pressed, .. } => format!("Key(pressed={pressed})"),
            });
        }
        if !leaked.is_empty() {
            leaks += 1;
        }
        println!(
            "{name:<18} {filtered:>8}  {}",
            if leaked.is_empty() {
                "-".to_string()
            } else {
                leaked.join(", ")
            }
        );
    }

    if let Some(saved) = saved {
        let _ = backend.pointer.warp(saved);
    }
    backend.capture.stop();

    println!();
    if leaks == 0 {
        println!("VERDICT: every injected event was recognised as ours.");
    } else {
        println!(
            "VERDICT: {leaks} kind(s) leaked back as local input. A machine being \
             driven will read those as a hand on its own mouse and claim control."
        );
    }
}

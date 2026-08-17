//! Platform-independent brains: the screen layout, the cursor router that
//! decides when the pointer crosses machines, cross-platform key remapping, and
//! the on-disk configuration.
//!
//! Nothing in this crate touches the OS or the network. That is deliberate —
//! edge-switching maths is the part most likely to have subtle bugs, and here
//! it is all unit-testable without a second computer.

pub mod config;
pub mod hotkey;
pub mod keymap;
pub mod layout;
pub mod transition;

pub use layout::{Layout, Located, MachineId, Placement};
pub use transition::{CursorRouter, Transition};

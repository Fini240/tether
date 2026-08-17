//! Wire protocol shared by the host and every client.
//!
//! Everything that crosses the network is defined here and nowhere else. The
//! types are deliberately platform-neutral: keys travel as USB HID usage codes,
//! pointer positions travel in each machine's own desktop coordinate space, and
//! the receiving end is responsible for translating into whatever its OS wants.

pub mod clipboard;
pub mod codec;
pub mod geometry;
pub mod input;
pub mod message;

pub use clipboard::{ClipFormat, ClipboardContents, ClipboardStamp};
pub use codec::{Codec, CodecError};
pub use geometry::{MonitorId, MonitorInfo, Point, Rect};
pub use input::{InputEvent, KeyCode, Modifiers, MouseButton, SourceEvent};
pub use message::{Frame, Hello, MachinePlacement, Platform, Welcome};

/// Bumped on any breaking change to the types in this crate. The host refuses
/// clients that do not match, with a message naming both versions — a silent
/// desync here shows up as phantom keystrokes, which is miserable to debug.
pub const PROTOCOL_VERSION: u16 = 1;

/// Default TCP port. Also the port advertised over mDNS.
pub const DEFAULT_PORT: u16 = 24800;

/// mDNS service type used for auto-discovery.
pub const SERVICE_TYPE: &str = "_tether._tcp.local.";

//! Frames exchanged over the TLS connection.

use serde::{Deserialize, Serialize};

use crate::clipboard::{ClipFormat, ClipboardContents, ClipboardStamp};
use crate::geometry::{MonitorInfo, Point};
use crate::input::{InputEvent, SourceEvent};

/// Which OS a peer runs. Drives the modifier remap and nothing else — no
/// behaviour should branch on this beyond key translation and lock-screen
/// invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Platform {
    MacOS,
    Windows,
    Linux,
}

impl Platform {
    /// The platform this binary was compiled for.
    pub const fn current() -> Platform {
        #[cfg(target_os = "macos")]
        {
            Platform::MacOS
        }
        #[cfg(target_os = "windows")]
        {
            Platform::Windows
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            Platform::Linux
        }
    }
}

impl std::fmt::Display for Platform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Platform::MacOS => "macOS",
            Platform::Windows => "Windows",
            Platform::Linux => "Linux",
        };
        f.write_str(s)
    }
}

/// First frame a client sends after the TLS handshake completes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol_version: u16,
    /// Stable per-installation ID, derived from the machine's certificate.
    pub machine_id: u64,
    /// Name shown in the arrangement UI.
    pub name: String,
    pub platform: Platform,
    /// Every display attached to this client, in its own local coordinates.
    pub monitors: Vec<MonitorInfo>,
}

/// Host's reply. A client that receives `Welcome` is fully joined.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Welcome {
    pub protocol_version: u16,
    pub machine_id: u64,
    pub name: String,
    /// The host's platform — the client needs it to reverse the modifier remap.
    pub platform: Platform,
    /// Heartbeat cadence the host expects, in milliseconds.
    pub heartbeat_ms: u32,
}

/// One machine's place on the shared canvas, as the host sees it.
///
/// Sent to clients so they can show the arrangement too. A client has no say
/// in it — the host owns the canvas — but a client that cannot display it
/// leaves the user staring at a window showing only their own screen and no
/// way to tell whether the other machine is even attached.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MachinePlacement {
    pub machine: u64,
    pub name: String,
    pub platform: Platform,
    pub origin: Point,
    pub monitors: Vec<MonitorInfo>,
}

/// An in-flight file transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TransferId(pub u64);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Frame {
    // ---- session lifecycle ----
    Hello(Hello),
    Welcome(Welcome),
    /// Host refused the connection; the string is user-facing.
    Rejected(String),
    /// Either side may send. `Pong` echoes the payload so RTT is measurable.
    Ping(u64),
    Pong(u64),
    /// Clean shutdown. The peer should not treat this as a fault and should not
    /// back off before reconnecting later.
    Bye(String),

    // ---- cursor ownership ----
    /// The cursor has crossed onto this client. Coordinates are in the client's
    /// local space. It should show the cursor and start accepting input.
    Enter {
        monitor: crate::geometry::MonitorId,
        x: i32,
        y: i32,
    },
    /// The cursor has left this client. It should hide the cursor and release
    /// any keys it is currently holding down — see `Frame::ReleaseAll`.
    Leave,
    /// Force-release every held key and mouse button. Sent on `Leave`, on
    /// reconnect, and whenever the host loses confidence in its shadow state.
    /// Without this, a modifier held during a screen switch stays stuck down on
    /// the machine you just left.
    ReleaseAll,

    // ---- input ----
    Input(InputEvent),

    // ---- input ownership (which machine is being physically touched) ----
    /// "Somebody just touched my keyboard or trackpad; I should be driving."
    ///
    /// Sent by a client that detects genuine local input while another machine
    /// owns it. The host arbitrates — a client never assumes it won.
    ClaimInput,
    /// The host's ruling on who is driving. Broadcast to everyone, so the
    /// machine that just lost input knows to stop suppressing its own.
    InputOwner {
        machine: u64,
    },
    /// Raw captured input from whichever machine currently owns it. The host
    /// feeds this through the same router as its own capture.
    SourceInput(SourceEvent),

    // ---- clipboard ----
    /// "My clipboard changed; here is what I have." No bytes yet.
    ClipboardOffer {
        stamp: ClipboardStamp,
        formats: Vec<ClipFormat>,
    },
    /// "Send me those bytes" — emitted lazily, on a real paste.
    ClipboardRequest {
        stamp: ClipboardStamp,
        formats: Vec<ClipFormat>,
    },
    ClipboardData(ClipboardContents),

    // ---- file transfer ----
    FileOffer {
        transfer: TransferId,
        name: String,
        size: u64,
    },
    FileAccept {
        transfer: TransferId,
    },
    FileChunk {
        transfer: TransferId,
        offset: u64,
        #[serde(with = "serde_bytes_compat")]
        data: Vec<u8>,
    },
    FileDone {
        transfer: TransferId,
    },
    FileAbort {
        transfer: TransferId,
        reason: String,
    },

    /// The whole canvas, pushed on every change.
    Arrangement(Vec<MachinePlacement>),

    // ---- misc ----
    /// A client's displays changed (monitor plugged, resolution changed). The
    /// host recomputes the canvas.
    MonitorsChanged(Vec<MonitorInfo>),
    /// Engage the screen lock / screensaver on the receiving machine.
    LockScreen,
}

/// `Vec<u8>` round-trips fine through bincode already; this module exists so the
/// attribute above documents intent and gives us one place to swap in a
/// zero-copy representation later without touching the enum.
mod serde_bytes_compat {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(v)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        Vec::<u8>::deserialize(d)
    }
}

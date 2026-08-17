//! Clipboard payloads.
//!
//! Clipboard sync uses an offer/request handshake rather than pushing contents
//! on every copy: a 20 MB screenshot copied on the host should not be blasted
//! to four idle clients. The owner announces *what formats it has* plus a
//! monotonic stamp; a machine only pulls the bytes when a paste actually
//! happens there.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ClipFormat {
    /// UTF-8 plain text.
    Text,
    /// HTML fragment — the interchange format every platform's rich text can
    /// round-trip through. RTF is deliberately not on the wire: macOS is the
    /// only platform that treats it as a first-class flavour.
    Html,
    /// PNG-encoded image.
    Png,
    /// A list of file paths (a drag-and-drop or a Finder/Explorer copy). The
    /// bytes are not the files; see the file-transfer frames.
    FileList,
}

/// Identifies a particular clipboard state. Compared, never ordered across
/// machines — `(owner, seq)` is unique because only the owner increments it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardStamp {
    /// Machine that produced this clipboard state.
    pub owner: u64,
    /// Increments on every local clipboard change on that machine.
    pub seq: u64,
}

/// Actual clipboard bytes, one variant per format present.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClipboardContents {
    pub stamp: ClipboardStamp,
    pub text: Option<String>,
    pub html: Option<String>,
    pub png: Option<Vec<u8>>,
    pub files: Option<Vec<String>>,
}

impl ClipboardContents {
    pub fn empty(stamp: ClipboardStamp) -> Self {
        Self {
            stamp,
            text: None,
            html: None,
            png: None,
            files: None,
        }
    }

    pub fn formats(&self) -> Vec<ClipFormat> {
        let mut v = Vec::new();
        if self.text.is_some() {
            v.push(ClipFormat::Text);
        }
        if self.html.is_some() {
            v.push(ClipFormat::Html);
        }
        if self.png.is_some() {
            v.push(ClipFormat::Png);
        }
        if self.files.is_some() {
            v.push(ClipFormat::FileList);
        }
        v
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_none() && self.html.is_none() && self.png.is_none() && self.files.is_none()
    }
}

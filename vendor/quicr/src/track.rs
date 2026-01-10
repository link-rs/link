//! Track naming types for MoQ pub/sub

use bytes::Bytes;
use core::fmt;

#[cfg(not(feature = "std"))]
use alloc::{string::String, vec::Vec};

/// A namespace tuple for MoQ tracks
///
/// TrackNamespace represents an N-tuple of byte sequences that identify
/// a namespace hierarchy. For example, `["chat", "room1"]` could represent
/// a chat room namespace.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct TrackNamespace {
    entries: Vec<Bytes>,
}

#[cfg(feature = "defmt-logging")]
impl defmt::Format for TrackNamespace {
    fn format(&self, f: defmt::Formatter) {
        // Format as a path-like string representation
        for (i, entry) in self.entries.iter().enumerate() {
            if i > 0 {
                defmt::write!(f, "/");
            }
            if let Ok(s) = core::str::from_utf8(entry) {
                defmt::write!(f, "{}", s);
            } else {
                defmt::write!(f, "<bytes>");
            }
        }
    }
}

impl TrackNamespace {
    /// Create a new empty track namespace
    pub fn new() -> Self {
        Self { entries: Vec::new() }
    }

    /// Create a track namespace from string slices
    ///
    /// # Example
    ///
    /// ```
    /// use quicr::TrackNamespace;
    ///
    /// let ns = TrackNamespace::from_strings(&["chat", "room1"]);
    /// assert_eq!(ns.len(), 2);
    /// ```
    pub fn from_strings(entries: &[&str]) -> Self {
        Self {
            entries: entries.iter().map(|s| Bytes::copy_from_slice(s.as_bytes())).collect(),
        }
    }

    /// Create a track namespace from byte slices
    pub fn from_bytes(entries: &[&[u8]]) -> Self {
        Self {
            entries: entries.iter().map(|b| Bytes::copy_from_slice(b)).collect(),
        }
    }

    /// Add an entry to the namespace
    pub fn push(&mut self, entry: impl Into<Bytes>) {
        self.entries.push(entry.into());
    }

    /// Get the number of entries in the namespace
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Check if the namespace is empty
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Get the entries as a slice
    pub fn entries(&self) -> &[Bytes] {
        &self.entries
    }

    /// Check if this namespace is a prefix of another
    pub fn is_prefix_of(&self, other: &TrackNamespace) -> bool {
        if self.entries.len() > other.entries.len() {
            return false;
        }
        self.entries.iter().zip(&other.entries).all(|(a, b)| a == b)
    }

    /// Convert to FFI representation
    pub(crate) fn to_ffi(&self) -> (Vec<crate::ffi::QuicrBytes>, crate::ffi::QuicrTrackNamespace) {
        let ffi_entries: Vec<crate::ffi::QuicrBytes> = self
            .entries
            .iter()
            .map(|e| crate::ffi::QuicrBytes {
                data: e.as_ptr(),
                len: e.len(),
            })
            .collect();

        let ns = crate::ffi::QuicrTrackNamespace {
            entries: ffi_entries.as_ptr() as *mut _,
            num_entries: ffi_entries.len(),
        };

        (ffi_entries, ns)
    }
}

impl Default for TrackNamespace {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for TrackNamespace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let entries: Vec<String> = self
            .entries
            .iter()
            .map(|e| String::from_utf8_lossy(e).into_owned())
            .collect();
        f.debug_tuple("TrackNamespace").field(&entries).finish()
    }
}

impl fmt::Display for TrackNamespace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let entries: Vec<String> = self
            .entries
            .iter()
            .map(|e| String::from_utf8_lossy(e).into_owned())
            .collect();
        write!(f, "{}", entries.join("/"))
    }
}

/// Full track name consisting of a namespace and a track name
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct FullTrackName {
    /// The namespace tuple
    pub namespace: TrackNamespace,
    /// The track name within the namespace
    pub name: Bytes,
}

#[cfg(feature = "defmt-logging")]
impl defmt::Format for FullTrackName {
    fn format(&self, f: defmt::Formatter) {
        // Format namespace first
        self.namespace.format(f);
        // Add separator and track name
        defmt::write!(f, "/");
        if let Ok(s) = core::str::from_utf8(&self.name) {
            defmt::write!(f, "{}", s);
        } else {
            defmt::write!(f, "<bytes>");
        }
    }
}

impl FullTrackName {
    /// Create a new full track name
    ///
    /// # Example
    ///
    /// ```
    /// use quicr::{FullTrackName, TrackNamespace};
    ///
    /// let track = FullTrackName::new(
    ///     TrackNamespace::from_strings(&["chat", "room1"]),
    ///     "messages",
    /// );
    /// ```
    pub fn new(namespace: TrackNamespace, name: impl Into<Bytes>) -> Self {
        Self {
            namespace,
            name: name.into(),
        }
    }

    /// Create from string components
    pub fn from_strings(namespace: &[&str], name: &str) -> Self {
        Self {
            namespace: TrackNamespace::from_strings(namespace),
            name: Bytes::copy_from_slice(name.as_bytes()),
        }
    }

    /// Convert to FFI representation
    pub(crate) fn to_ffi(
        &self,
    ) -> (
        Vec<crate::ffi::QuicrBytes>,
        crate::ffi::QuicrFullTrackName,
    ) {
        let (ffi_entries, ns) = self.namespace.to_ffi();

        let ftn = crate::ffi::QuicrFullTrackName {
            name_space: ns,
            name: crate::ffi::QuicrBytes {
                data: self.name.as_ptr(),
                len: self.name.len(),
            },
        };

        (ffi_entries, ftn)
    }
}

impl fmt::Debug for FullTrackName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FullTrackName")
            .field("namespace", &self.namespace)
            .field("name", &String::from_utf8_lossy(&self.name))
            .finish()
    }
}

impl fmt::Display for FullTrackName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}/{}",
            self.namespace,
            String::from_utf8_lossy(&self.name)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_track_namespace_from_strings() {
        let ns = TrackNamespace::from_strings(&["chat", "room1"]);
        assert_eq!(ns.len(), 2);
        assert!(!ns.is_empty());
        assert_eq!(ns.entries()[0].as_ref(), b"chat");
        assert_eq!(ns.entries()[1].as_ref(), b"room1");
    }

    #[test]
    fn test_track_namespace_from_bytes() {
        let ns = TrackNamespace::from_bytes(&[b"chat", b"room1"]);
        assert_eq!(ns.len(), 2);
    }

    #[test]
    fn test_empty_namespace() {
        let ns = TrackNamespace::new();
        assert!(ns.is_empty());
        assert_eq!(ns.len(), 0);
    }

    #[test]
    fn test_namespace_push() {
        let mut ns = TrackNamespace::new();
        ns.push("chat");
        ns.push("room1");
        assert_eq!(ns.len(), 2);
    }

    #[test]
    fn test_track_namespace_prefix() {
        let ns1 = TrackNamespace::from_strings(&["chat"]);
        let ns2 = TrackNamespace::from_strings(&["chat", "room1"]);
        let ns3 = TrackNamespace::from_strings(&["video", "room1"]);

        assert!(ns1.is_prefix_of(&ns2));
        assert!(!ns2.is_prefix_of(&ns1));
        assert!(!ns1.is_prefix_of(&ns3));
    }

    #[test]
    fn test_namespace_display() {
        let ns = TrackNamespace::from_strings(&["chat", "room1"]);
        assert_eq!(ns.to_string(), "chat/room1");
    }

    #[test]
    fn test_full_track_name() {
        let track = FullTrackName::from_strings(&["chat", "room1"], "messages");
        assert_eq!(track.namespace.len(), 2);
        assert_eq!(track.name.as_ref(), b"messages");
        assert_eq!(track.to_string(), "chat/room1/messages");
    }

    #[test]
    fn test_full_track_name_equality() {
        let track1 = FullTrackName::from_strings(&["chat", "room1"], "messages");
        let track2 = FullTrackName::from_strings(&["chat", "room1"], "messages");
        let track3 = FullTrackName::from_strings(&["chat", "room2"], "messages");

        assert_eq!(track1, track2);
        assert_ne!(track1, track3);
    }
}

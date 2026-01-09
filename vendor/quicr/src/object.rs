//! Object types for MoQ data transfer

use bytes::Bytes;

/// Status of an object as reported by the publisher
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ObjectStatus {
    /// Object is available
    Available = 0,
    /// Object does not exist
    DoesNotExist = 1,
    /// Marks end of group
    EndOfGroup = 3,
    /// Marks end of track
    EndOfTrack = 4,
    /// Marks end of subgroup
    EndOfSubGroup = 5,
}

impl From<crate::ffi::QuicrObjectStatus> for ObjectStatus {
    fn from(status: crate::ffi::QuicrObjectStatus) -> Self {
        match status {
            crate::ffi::QuicrObjectStatus_QUICR_OBJECT_STATUS_AVAILABLE => ObjectStatus::Available,
            crate::ffi::QuicrObjectStatus_QUICR_OBJECT_STATUS_DOES_NOT_EXIST => ObjectStatus::DoesNotExist,
            crate::ffi::QuicrObjectStatus_QUICR_OBJECT_STATUS_END_OF_GROUP => ObjectStatus::EndOfGroup,
            crate::ffi::QuicrObjectStatus_QUICR_OBJECT_STATUS_END_OF_TRACK => ObjectStatus::EndOfTrack,
            crate::ffi::QuicrObjectStatus_QUICR_OBJECT_STATUS_END_OF_SUBGROUP => ObjectStatus::EndOfSubGroup,
            _ => ObjectStatus::Available,
        }
    }
}

impl From<ObjectStatus> for crate::ffi::QuicrObjectStatus {
    fn from(status: ObjectStatus) -> Self {
        match status {
            ObjectStatus::Available => crate::ffi::QuicrObjectStatus_QUICR_OBJECT_STATUS_AVAILABLE,
            ObjectStatus::DoesNotExist => crate::ffi::QuicrObjectStatus_QUICR_OBJECT_STATUS_DOES_NOT_EXIST,
            ObjectStatus::EndOfGroup => crate::ffi::QuicrObjectStatus_QUICR_OBJECT_STATUS_END_OF_GROUP,
            ObjectStatus::EndOfTrack => crate::ffi::QuicrObjectStatus_QUICR_OBJECT_STATUS_END_OF_TRACK,
            ObjectStatus::EndOfSubGroup => crate::ffi::QuicrObjectStatus_QUICR_OBJECT_STATUS_END_OF_SUBGROUP,
        }
    }
}

/// Track mode for publishing
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum TrackMode {
    /// Send objects as datagrams (unreliable)
    Datagram = 0,
    /// Send objects over streams (reliable)
    #[default]
    Stream = 1,
}

impl From<crate::ffi::QuicrTrackMode> for TrackMode {
    fn from(mode: crate::ffi::QuicrTrackMode) -> Self {
        match mode {
            crate::ffi::QuicrTrackMode_QUICR_TRACK_MODE_DATAGRAM => TrackMode::Datagram,
            _ => TrackMode::Stream,
        }
    }
}

impl From<TrackMode> for crate::ffi::QuicrTrackMode {
    fn from(mode: TrackMode) -> Self {
        match mode {
            TrackMode::Datagram => crate::ffi::QuicrTrackMode_QUICR_TRACK_MODE_DATAGRAM,
            TrackMode::Stream => crate::ffi::QuicrTrackMode_QUICR_TRACK_MODE_STREAM,
        }
    }
}

/// Group ordering for subscriptions
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum GroupOrder {
    /// Ascending group order
    #[default]
    Ascending = 0,
    /// Descending group order
    Descending = 1,
}

impl From<GroupOrder> for crate::ffi::QuicrGroupOrder {
    fn from(order: GroupOrder) -> Self {
        match order {
            GroupOrder::Ascending => crate::ffi::QuicrGroupOrder_QUICR_GROUP_ORDER_ASCENDING,
            GroupOrder::Descending => crate::ffi::QuicrGroupOrder_QUICR_GROUP_ORDER_DESCENDING,
        }
    }
}

/// Filter type for subscriptions
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum FilterType {
    /// Start from the next group
    NextGroupStart = 1,
    /// Start from the largest object (default)
    #[default]
    LargestObject = 2,
    /// Start from absolute position
    AbsoluteStart = 3,
    /// Absolute range
    AbsoluteRange = 4,
}

impl From<FilterType> for crate::ffi::QuicrFilterType {
    fn from(filter: FilterType) -> Self {
        match filter {
            FilterType::NextGroupStart => {
                crate::ffi::QuicrFilterType_QUICR_FILTER_TYPE_NEXT_GROUP_START
            }
            FilterType::LargestObject => {
                crate::ffi::QuicrFilterType_QUICR_FILTER_TYPE_LARGEST_OBJECT
            }
            FilterType::AbsoluteStart => {
                crate::ffi::QuicrFilterType_QUICR_FILTER_TYPE_ABSOLUTE_START
            }
            FilterType::AbsoluteRange => {
                crate::ffi::QuicrFilterType_QUICR_FILTER_TYPE_ABSOLUTE_RANGE
            }
        }
    }
}

/// Object headers describing a published/received object
#[derive(Debug, Clone)]
pub struct ObjectHeaders {
    /// Group ID - Application defined order of generation
    pub group_id: u64,
    /// Object ID - Application defined order of generation
    pub object_id: u64,
    /// Subgroup ID - Starts at 0, monotonically increases by 1
    pub subgroup_id: u64,
    /// Length of payload data
    pub payload_length: u64,
    /// Status of the object at the publisher
    pub status: ObjectStatus,
    /// Priority of the object (lower is higher priority)
    pub priority: Option<u8>,
    /// Time-to-live in milliseconds
    pub ttl: Option<u16>,
    /// Track mode for this object
    pub track_mode: Option<TrackMode>,
}

impl ObjectHeaders {
    /// Create new object headers with default values
    pub fn new(group_id: u64, object_id: u64) -> Self {
        Self {
            group_id,
            object_id,
            subgroup_id: 0,
            payload_length: 0,
            status: ObjectStatus::Available,
            priority: None,
            ttl: None,
            track_mode: None,
        }
    }

    /// Create a builder for object headers
    pub fn builder() -> ObjectHeadersBuilder {
        ObjectHeadersBuilder::default()
    }

    /// Convert to FFI representation
    pub(crate) fn to_ffi(&self) -> crate::ffi::QuicrObjectHeaders {
        crate::ffi::QuicrObjectHeaders {
            group_id: self.group_id,
            object_id: self.object_id,
            subgroup_id: self.subgroup_id,
            payload_length: self.payload_length,
            status: self.status.into(),
            priority: self.priority.unwrap_or(0),
            has_priority: self.priority.is_some(),
            ttl: self.ttl.unwrap_or(0),
            has_ttl: self.ttl.is_some(),
            track_mode: self.track_mode.map(Into::into).unwrap_or(0),
            has_track_mode: self.track_mode.is_some(),
        }
    }
}

impl From<&crate::ffi::QuicrObjectHeaders> for ObjectHeaders {
    fn from(ffi: &crate::ffi::QuicrObjectHeaders) -> Self {
        Self {
            group_id: ffi.group_id,
            object_id: ffi.object_id,
            subgroup_id: ffi.subgroup_id,
            payload_length: ffi.payload_length,
            status: ffi.status.into(),
            priority: if ffi.has_priority {
                Some(ffi.priority)
            } else {
                None
            },
            ttl: if ffi.has_ttl { Some(ffi.ttl) } else { None },
            track_mode: if ffi.has_track_mode {
                Some(ffi.track_mode.into())
            } else {
                None
            },
        }
    }
}

/// Builder for ObjectHeaders
#[derive(Debug, Default)]
pub struct ObjectHeadersBuilder {
    group_id: u64,
    object_id: u64,
    subgroup_id: u64,
    status: ObjectStatus,
    priority: Option<u8>,
    ttl: Option<u16>,
    track_mode: Option<TrackMode>,
}

impl ObjectHeadersBuilder {
    /// Set the group ID
    pub fn group_id(mut self, id: u64) -> Self {
        self.group_id = id;
        self
    }

    /// Set the object ID
    pub fn object_id(mut self, id: u64) -> Self {
        self.object_id = id;
        self
    }

    /// Set the subgroup ID
    pub fn subgroup_id(mut self, id: u64) -> Self {
        self.subgroup_id = id;
        self
    }

    /// Set the object status
    pub fn status(mut self, status: ObjectStatus) -> Self {
        self.status = status;
        self
    }

    /// Set the priority (lower is higher priority)
    pub fn priority(mut self, priority: u8) -> Self {
        self.priority = Some(priority);
        self
    }

    /// Set the TTL in milliseconds
    pub fn ttl(mut self, ttl: u16) -> Self {
        self.ttl = Some(ttl);
        self
    }

    /// Set the track mode
    pub fn track_mode(mut self, mode: TrackMode) -> Self {
        self.track_mode = Some(mode);
        self
    }

    /// Build the ObjectHeaders with the given payload length
    pub fn build(self, payload_length: u64) -> ObjectHeaders {
        ObjectHeaders {
            group_id: self.group_id,
            object_id: self.object_id,
            subgroup_id: self.subgroup_id,
            payload_length,
            status: self.status,
            priority: self.priority,
            ttl: self.ttl,
            track_mode: self.track_mode,
        }
    }
}

impl Default for ObjectStatus {
    fn default() -> Self {
        ObjectStatus::Available
    }
}

/// A received object with headers and data
#[derive(Debug, Clone)]
pub struct ReceivedObject {
    /// Object headers
    pub headers: ObjectHeaders,
    /// Object payload data
    pub data: Bytes,
}

impl ReceivedObject {
    /// Create a new received object
    pub fn new(headers: ObjectHeaders, data: Bytes) -> Self {
        Self { headers, data }
    }

    /// Get the payload as a byte slice
    pub fn payload(&self) -> &[u8] {
        &self.data
    }

    /// Get the payload as a UTF-8 string (if valid)
    pub fn payload_str(&self) -> Option<&str> {
        core::str::from_utf8(&self.data).ok()
    }
}

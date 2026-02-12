//! Message type enums defining the communication protocol between chips.

use num_enum::{IntoPrimitive, TryFromPrimitive};

use serde::{Deserialize, Serialize};

/// Channel ID for routing messages (matches hactar ui_net_link.hh)
#[derive(Copy, Clone, PartialEq, Eq, Debug, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[repr(u8)]
pub enum ChannelId {
    /// Push-to-talk audio (human voice) - Button A
    Ptt = 0,
    /// AI audio channel (AI-generated voice) - Button B
    PttAi = 1,
    // Chat = 2 is reserved but not implemented
    /// AI text/JSON responses for track reconfiguration
    ChatAi = 3,
}

/// Message type within a chunk (matches hactar ui_net_link.hh)
#[derive(Copy, Clone, PartialEq, Eq, Debug, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[repr(u8)]
pub enum MessageType {
    /// Regular audio data
    Media = 1,
    /// Audio to AI
    AiRequest = 2,
    /// Response from AI
    AiResponse = 3,
}

/// Loopback mode for audio testing on the UI chip.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[repr(u8)]
pub enum UiLoopbackMode {
    /// No loopback - normal operation, audio sent to NET
    #[default]
    Off = 0,
    /// Raw loopback - before A-law encoding (stereo PCM directly to speaker)
    Raw = 1,
    /// A-law loopback - after encoding, before SFrame (encode then decode)
    Alaw = 2,
    /// SFrame loopback - full encryption round-trip (encode → encrypt → decrypt → decode)
    Sframe = 3,
}

/// Loopback mode for the NET chip.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[repr(u8)]
pub enum NetLoopbackMode {
    /// Normal operation - audio to MoQ, filter self-echo
    #[default]
    Off = 0,
    /// Local bypass - audio directly back to UI (no MoQ)
    Raw = 1,
    /// MoQ loopback - audio to MoQ, DON'T filter self-echo (hear own audio via relay)
    Moq = 2,
}

/// Pin identifiers for SetPin command.
#[derive(Copy, Clone, PartialEq, Eq, Debug, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[repr(u8)]
pub enum Pin {
    /// UI chip BOOT0 pin
    UiBoot0 = 0,
    /// UI chip BOOT1 pin
    UiBoot1 = 1,
    /// UI chip RST pin
    UiRst = 2,
    /// NET chip GPIO0/BOOT pin
    NetBoot = 3,
    /// NET chip EN/RST pin
    NetRst = 4,
}

/// Pin value for SetPin command.
#[derive(Copy, Clone, PartialEq, Eq, Debug, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[repr(u8)]
pub enum PinValue {
    /// Pin low (for BOOT: normal mode; for RST: hold in reset)
    Low = 0,
    /// Pin high (for BOOT: bootloader mode; for RST: run)
    High = 1,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, IntoPrimitive, TryFromPrimitive)]
#[repr(u16)]
pub enum CtlToMgmt {
    Ping = 0x00,
    ToUi,
    ToNet,
    /// Hello handshake for device detection (4 bytes, XOR'd with b"LINK")
    Hello,
    /// Set a pin (2 bytes: pin enum, value 0=low/1=high)
    SetPin,
    /// Set NET UART baud rate (4 bytes: u32 big-endian)
    SetNetBaudRate,
    /// Set UI UART baud rate (4 bytes: u32 big-endian)
    SetUiBaudRate,
    /// Set CTL UART baud rate (4 bytes: u32 big-endian)
    /// ACK is sent before the baud rate change takes effect.
    SetCtlBaudRate,
    /// Get MGMT chip stack usage information
    GetStackInfo,
    /// Repaint the MGMT chip stack with the paint pattern
    RepaintStack,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, IntoPrimitive, TryFromPrimitive)]
#[repr(u16)]
pub enum MgmtToCtl {
    Pong = 0x10,
    FromUi,
    FromNet,
    Ack,
    /// Hello response (4 bytes XOR'd with b"LINK")
    Hello,
    /// Stack usage information (postcard-serialized StackInfo)
    StackInfo,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, IntoPrimitive, TryFromPrimitive)]
#[repr(u16)]
pub enum CtlToUi {
    Ping = 0x20,
    CircularPing,
    GetVersion,
    SetVersion,
    GetSFrameKey,
    SetSFrameKey,
    /// Set loopback mode (1 byte: UiLoopbackMode)
    SetLoopback,
    /// Get loopback mode
    GetLoopback,
    /// Get stack usage information
    GetStackInfo,
    /// Repaint the stack with the paint pattern
    RepaintStack,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, IntoPrimitive, TryFromPrimitive)]
#[repr(u16)]
pub enum UiToCtl {
    Pong = 0x30,
    CircularPing,
    Version,
    SFrameKey,
    Ack,
    Error,
    /// Loopback mode status (1 byte: UiLoopbackMode)
    Loopback,
    /// Debug log message (UTF-8 string)
    Log,
    /// Stack usage information (postcard-serialized StackInfo)
    StackInfo,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, IntoPrimitive, TryFromPrimitive)]
#[repr(u16)]
pub enum CtlToNet {
    Ping = 0x40,
    CircularPing,
    AddWifiSsid,
    GetWifiSsids,
    ClearWifiSsids,
    GetRelayUrl,
    SetRelayUrl,
    /// Set loopback mode (1 byte: NetLoopbackMode - 0=Off, 1=Raw, 2=Moq)
    SetLoopback,
    /// Get loopback mode
    GetLoopback,
    // Channel configuration commands
    /// Get channel configuration (value: channel_id u8)
    GetChannelConfig,
    /// Set channel configuration (value: postcard-serialized ChannelConfig)
    SetChannelConfig,
    /// Clear all channel configurations (no payload)
    ClearChannelConfigs,
    /// Get jitter buffer stats for a channel (value: channel_id u8)
    GetJitterStats,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, IntoPrimitive, TryFromPrimitive)]
#[repr(u16)]
pub enum NetToCtl {
    Pong = 0x50,
    CircularPing,
    WifiSsids,
    RelayUrl,
    Ack,
    Error,
    /// Loopback mode status (1 byte: NetLoopbackMode - 0=Off, 1=Raw, 2=Moq)
    Loopback,
    // Channel configuration responses
    /// Channel configuration (value: postcard-serialized ChannelConfig)
    ChannelConfig,
    /// Jitter buffer statistics (postcard-serialized JitterStatsInfo)
    JitterStats,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, IntoPrimitive, TryFromPrimitive)]
#[repr(u16)]
pub enum UiToNet {
    CircularPing = 0x60,
    /// Audio frame with channel_id prefix + encrypted chunk (hactar format)
    /// Format: [channel_id: u8][sframe_header][encrypted_chunk][auth_tag]
    AudioFrame,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, IntoPrimitive, TryFromPrimitive)]
#[repr(u16)]
pub enum NetToUi {
    CircularPing = 0x70,
    /// Audio frame to play out
    AudioFrame,
}

/// Stack usage information (wire format, postcard-serialized).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct StackInfo {
    /// Stack base address (highest address, start of stack memory).
    pub stack_base: u32,
    /// Stack top address (lowest address, end of stack memory).
    pub stack_top: u32,
    /// Total stack size in bytes.
    pub stack_size: u32,
    /// Stack usage (bytes from top to high-water mark).
    pub stack_used: u32,
}

impl StackInfo {
    pub fn stack_free(&self) -> u32 {
        self.stack_size.saturating_sub(self.stack_used)
    }
    pub fn usage_percent(&self) -> f64 {
        if self.stack_size > 0 {
            (self.stack_used as f64 / self.stack_size as f64) * 100.0
        } else {
            0.0
        }
    }
    /// Serialize to postcard into the provided buffer.
    pub fn to_bytes<'a>(&self, buf: &'a mut [u8]) -> Option<&'a [u8]> {
        postcard::to_slice(self, buf).ok().map(|s| &*s)
    }
    /// Deserialize from postcard bytes.
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        postcard::from_bytes(data).ok()
    }
}

/// Jitter buffer statistics (wire format, postcard-serialized).
///
/// This is the wire-format struct used for TLV communication.
/// The internal `JitterStats` in jitter_buffer.rs uses `usize` for level
/// and gets converted to this when serializing for the wire.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct JitterStatsInfo {
    /// Total frames received.
    pub received: u32,
    /// Total frames output.
    pub output: u32,
    /// Number of underruns (had to output silence).
    pub underruns: u32,
    /// Number of overruns (had to drop frames).
    pub overruns: u32,
    /// Current buffer level.
    pub level: u16,
    /// Current state (0=Buffering, 1=Playing).
    pub state: u8,
}

impl JitterStatsInfo {
    /// Serialize to postcard into the provided buffer.
    pub fn to_bytes<'a>(&self, buf: &'a mut [u8]) -> Option<&'a [u8]> {
        postcard::to_slice(self, buf).ok().map(|s| &*s)
    }
    /// Deserialize from postcard bytes.
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        postcard::from_bytes(data).ok()
    }
}

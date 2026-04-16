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

impl ChannelId {
    /// All defined channel IDs.
    pub const ALL: &[ChannelId] = &[ChannelId::Ptt, ChannelId::PttAi, ChannelId::ChatAi];
}

impl core::fmt::Display for ChannelId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ChannelId::Ptt => write!(f, "Ptt"),
            ChannelId::PttAi => write!(f, "PttAi"),
            ChannelId::ChatAi => write!(f, "ChatAi"),
        }
    }
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

impl core::fmt::Display for UiLoopbackMode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            UiLoopbackMode::Off => write!(f, "off"),
            UiLoopbackMode::Raw => write!(f, "raw"),
            UiLoopbackMode::Alaw => write!(f, "alaw"),
            UiLoopbackMode::Sframe => write!(f, "sframe"),
        }
    }
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

impl core::fmt::Display for NetLoopbackMode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            NetLoopbackMode::Off => write!(f, "off"),
            NetLoopbackMode::Raw => write!(f, "raw"),
            NetLoopbackMode::Moq => write!(f, "moq"),
        }
    }
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
    /// Set UI UART baud rate (4 bytes: u32 big-endian)
    SetUiBaudRate,
    /// Get MGMT chip stack usage information
    GetStackInfo,
    /// Get the board version from MGMT option bytes
    GetBoardVersion,
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
    /// Stack usage information (JSON-serialized StackInfo)
    StackInfo,
    /// Board version from MGMT option bytes (1 byte)
    BoardVersion,
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
    /// Get logs enabled state (returns LogsEnabled)
    GetLogsEnabled,
    /// Set logs enabled state (1 byte: 0=disabled, 1=enabled)
    SetLogsEnabled,
    /// Clear all stored configuration (EEPROM)
    ClearStorage,
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
    /// Stack usage information (JSON-serialized StackInfo)
    StackInfo,
    /// Logs enabled state (1 byte: 0=disabled, 1=enabled)
    LogsEnabled,
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
    /// Get logs enabled state (returns LogsEnabled)
    GetLogsEnabled,
    /// Set logs enabled state (1 byte: 0=disabled, 1=enabled)
    SetLogsEnabled,
    /// Clear all stored configuration (NVS)
    ClearStorage,
    /// Get language setting (returns Language)
    GetLanguage,
    /// Set language setting (UTF-8 string, e.g. "en-US")
    SetLanguage,
    /// Get channel configuration (returns Channel)
    GetChannel,
    /// Set channel configuration (JSON array: ["relay","org","channel","ptt"])
    SetChannel,
    /// Get AI configuration (returns Ai)
    GetAi,
    /// Set AI configuration (JSON object: {"query":[...],"audio":[...],"cmd":[...]})
    SetAi,
    /// Burn JTAG/USB disable efuse (IRREVERSIBLE!)
    BurnJtagEfuse,
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
    /// Logs enabled state (1 byte: 0=disabled, 1=enabled)
    LogsEnabled,
    /// Language setting (UTF-8 string)
    Language,
    /// Channel configuration (JSON array)
    Channel,
    /// AI configuration (JSON object)
    Ai,
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

/// Stack usage information (wire format, JSON-serialized).
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
    /// Serialize to JSON into the provided buffer.
    pub fn to_bytes<'a>(&self, buf: &'a mut [u8]) -> Option<&'a [u8]> {
        serde_json_core::to_slice(self, buf)
            .ok()
            .map(|len| &buf[..len])
    }
    /// Deserialize from JSON bytes.
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        serde_json_core::from_slice(data).ok().map(|(v, _)| v)
    }
}

/// Jitter buffer state.
#[derive(
    Copy,
    Clone,
    PartialEq,
    Eq,
    Debug,
    Default,
    IntoPrimitive,
    TryFromPrimitive,
    Serialize,
    Deserialize,
)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[repr(u8)]
pub enum JitterState {
    #[default]
    Buffering = 0,
    Playing = 1,
}

impl core::fmt::Display for JitterState {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            JitterState::Buffering => write!(f, "Buffering"),
            JitterState::Playing => write!(f, "Playing"),
        }
    }
}

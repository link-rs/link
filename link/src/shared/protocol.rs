//! Message type enums defining the communication protocol between chips.

use num_enum::{IntoPrimitive, TryFromPrimitive};

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
pub enum LoopbackMode {
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
pub enum NetLoopback {
    /// Normal operation - audio to MoQ, filter self-echo
    #[default]
    Off = 0,
    /// Local bypass - audio directly back to UI (no MoQ)
    Raw = 1,
    /// MoQ loopback - audio to MoQ, DON'T filter self-echo (hear own audio via relay)
    Moq = 2,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, IntoPrimitive, TryFromPrimitive)]
#[repr(u16)]
pub enum CtlToMgmt {
    Ping = 0x00,
    ToUi,
    ToNet,
    /// Reset UI chip into bootloader mode (BOOT0 high, then reset)
    ResetUiToBootloader,
    /// Reset UI chip into user mode (BOOT0 low, then reset)
    ResetUiToUser,
    /// Reset NET chip into bootloader mode (BOOT0 high, then reset)
    ResetNetToBootloader,
    /// Reset NET chip into user mode (BOOT0 low, then reset)
    ResetNetToUser,
    /// Hello handshake for device detection (4 bytes, XOR'd with b"LINK")
    Hello,
    /// Hold UI chip in reset
    HoldUiReset,
    /// Hold NET chip in reset
    HoldNetReset,
    /// Set NET chip GPIO0/BOOT pin directly (1 byte: 0=low, 1=high)
    /// Low = bootloader mode when reset is released
    SetNetBoot,
    /// Set NET chip EN/RST pin directly (1 byte: 0=low/reset, 1=high/run)
    SetNetRst,
    /// Set NET UART baud rate (4 bytes: u32 little-endian)
    SetNetBaudRate,
    /// Set CTL UART baud rate (4 bytes: u32 little-endian)
    /// ACK is sent before the baud rate change takes effect.
    SetCtlBaudRate,
    /// Speed test data packet (value contains test payload)
    SpeedTestData,
    /// Speed test done signal (no payload)
    SpeedTestDone,
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
    /// CTL-MGMT speed test results (8 bytes: packet_count u32 LE, total_bytes u32 LE)
    SpeedTestResult,
    /// Stack usage information (16 bytes: stack_base u32 LE, stack_top u32 LE, stack_size u32 LE, stack_used u32 LE)
    StackInfo,
    /// Stack repaint acknowledgement (8 bytes: painted_start u32 LE, painted_end u32 LE)
    StackRepainted,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, IntoPrimitive, TryFromPrimitive)]
#[repr(u16)]
pub enum MgmtToUi {
    Ping = 0x20,
    CircularPing,
    GetVersion,
    SetVersion,
    GetSFrameKey,
    SetSFrameKey,
    /// Set loopback mode (1 byte: LoopbackMode)
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
pub enum UiToMgmt {
    Pong = 0x30,
    CircularPing,
    Version,
    SFrameKey,
    Ack,
    Error,
    /// Loopback mode status (1 byte: LoopbackMode)
    Loopback,
    /// Debug log message (UTF-8 string)
    Log,
    /// Stack usage information (16 bytes: stack_base u32 LE, stack_top u32 LE, stack_size u32 LE, stack_used u32 LE)
    StackInfo,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, IntoPrimitive, TryFromPrimitive)]
#[repr(u16)]
pub enum MgmtToNet {
    Ping = 0x40,
    CircularPing,
    AddWifiSsid,
    GetWifiSsids,
    ClearWifiSsids,
    GetRelayUrl,
    SetRelayUrl,
    /// Set loopback mode (1 byte: NetLoopback - 0=Off, 1=Raw, 2=Moq)
    SetLoopback,
    /// Get loopback mode
    GetLoopback,
    /// Send chat message (value: UTF-8 message)
    SendChatMessage,
    // Channel configuration commands
    /// Get channel configuration (value: channel_id u8)
    GetChannelConfig,
    /// Set channel configuration (value: postcard-serialized ChannelConfig)
    SetChannelConfig,
    /// Get all channel configurations (no payload)
    GetAllChannelConfigs,
    /// Clear all channel configurations (no payload)
    ClearChannelConfigs,
    /// Get jitter buffer stats for a channel (value: channel_id u8)
    GetJitterStats,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, IntoPrimitive, TryFromPrimitive)]
#[repr(u16)]
pub enum NetToMgmt {
    Pong = 0x50,
    CircularPing,
    WifiSsids,
    RelayUrl,
    Ack,
    Error,
    /// Loopback mode status (1 byte: NetLoopback - 0=Off, 1=Raw, 2=Moq)
    Loopback,
    /// MoQ connected to relay
    MoqConnected,
    /// MoQ disconnected from relay
    MoqDisconnected,
    /// Chat message sent confirmation
    ChatMessageSent,
    /// Chat message received (value: UTF-8 message)
    ChatMessageReceived,
    // Channel configuration responses
    /// Channel configuration (value: postcard-serialized ChannelConfig)
    ChannelConfig,
    /// All channel configurations (value: postcard-serialized Vec<ChannelConfig>)
    AllChannelConfigs,
    /// Jitter buffer statistics (19 bytes: received u32, output u32, underruns u32, overruns u32, level u16, state u8)
    JitterStats,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, IntoPrimitive, TryFromPrimitive)]
#[repr(u16)]
pub enum UiToNet {
    CircularPing = 0x60,
    /// Legacy: Audio frame from button A press (no channel_id prefix)
    AudioFrameA,
    /// Legacy: Audio frame from button B press (no channel_id prefix)
    AudioFrameB,
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

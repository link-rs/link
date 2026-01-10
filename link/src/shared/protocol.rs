//! Message type enums defining the communication protocol between chips.

use num_enum::{IntoPrimitive, TryFromPrimitive};

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
    /// Start WebSocket echo test: send packets, measure inter-arrival times
    WsEchoTest,
    /// Hold UI chip in reset
    HoldUiReset,
    /// Start WebSocket speed test: blast frames as fast as possible
    WsSpeedTest,
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
    /// WebSocket echo test results
    WsEchoTestResult,
    /// WebSocket speed test results
    WsSpeedTestResult,
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
    /// Set loopback mode (1 byte: 0=off, 1=on)
    SetLoopback,
    /// Get loopback mode
    GetLoopback,
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
    /// Loopback mode status (1 byte: 0=off, 1=on)
    Loopback,
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
    WsSend,
    /// Start WebSocket echo test
    WsEchoTest,
    /// Set loopback mode (1 byte: 0=off, 1=on)
    SetLoopback,
    /// Get loopback mode
    GetLoopback,
    /// Start WebSocket speed test
    WsSpeedTest,
    // MoQ commands
    /// Get MoQ relay URL
    GetMoqRelayUrl,
    /// Set MoQ relay URL (value: UTF-8 URL string)
    SetMoqRelayUrl,
    /// Get MoQ enabled state
    GetMoqEnabled,
    /// Set MoQ enabled state (1 byte: 0=disabled, 1=enabled)
    SetMoqEnabled,
    /// Get MoQ example type
    GetMoqExampleType,
    /// Set MoQ example type (1 byte: 0=Clock, 1=Chat, 2=Benchmark)
    SetMoqExampleType,
    /// Get MoQ configuration summary
    GetMoqConfig,
    /// Get benchmark target FPS (4 bytes LE)
    GetBenchmarkFps,
    /// Set benchmark target FPS (4 bytes LE, 0=burst mode)
    SetBenchmarkFps,
    /// Get benchmark payload size (4 bytes LE)
    GetBenchmarkPayloadSize,
    /// Set benchmark payload size (4 bytes LE)
    SetBenchmarkPayloadSize,
    /// Start MoQ example
    StartMoqExample,
    /// Stop MoQ example
    StopMoqExample,
    /// Send chat message (value: UTF-8 message)
    SendChatMessage,
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
    WsReceived,
    WsConnected,
    WsDisconnected,
    /// WebSocket echo test results
    WsEchoTestResult,
    /// WebSocket speed test results
    WsSpeedTestResult,
    /// Loopback mode status (1 byte: 0=off, 1=on)
    Loopback,
    // MoQ responses
    /// MoQ relay URL (value: UTF-8 URL string)
    MoqRelayUrl,
    /// MoQ enabled state (1 byte: 0=disabled, 1=enabled)
    MoqEnabled,
    /// MoQ example type (1 byte: 0=Clock, 1=Chat, 2=Benchmark)
    MoqExampleType,
    /// MoQ configuration summary (value: text)
    MoqConfig,
    /// Benchmark target FPS (4 bytes LE)
    BenchmarkFps,
    /// Benchmark payload size (4 bytes LE)
    BenchmarkPayloadSize,
    /// MoQ example started (1 byte: example type)
    MoqExampleStarted,
    /// MoQ example stopped
    MoqExampleStopped,
    /// Chat message sent confirmation
    ChatMessageSent,
    /// Chat message received (value: UTF-8 message)
    ChatMessageReceived,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, IntoPrimitive, TryFromPrimitive)]
#[repr(u16)]
pub enum UiToNet {
    CircularPing = 0x60,
    /// Audio frame from button A press
    AudioFrameA,
    /// Audio frame from button B press
    AudioFrameB,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, IntoPrimitive, TryFromPrimitive)]
#[repr(u16)]
pub enum NetToUi {
    CircularPing = 0x70,
    /// Audio frame to play out
    AudioFrame,
}

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

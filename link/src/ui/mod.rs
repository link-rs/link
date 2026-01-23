//! UI (User Interface) chip - handles buttons and user interaction.

mod audio;
mod eeprom;
mod log;
#[cfg(feature = "sframe")]
mod sframe;
#[cfg(feature = "sframe")]
pub use sframe::KeyMaterial;

pub use audio::{
    AudioError, AudioSystem, Frame, StereoFrame, ENCODED_FRAME_SIZE, FRAME_SIZE, STEREO_FRAME_SIZE,
};
pub use eeprom::Eeprom;
pub use log::{LogMessage, LogSender, MAX_LOG_SIZE};

use crate::info;
use crate::shared::{
    read_tlv_loop, Channel, Color, CriticalSectionRawMutex, Led, LoopbackMode, MgmtToUi, NetToUi,
    Sender, Tlv, UiToMgmt, UiToNet, WriteTlv,
};
use crate::tlv_log;
use embedded_hal::delay::DelayNs;
use embedded_hal::digital::StatefulOutputPin;
use embedded_hal::i2c::I2c;
use embedded_hal_async::digital::Wait;
use embedded_io_async::{Read, Write};
use portable_atomic::{AtomicU8, Ordering};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Button {
    A,
    B,
}

enum Event {
    Mgmt(Tlv<MgmtToUi>),
    Net(Tlv<NetToUi>),
    ButtonDown(Button),
    ButtonUp(Button),
    AudioFrame(Frame),
}

#[allow(unreachable_code)]
pub async fn run<W, R, LR, LG, LB, BA, BB, BM, I, D, AS>(
    mut to_mgmt: W,
    from_mgmt: R,
    mut to_net: W,
    from_net: R,
    led: (LR, LG, LB),
    button_a: BA,
    button_b: BB,
    button_mic: BM,
    mut i2c: I,
    mut delay: D,
    mut audio_system: AS,
) -> !
where
    W: Write,
    R: Read,
    LR: StatefulOutputPin,
    LG: StatefulOutputPin,
    LB: StatefulOutputPin,
    BA: Wait,
    BB: Wait,
    BM: Wait,
    I: I2c,
    D: DelayNs,
    AS: AudioSystem,
{
    info!("ui: starting");

    // Initialize LED
    let mut led = Led::new(led.0, led.1, led.2);
    led.set(Color::Green);

    const MAX_QUEUE_DEPTH: usize = 4;
    let channel: Channel<CriticalSectionRawMutex, Event, MAX_QUEUE_DEPTH> = Channel::new();

    let mgmt_read_task = read_tlv_loop(from_mgmt, channel.sender(), Event::Mgmt);
    let net_read_task = read_tlv_loop(from_net, channel.sender(), Event::Net);
    let button_a_task = button_monitor(button_a, Button::A, false, channel.sender());
    let button_b_task = button_monitor(button_b, Button::B, false, channel.sender());
    let button_mic_task = button_monitor(button_mic, Button::A, true, channel.sender());

    // Queue for playback frames (from NET)
    const PLAYBACK_QUEUE_SIZE: usize = 4;
    let playback_channel: Channel<CriticalSectionRawMutex, Frame, PLAYBACK_QUEUE_SIZE> =
        Channel::new();

    // Queue for log messages (to MGMT)
    const LOG_QUEUE_SIZE: usize = 8;
    let log_channel: Channel<CriticalSectionRawMutex, LogMessage, LOG_QUEUE_SIZE> = Channel::new();
    let log_sender = log_channel.sender();

    // Shared loopback mode (atomic for cross-task access)
    let loopback_mode = AtomicU8::new(LoopbackMode::Off as u8);

    // Flag to indicate SFrame key needs re-derivation (set when key is changed via SetSFrameKey)
    #[cfg(feature = "sframe")]
    let sframe_key_dirty = portable_atomic::AtomicBool::new(true);

    let handle_task = async {
        info!("ui: ready to handle events");

        // Track which button is currently held (if any)
        let mut active_button: Option<Button> = None;

        // SFrame state for encryption loopback
        #[cfg(feature = "sframe")]
        let mut sframe_state: Option<(sframe::KeyMaterial, u64)> = None; // (key_material, counter)

        loop {
            match channel.receive().await {
                Event::Mgmt(tlv) => {
                    handle_mgmt(
                        tlv,
                        &mut to_mgmt,
                        &mut to_net,
                        &mut i2c,
                        &mut delay,
                        &loopback_mode,
                        #[cfg(feature = "sframe")]
                        &sframe_key_dirty,
                    )
                    .await
                }
                Event::Net(tlv) => {
                    if let Some(frame) = handle_net(tlv, &mut to_mgmt).await {
                        playback_channel.send(frame).await;
                    }
                }
                Event::ButtonDown(button) => {
                    info!("ui: button {:?} down", button);
                    tlv_log!(log_sender, "button {:?} down", button);
                    if active_button.is_none() {
                        active_button = Some(button);
                    }
                }
                Event::ButtonUp(button) => {
                    info!("ui: button {:?} up", button);
                    tlv_log!(log_sender, "button {:?} up", button);
                    if active_button == Some(button) {
                        active_button = None;
                    }
                }
                Event::AudioFrame(frame) => {
                    // Audio frame read from microphone (already A-law encoded)
                    // Only process if a button is held
                    if let Some(button) = active_button {
                        let mode = LoopbackMode::try_from(loopback_mode.load(Ordering::Relaxed))
                            .unwrap_or(LoopbackMode::Off);

                        match mode {
                            LoopbackMode::Off => {
                                // Normal: send to NET
                                let tlv_type = match button {
                                    Button::A => UiToNet::AudioFrameA,
                                    Button::B => UiToNet::AudioFrameB,
                                };
                                to_net.must_write_tlv(tlv_type, frame.as_bytes()).await;
                            }
                            LoopbackMode::Raw => {
                                // Raw loopback is handled in audio_task, nothing to do here
                            }
                            LoopbackMode::Alaw => {
                                // Alaw loopback: send directly to speaker (encode then decode)
                                playback_channel.send(frame).await;
                            }
                            #[cfg(feature = "sframe")]
                            LoopbackMode::Sframe => {
                                // SFrame loopback: encrypt then decrypt
                                // Re-derive key material if dirty or not yet initialized
                                if sframe_state.is_none()
                                    || sframe_key_dirty.load(Ordering::Relaxed)
                                {
                                    let mut eeprom = Eeprom::new(&mut i2c, &mut delay);
                                    if let Ok(base_key) = eeprom.get_sframe_key() {
                                        let key_material = sframe::KeyMaterial::derive(&base_key, 0);
                                        sframe_state = Some((key_material, 0));
                                        sframe_key_dirty.store(false, Ordering::Relaxed);
                                    }
                                }

                                if let Some((ref key_material, ref mut counter)) = sframe_state {
                                    // Copy frame to buffer for in-place encryption
                                    let mut buf: heapless::Vec<u8, 256> = heapless::Vec::new();
                                    if buf.extend_from_slice(frame.as_bytes()).is_ok() {
                                        // Protect (encrypt)
                                        if key_material.protect(*counter, &[], &mut buf).is_ok() {
                                            *counter += 1;
                                            // Unprotect (decrypt)
                                            if key_material.unprotect(&[], &mut buf).is_ok() {
                                                // Convert back to Frame and play
                                                if let Some(decrypted_frame) = Frame::from_bytes(&buf) {
                                                    playback_channel.send(decrypted_frame).await;
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            #[cfg(not(feature = "sframe"))]
                            LoopbackMode::Sframe => {
                                // SFrame not enabled, fall back to Alaw loopback
                                playback_channel.send(frame).await;
                            }
                        }
                    }
                }
            }

            // Drain any pending log messages
            while let Ok(msg) = log_channel.try_receive() {
                to_mgmt.must_write_tlv(UiToMgmt::Log, msg.as_bytes()).await;
            }
        }
    };

    // Audio I/O task: reads from microphone, writes queued playback frames
    let audio_task = async {
        audio_system.start().await;

        // Play a 400Hz stereo square wave for startup
        // Generate stereo tone (interleaved L/R samples)
        let tone_stereo = StereoFrame(core::array::from_fn(|i| {
            const AMPLITUDE: u16 = 0x1ff;
            const FREQ: u16 = 40; // Period in samples (doubled for stereo)
            ((((i / 2) as u16) / (FREQ / 2)) % 2) * AMPLITUDE
        }));
        let mut zero_stereo = StereoFrame::default();
        for _i in 0..25 {
            // 25 frames at 80ms each = 2 seconds
            let _ = audio_system
                .read_write(&tone_stereo, &mut zero_stereo)
                .await;
        }

        // Buffer for raw loopback (previous frame's rx becomes next frame's tx)
        let mut raw_loopback_frame = StereoFrame::default();

        loop {
            let mode = LoopbackMode::try_from(loopback_mode.load(Ordering::Relaxed))
                .unwrap_or(LoopbackMode::Off);

            // Raw loopback: bypass encode/decode, just echo stereo directly
            if mode == LoopbackMode::Raw {
                let mut rx_stereo = StereoFrame::default();
                if audio_system
                    .read_write(&raw_loopback_frame, &mut rx_stereo)
                    .await
                    .is_ok()
                {
                    raw_loopback_frame = rx_stereo;
                }
                continue;
            }

            // Wait for a frame with timeout matching frame duration (20ms)
            #[cfg(feature = "audio-buffer")]
            let tx_stereo = {
                use embassy_time::{with_timeout, Duration};
                match with_timeout(Duration::from_millis(20), playback_channel.receive()).await {
                    Ok(frame) => frame.decode_to_stereo(),
                    Err(_timeout) => StereoFrame::default(),
                }
            };

            // Fallback for tests (no embassy-time available)
            #[cfg(not(feature = "audio-buffer"))]
            let tx_stereo = if let Ok(frame) = playback_channel.try_receive() {
                frame.decode_to_stereo()
            } else {
                StereoFrame::default()
            };

            let mut rx_stereo = StereoFrame::default();

            // Do the I2S read/write cycle
            if audio_system
                .read_write(&tx_stereo, &mut rx_stereo)
                .await
                .is_ok()
            {
                // Encode stereo to A-law mono for transmission
                let encoded_frame = rx_stereo.encode();
                // Try to send the recorded frame - drop if channel is full
                // This prevents blocking the audio task if event handler is slow
                let _ = channel.try_send(Event::AudioFrame(encoded_frame));
            }
        }
    };

    futures::join!(
        mgmt_read_task,
        net_read_task,
        button_a_task,
        button_b_task,
        button_mic_task,
        handle_task,
        audio_task
    );
    unreachable!()
}

async fn button_monitor<'a, B: Wait, const N: usize>(
    mut button: B,
    which: Button,
    active_low: bool,
    sender: Sender<'a, CriticalSectionRawMutex, Event, N>,
) -> ! {
    loop {
        if active_low {
            // Active low: falling edge = press, rising edge = release
            let _ = button.wait_for_falling_edge().await;
            sender.send(Event::ButtonDown(which)).await;
            let _ = button.wait_for_rising_edge().await;
            sender.send(Event::ButtonUp(which)).await;
        } else {
            // Active high: rising edge = press, falling edge = release
            let _ = button.wait_for_rising_edge().await;
            sender.send(Event::ButtonDown(which)).await;
            let _ = button.wait_for_falling_edge().await;
            sender.send(Event::ButtonUp(which)).await;
        }
    }
}

async fn handle_mgmt<M, N, I, D>(
    tlv: Tlv<MgmtToUi>,
    to_mgmt: &mut M,
    to_net: &mut N,
    i2c: &mut I,
    delay: &mut D,
    loopback_mode: &AtomicU8,
    #[cfg(feature = "sframe")] sframe_key_dirty: &portable_atomic::AtomicBool,
) where
    M: WriteTlv<UiToMgmt>,
    N: WriteTlv<UiToNet>,
    I: I2c,
    D: DelayNs,
{
    match tlv.tlv_type {
        MgmtToUi::Ping => {
            info!("ui: mgmt ping, sending pong");
            to_mgmt.must_write_tlv(UiToMgmt::Pong, &tlv.value).await;
        }
        MgmtToUi::CircularPing => {
            info!("ui: mgmt circular ping -> net");
            to_net
                .must_write_tlv(UiToNet::CircularPing, &tlv.value)
                .await;
        }
        MgmtToUi::GetVersion => {
            info!("ui: get version");
            let mut eeprom = Eeprom::new(i2c, delay);
            let Ok(version) = eeprom.get_version() else {
                info!("ui: failed to read version from EEPROM");
                return;
            };
            let value = version.to_be_bytes();
            to_mgmt.must_write_tlv(UiToMgmt::Version, &value).await;
        }
        MgmtToUi::SetVersion => {
            info!("ui: set version");
            if tlv.value.len() != 4 {
                info!("ui: invalid version length: {}", tlv.value.len());
                to_mgmt.must_write_tlv(UiToMgmt::Error, b"length").await;
                return;
            }
            let version =
                u32::from_be_bytes([tlv.value[0], tlv.value[1], tlv.value[2], tlv.value[3]]);
            let mut eeprom = Eeprom::new(i2c, delay);
            if eeprom.set_version(version).is_err() {
                info!("ui: failed to write version to EEPROM");
                to_mgmt.must_write_tlv(UiToMgmt::Error, b"eeprom").await;
                return;
            }
            to_mgmt.must_write_tlv(UiToMgmt::Ack, &[]).await;
        }
        MgmtToUi::GetSFrameKey => {
            info!("ui: get sframe key");
            let mut eeprom = Eeprom::new(i2c, delay);
            let Ok(key) = eeprom.get_sframe_key() else {
                info!("ui: failed to read sframe key from EEPROM");
                return;
            };
            to_mgmt.must_write_tlv(UiToMgmt::SFrameKey, &key).await;
        }
        MgmtToUi::SetSFrameKey => {
            info!("ui: set sframe key");
            if tlv.value.len() != 16 {
                info!("ui: invalid sframe key length: {}", tlv.value.len());
                to_mgmt.must_write_tlv(UiToMgmt::Error, b"length").await;
                return;
            }
            let mut key = [0u8; 16];
            key.copy_from_slice(&tlv.value[..16]);
            let mut eeprom = Eeprom::new(i2c, delay);
            if eeprom.set_sframe_key(&key).is_err() {
                info!("ui: failed to write sframe key to EEPROM");
                to_mgmt.must_write_tlv(UiToMgmt::Error, b"eeprom").await;
                return;
            }
            // Mark SFrame key as dirty so it gets re-derived on next use
            #[cfg(feature = "sframe")]
            sframe_key_dirty.store(true, Ordering::Relaxed);
            to_mgmt.must_write_tlv(UiToMgmt::Ack, &[]).await;
        }
        MgmtToUi::SetLoopback => {
            let mode_byte = tlv.value.first().copied().unwrap_or(0);
            let mode = LoopbackMode::try_from(mode_byte).unwrap_or(LoopbackMode::Off);
            info!("ui: set loopback = {:?}", mode);
            loopback_mode.store(mode as u8, Ordering::Relaxed);
            to_mgmt.must_write_tlv(UiToMgmt::Ack, &[]).await;
        }
        MgmtToUi::GetLoopback => {
            let mode = loopback_mode.load(Ordering::Relaxed);
            info!("ui: get loopback = {}", mode);
            to_mgmt.must_write_tlv(UiToMgmt::Loopback, &[mode]).await;
        }
    }
}

async fn handle_net<M>(tlv: Tlv<NetToUi>, to_mgmt: &mut M) -> Option<Frame>
where
    M: WriteTlv<UiToMgmt>,
{
    match tlv.tlv_type {
        NetToUi::CircularPing => {
            info!("ui: net circular ping -> mgmt");
            to_mgmt
                .must_write_tlv(UiToMgmt::CircularPing, &tlv.value)
                .await;
            None
        }
        NetToUi::AudioFrame => {
            if let Some(frame) = Frame::from_bytes(&tlv.value) {
                Some(frame)
            } else {
                info!("ui: invalid audio frame size: {}", tlv.value.len());
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::mocks::{mock_i2c_with_eeprom, MockDelay};
    use crate::shared::{Tlv, Value};

    /// Mock writer that captures TLVs
    struct MockTlvWriter {
        written: std::vec::Vec<(UiToMgmt, std::vec::Vec<u8>)>,
    }

    impl MockTlvWriter {
        fn new() -> Self {
            Self {
                written: std::vec::Vec::new(),
            }
        }
    }

    impl WriteTlv<UiToMgmt> for MockTlvWriter {
        type Error = ();

        async fn write_tlv(&mut self, tlv_type: UiToMgmt, value: &[u8]) -> Result<(), ()> {
            self.written.push((tlv_type, value.to_vec()));
            Ok(())
        }
    }

    /// Dummy writer for to_net (not used in EEPROM tests)
    struct DummyNetWriter;

    impl WriteTlv<UiToNet> for DummyNetWriter {
        type Error = ();

        async fn write_tlv(&mut self, _: UiToNet, _: &[u8]) -> Result<(), ()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn tlv_get_version() {
        let mut to_mgmt = MockTlvWriter::new();
        let mut to_net = DummyNetWriter;
        let mut i2c = mock_i2c_with_eeprom();
        let mut delay = MockDelay;

        // Set a known version
        {
            let mut eeprom = Eeprom::new(&mut i2c, &mut delay);
            eeprom.set_version(0xaabbccdd).unwrap();
        }

        // Create GetVersion TLV
        let tlv = Tlv {
            tlv_type: MgmtToUi::GetVersion,
            value: Value::new(),
        };

        let loopback_mode = AtomicU8::new(LoopbackMode::Off as u8);
        handle_mgmt(
            tlv,
            &mut to_mgmt,
            &mut to_net,
            &mut i2c,
            &mut delay,
            &loopback_mode,
        )
        .await;

        assert_eq!(to_mgmt.written.len(), 1);
        assert_eq!(to_mgmt.written[0].0, UiToMgmt::Version);
        assert_eq!(to_mgmt.written[0].1, &[0xaa, 0xbb, 0xcc, 0xdd]);
    }

    #[tokio::test]
    async fn tlv_set_version() {
        let mut to_mgmt = MockTlvWriter::new();
        let mut to_net = DummyNetWriter;
        let mut i2c = mock_i2c_with_eeprom();
        let mut delay = MockDelay;

        // Create SetVersion TLV with version 0x11223344
        let mut value: Value = Value::new();
        value.extend_from_slice(&[0x11, 0x22, 0x33, 0x44]).unwrap();
        let tlv = Tlv {
            tlv_type: MgmtToUi::SetVersion,
            value,
        };

        let loopback_mode = AtomicU8::new(LoopbackMode::Off as u8);
        handle_mgmt(
            tlv,
            &mut to_mgmt,
            &mut to_net,
            &mut i2c,
            &mut delay,
            &loopback_mode,
        )
        .await;

        // Verify version was set and Ack was sent
        assert_eq!(to_mgmt.written.len(), 1);
        assert_eq!(to_mgmt.written[0].0, UiToMgmt::Ack);
        let mut eeprom = Eeprom::new(&mut i2c, &mut delay);
        assert_eq!(eeprom.get_version().unwrap(), 0x11223344);
    }

    #[tokio::test]
    async fn tlv_get_sframe_key() {
        let mut to_mgmt = MockTlvWriter::new();
        let mut to_net = DummyNetWriter;
        let mut i2c = mock_i2c_with_eeprom();
        let mut delay = MockDelay;

        // Set a known key
        let key = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
        {
            let mut eeprom = Eeprom::new(&mut i2c, &mut delay);
            eeprom.set_sframe_key(&key).unwrap();
        }

        // Create GetSFrameKey TLV
        let tlv = Tlv {
            tlv_type: MgmtToUi::GetSFrameKey,
            value: Value::new(),
        };

        let loopback_mode = AtomicU8::new(LoopbackMode::Off as u8);
        handle_mgmt(
            tlv,
            &mut to_mgmt,
            &mut to_net,
            &mut i2c,
            &mut delay,
            &loopback_mode,
        )
        .await;

        assert_eq!(to_mgmt.written.len(), 1);
        assert_eq!(to_mgmt.written[0].0, UiToMgmt::SFrameKey);
        assert_eq!(to_mgmt.written[0].1, &key);
    }

    #[tokio::test]
    async fn tlv_set_sframe_key() {
        let mut to_mgmt = MockTlvWriter::new();
        let mut to_net = DummyNetWriter;
        let mut i2c = mock_i2c_with_eeprom();
        let mut delay = MockDelay;

        // Create SetSFrameKey TLV
        let key = [
            0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88,
            0x99, 0x00,
        ];
        let mut value: Value = Value::new();
        value.extend_from_slice(&key).unwrap();
        let tlv = Tlv {
            tlv_type: MgmtToUi::SetSFrameKey,
            value,
        };

        let loopback_mode = AtomicU8::new(LoopbackMode::Off as u8);
        handle_mgmt(
            tlv,
            &mut to_mgmt,
            &mut to_net,
            &mut i2c,
            &mut delay,
            &loopback_mode,
        )
        .await;

        // Verify key was set and Ack was sent
        assert_eq!(to_mgmt.written.len(), 1);
        assert_eq!(to_mgmt.written[0].0, UiToMgmt::Ack);
        let mut eeprom = Eeprom::new(&mut i2c, &mut delay);
        assert_eq!(eeprom.get_sframe_key().unwrap(), key);
    }

    #[tokio::test]
    async fn tlv_set_version_invalid_length() {
        let mut to_mgmt = MockTlvWriter::new();
        let mut to_net = DummyNetWriter;
        let mut i2c = mock_i2c_with_eeprom();
        let mut delay = MockDelay;

        // Create SetVersion TLV with only 2 bytes (invalid)
        let mut value: Value = Value::new();
        value.extend_from_slice(&[0x11, 0x22]).unwrap();
        let tlv = Tlv {
            tlv_type: MgmtToUi::SetVersion,
            value,
        };

        let loopback_mode = AtomicU8::new(LoopbackMode::Off as u8);
        handle_mgmt(
            tlv,
            &mut to_mgmt,
            &mut to_net,
            &mut i2c,
            &mut delay,
            &loopback_mode,
        )
        .await;

        // Version should remain default (0xffffffff) and Error should be sent
        assert_eq!(to_mgmt.written.len(), 1);
        assert_eq!(to_mgmt.written[0].0, UiToMgmt::Error);
        let mut eeprom = Eeprom::new(&mut i2c, &mut delay);
        assert_eq!(eeprom.get_version().unwrap(), 0xffffffff);
    }

    #[tokio::test]
    async fn tlv_set_sframe_key_invalid_length() {
        let mut to_mgmt = MockTlvWriter::new();
        let mut to_net = DummyNetWriter;
        let mut i2c = mock_i2c_with_eeprom();
        let mut delay = MockDelay;

        // Create SetSFrameKey TLV with only 8 bytes (invalid)
        let mut value: Value = Value::new();
        value.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]).unwrap();
        let tlv = Tlv {
            tlv_type: MgmtToUi::SetSFrameKey,
            value,
        };

        let loopback_mode = AtomicU8::new(LoopbackMode::Off as u8);
        handle_mgmt(
            tlv,
            &mut to_mgmt,
            &mut to_net,
            &mut i2c,
            &mut delay,
            &loopback_mode,
        )
        .await;

        // Key should remain default (0xff) and Error should be sent
        assert_eq!(to_mgmt.written.len(), 1);
        assert_eq!(to_mgmt.written[0].0, UiToMgmt::Error);
        let mut eeprom = Eeprom::new(&mut i2c, &mut delay);
        assert_eq!(eeprom.get_sframe_key().unwrap(), [0xff; 16]);
    }
}

#[cfg(test)]
mod audio_streaming_tests {
    use super::*;
    use crate::shared::mocks::{
        mock_i2c_with_eeprom, mock_led_pins, ControllableButton, MockAudioStream, MockButton,
        MockDelay,
    };
    use crate::shared::ReadTlv;
    use embedded_io_adapters::futures_03::FromFutures;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    type Reader = FromFutures<async_ringbuffer::Reader>;
    type Writer = FromFutures<async_ringbuffer::Writer>;

    fn channel() -> (Writer, Reader) {
        const BUFFER_CAPACITY: usize = 4096;
        let (w, r) = async_ringbuffer::ring_buffer(BUFFER_CAPACITY);
        (FromFutures::new(w), FromFutures::new(r))
    }

    /// Collector for TLVs received from the UI chip.
    struct TlvCollector {
        frames_a: Arc<Mutex<Vec<Vec<u8>>>>,
        frames_b: Arc<Mutex<Vec<Vec<u8>>>>,
    }

    impl TlvCollector {
        fn new() -> Self {
            Self {
                frames_a: Arc::new(Mutex::new(Vec::new())),
                frames_b: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn frames_a(&self) -> Arc<Mutex<Vec<Vec<u8>>>> {
            self.frames_a.clone()
        }

        fn frames_b(&self) -> Arc<Mutex<Vec<Vec<u8>>>> {
            self.frames_b.clone()
        }

        async fn collect_from(&self, mut reader: Reader) {
            use crate::shared::Tlv;
            loop {
                let result: Result<Option<Tlv<UiToNet>>, _> = reader.read_tlv().await;
                if let Ok(Some(tlv)) = result {
                    match tlv.tlv_type {
                        UiToNet::AudioFrameA => {
                            self.frames_a.lock().unwrap().push(tlv.value.to_vec());
                        }
                        UiToNet::AudioFrameB => {
                            self.frames_b.lock().unwrap().push(tlv.value.to_vec());
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn button_a_sends_audio_frame_a() {
        let (ui_to_mgmt, _mgmt_from_ui) = channel();
        let (_mgmt_to_ui, ui_from_mgmt) = channel();
        let (ui_to_net, net_from_ui) = channel();
        let (_net_to_ui, ui_from_net) = channel();

        let (button_a, button_a_ctrl) = ControllableButton::new();

        let collector = TlvCollector::new();
        let frames_a = collector.frames_a();

        tokio::select! {
            _ = run(
                ui_to_mgmt,
                ui_from_mgmt,
                ui_to_net,
                ui_from_net,
                mock_led_pins(),
                button_a,
                MockButton,
                MockButton,
                mock_i2c_with_eeprom(),
                MockDelay,
                MockAudioStream::new(),
            ) => unreachable!(),
            _ = collector.collect_from(net_from_ui) => unreachable!(),
            _ = async {
                // Wait for startup tone to complete (50 frames × 5ms = 250ms)
                tokio::time::sleep(Duration::from_millis(300)).await;

                // Press button A
                button_a_ctrl.press().await;

                // Wait for at least 2 audio frames (5ms each + margin)
                tokio::time::sleep(Duration::from_millis(100)).await;

                // Release button A
                button_a_ctrl.release().await;

                // Wait a bit to ensure no more frames after release
                tokio::time::sleep(Duration::from_millis(30)).await;
            } => {}
        }

        // Should have received at least 2 AudioFrameA TLVs while button was held
        let frames = frames_a.lock().unwrap();
        assert!(
            frames.len() >= 2,
            "Expected at least 2 frames, got {}",
            frames.len()
        );

        // Each frame should be 160 bytes (20ms at 8kHz A-law)
        for frame in frames.iter() {
            assert_eq!(frame.len(), 160, "Frame should be 160 bytes");
        }
    }

    #[tokio::test]
    async fn mic_button_sends_audio_frame_a() {
        let (ui_to_mgmt, _mgmt_from_ui) = channel();
        let (_mgmt_to_ui, ui_from_mgmt) = channel();
        let (ui_to_net, net_from_ui) = channel();
        let (_net_to_ui, ui_from_net) = channel();

        let (button_mic, button_mic_ctrl) = ControllableButton::new();

        let collector = TlvCollector::new();
        let frames_a = collector.frames_a();
        let frames_b = collector.frames_b();

        tokio::select! {
            _ = run(
                ui_to_mgmt,
                ui_from_mgmt,
                ui_to_net,
                ui_from_net,
                mock_led_pins(),
                MockButton,
                MockButton,
                button_mic,
                mock_i2c_with_eeprom(),
                MockDelay,
                MockAudioStream::new(),
            ) => unreachable!(),
            _ = collector.collect_from(net_from_ui) => unreachable!(),
            _ = async {
                // Wait for startup tone to complete (50 frames × 5ms = 250ms)
                tokio::time::sleep(Duration::from_millis(300)).await;

                // Press mic button (active-low)
                button_mic_ctrl.press_active_low().await;

                // Wait for at least 2 audio frames
                tokio::time::sleep(Duration::from_millis(100)).await;

                // Release mic button (active-low)
                button_mic_ctrl.release_active_low().await;

                // Wait a bit
                tokio::time::sleep(Duration::from_millis(30)).await;
            } => {}
        }

        // Mic button should send AudioFrameA (same as button A)
        let frames = frames_a.lock().unwrap();
        assert!(
            frames.len() >= 2,
            "Expected at least 2 AudioFrameA frames from mic button, got {}",
            frames.len()
        );

        // Should have no AudioFrameB
        assert_eq!(
            frames_b.lock().unwrap().len(),
            0,
            "Mic button should not send AudioFrameB"
        );
    }

    #[tokio::test]
    async fn button_b_sends_audio_frame_b() {
        let (ui_to_mgmt, _mgmt_from_ui) = channel();
        let (_mgmt_to_ui, ui_from_mgmt) = channel();
        let (ui_to_net, net_from_ui) = channel();
        let (_net_to_ui, ui_from_net) = channel();

        let (button_b, button_b_ctrl) = ControllableButton::new();

        let collector = TlvCollector::new();
        let frames_b = collector.frames_b();

        tokio::select! {
            _ = run(
                ui_to_mgmt,
                ui_from_mgmt,
                ui_to_net,
                ui_from_net,
                mock_led_pins(),
                MockButton,
                button_b,
                MockButton,
                mock_i2c_with_eeprom(),
                MockDelay,
                MockAudioStream::new(),
            ) => unreachable!(),
            _ = collector.collect_from(net_from_ui) => unreachable!(),
            _ = async {
                // Wait for startup tone to complete (50 frames × 5ms = 250ms)
                tokio::time::sleep(Duration::from_millis(300)).await;

                // Press button B
                button_b_ctrl.press().await;

                // Wait for at least 2 audio frames
                tokio::time::sleep(Duration::from_millis(100)).await;

                // Release button B
                button_b_ctrl.release().await;

                // Wait a bit
                tokio::time::sleep(Duration::from_millis(30)).await;
            } => {}
        }

        // Should have received AudioFrameB TLVs
        let frames = frames_b.lock().unwrap();
        assert!(
            frames.len() >= 2,
            "Expected at least 2 frames, got {}",
            frames.len()
        );

        // Each frame should be 160 bytes (20ms at 8kHz A-law)
        for frame in frames.iter() {
            assert_eq!(frame.len(), 160, "Frame should be 160 bytes");
        }
    }

    #[tokio::test]
    async fn no_audio_when_button_not_pressed() {
        let (ui_to_mgmt, _mgmt_from_ui) = channel();
        let (_mgmt_to_ui, ui_from_mgmt) = channel();
        let (ui_to_net, net_from_ui) = channel();
        let (_net_to_ui, ui_from_net) = channel();

        let collector = TlvCollector::new();
        let frames_a = collector.frames_a();
        let frames_b = collector.frames_b();

        tokio::select! {
            _ = run(
                ui_to_mgmt,
                ui_from_mgmt,
                ui_to_net,
                ui_from_net,
                mock_led_pins(),
                MockButton,
                MockButton,
                MockButton,
                mock_i2c_with_eeprom(),
                MockDelay,
                MockAudioStream::new(),
            ) => unreachable!(),
            _ = collector.collect_from(net_from_ui) => unreachable!(),
            _ = async {
                // Wait for startup tone + extra time without pressing any button
                tokio::time::sleep(Duration::from_millis(350)).await;
            } => {}
        }

        // Should have received no audio frames
        assert_eq!(frames_a.lock().unwrap().len(), 0, "Should have no A frames");
        assert_eq!(frames_b.lock().unwrap().len(), 0, "Should have no B frames");
    }

    #[tokio::test]
    async fn audio_stops_after_button_release() {
        let (ui_to_mgmt, _mgmt_from_ui) = channel();
        let (_mgmt_to_ui, ui_from_mgmt) = channel();
        let (ui_to_net, net_from_ui) = channel();
        let (_net_to_ui, ui_from_net) = channel();

        let (button_a, button_a_ctrl) = ControllableButton::new();

        let collector = TlvCollector::new();
        let frames_a = collector.frames_a();

        tokio::select! {
            _ = run(
                ui_to_mgmt,
                ui_from_mgmt,
                ui_to_net,
                ui_from_net,
                mock_led_pins(),
                button_a,
                MockButton,
                MockButton,
                mock_i2c_with_eeprom(),
                MockDelay,
                MockAudioStream::new(),
            ) => unreachable!(),
            _ = collector.collect_from(net_from_ui) => unreachable!(),
            _ = async {
                // Wait for startup tone to complete
                tokio::time::sleep(Duration::from_millis(300)).await;

                // Press button A briefly
                button_a_ctrl.press().await;
                tokio::time::sleep(Duration::from_millis(50)).await;
                button_a_ctrl.release().await;

                // Record frame count after release
                tokio::time::sleep(Duration::from_millis(20)).await;
                let count_after_release = frames_a.lock().unwrap().len();

                // Wait more and verify no new frames
                tokio::time::sleep(Duration::from_millis(100)).await;
                let count_later = frames_a.lock().unwrap().len();

                assert_eq!(
                    count_after_release, count_later,
                    "No new frames should arrive after button release"
                );
            } => {}
        }
    }

    #[tokio::test]
    async fn first_button_controls_when_both_pressed() {
        let (ui_to_mgmt, _mgmt_from_ui) = channel();
        let (_mgmt_to_ui, ui_from_mgmt) = channel();
        let (ui_to_net, net_from_ui) = channel();
        let (_net_to_ui, ui_from_net) = channel();

        let (button_a, button_a_ctrl) = ControllableButton::new();
        let (button_b, button_b_ctrl) = ControllableButton::new();

        let collector = TlvCollector::new();
        let frames_a = collector.frames_a();
        let frames_b = collector.frames_b();

        tokio::select! {
            _ = run(
                ui_to_mgmt,
                ui_from_mgmt,
                ui_to_net,
                ui_from_net,
                mock_led_pins(),
                button_a,
                button_b,
                MockButton,
                mock_i2c_with_eeprom(),
                MockDelay,
                MockAudioStream::new(),
            ) => unreachable!(),
            _ = collector.collect_from(net_from_ui) => unreachable!(),
            _ = async {
                // Wait for startup tone to complete
                tokio::time::sleep(Duration::from_millis(300)).await;

                // Press button A first
                button_a_ctrl.press().await;
                tokio::time::sleep(Duration::from_millis(30)).await;

                // Now press button B while A is still held
                button_b_ctrl.press().await;
                tokio::time::sleep(Duration::from_millis(50)).await;

                // Release button B (should have no effect since A was first)
                button_b_ctrl.release().await;
                tokio::time::sleep(Duration::from_millis(30)).await;

                // Release button A
                button_a_ctrl.release().await;
                tokio::time::sleep(Duration::from_millis(30)).await;
            } => {}
        }

        // Should have received only AudioFrameA TLVs (first button controls)
        let a_frames = frames_a.lock().unwrap();
        let b_frames = frames_b.lock().unwrap();

        assert!(
            a_frames.len() >= 2,
            "Expected AudioFrameA frames, got {}",
            a_frames.len()
        );
        assert_eq!(
            b_frames.len(),
            0,
            "Should have no AudioFrameB frames when A was pressed first"
        );
    }

    #[tokio::test]
    async fn net_audio_frame_plays_out() {
        use crate::shared::mocks::CapturingAudioStream;
        use crate::shared::{WriteTlv, MIN_START_LEVEL};
        use audio_codec_algorithms::{decode_alaw, encode_alaw};

        let (ui_to_mgmt, _mgmt_from_ui) = channel();
        let (_mgmt_to_ui, ui_from_mgmt) = channel();
        let (ui_to_net, _net_from_ui) = channel();
        let (mut net_to_ui, ui_from_net) = channel();

        let (audio_stream, written_frames) = CapturingAudioStream::new();

        tokio::select! {
            _ = run(
                ui_to_mgmt,
                ui_from_mgmt,
                ui_to_net,
                ui_from_net,
                mock_led_pins(),
                MockButton,
                MockButton,
                MockButton,
                mock_i2c_with_eeprom(),
                MockDelay,
                audio_stream,
            ) => unreachable!(),
            _ = async {
                // Wait for startup tone to complete (50 frames × 5ms = 250ms)
                tokio::time::sleep(Duration::from_millis(300)).await;

                // Send enough frames to fill jitter buffer past MIN_START_LEVEL
                for _ in 0..MIN_START_LEVEL {
                    let padding_frame = crate::ui::Frame::default();
                    net_to_ui
                        .write_tlv(crate::shared::NetToUi::AudioFrame, padding_frame.as_bytes())
                        .await
                        .unwrap();
                }

                // Create a test frame with known A-law encoded data
                // Use encode_alaw to get predictable decoded values
                let mut test_frame = crate::ui::Frame::default();
                test_frame.0[0] = encode_alaw(1000);  // Encode known PCM values
                test_frame.0[1] = encode_alaw(2000);
                test_frame.0[159] = encode_alaw(3000); // Last sample in 160-byte frame

                // Send the audio frame from NET to UI
                net_to_ui
                    .write_tlv(crate::shared::NetToUi::AudioFrame, test_frame.as_bytes())
                    .await
                    .unwrap();

                // Wait for the frames to be processed and played
                tokio::time::sleep(Duration::from_millis(200)).await;
            } => {}
        }

        // Verify the frame was played out
        let frames = written_frames.lock().unwrap();
        assert!(
            frames.len() >= 1,
            "Expected at least 1 playback frame, got {}",
            frames.len()
        );

        // After decode, stereo frame has L/R pairs: stereo[0]=L0, stereo[1]=R0, stereo[2]=L1, ...
        // So frame.0[i] (A-law) -> stereo.0[i*2] and stereo.0[i*2+1] (both same decoded value)
        let expected_0 = decode_alaw(encode_alaw(1000)) as u16;
        let expected_1 = decode_alaw(encode_alaw(2000)) as u16;
        let expected_last = decode_alaw(encode_alaw(3000)) as u16;

        // Find our test frame (check stereo positions)
        let found = frames
            .iter()
            .any(|f| f.0[0] == expected_0 && f.0[2] == expected_1 && f.0[159 * 2] == expected_last);
        assert!(found, "Test frame should have been played out");
    }

    #[tokio::test]
    async fn multiple_net_audio_frames_play_in_order() {
        use crate::shared::mocks::CapturingAudioStream;
        use crate::shared::{WriteTlv, MIN_START_LEVEL};
        use audio_codec_algorithms::{decode_alaw, encode_alaw};

        let (ui_to_mgmt, _mgmt_from_ui) = channel();
        let (_mgmt_to_ui, ui_from_mgmt) = channel();
        let (ui_to_net, _net_from_ui) = channel();
        let (mut net_to_ui, ui_from_net) = channel();

        let (audio_stream, written_frames) = CapturingAudioStream::new();

        // Pre-compute expected decoded values for markers 1000, 2000, 3000
        let markers: Vec<(u8, u16)> = (0..3)
            .map(|i| {
                let pcm_value = 1000 + i * 1000;
                let alaw = encode_alaw(pcm_value);
                let decoded = decode_alaw(alaw) as u16;
                (alaw, decoded)
            })
            .collect();

        tokio::select! {
            _ = run(
                ui_to_mgmt,
                ui_from_mgmt,
                ui_to_net,
                ui_from_net,
                mock_led_pins(),
                MockButton,
                MockButton,
                MockButton,
                mock_i2c_with_eeprom(),
                MockDelay,
                audio_stream,
            ) => unreachable!(),
            _ = async {
                // Wait for startup tone to complete (50 frames × 5ms = 250ms)
                tokio::time::sleep(Duration::from_millis(300)).await;

                // Send padding frames to fill jitter buffer past MIN_START_LEVEL
                for _ in 0..MIN_START_LEVEL {
                    let padding_frame = crate::ui::Frame::default();
                    net_to_ui
                        .write_tlv(crate::shared::NetToUi::AudioFrame, padding_frame.as_bytes())
                        .await
                        .unwrap();
                }

                // Send multiple frames with distinct A-law markers
                for (alaw, _) in &markers {
                    let mut frame = crate::ui::Frame::default();
                    frame.0[0] = *alaw;
                    net_to_ui
                        .write_tlv(crate::shared::NetToUi::AudioFrame, frame.as_bytes())
                        .await
                        .unwrap();
                    // Small delay between sends
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }

                // Wait for all frames to be processed
                tokio::time::sleep(Duration::from_millis(200)).await;
            } => {}
        }

        // Verify frames were played
        let frames = written_frames.lock().unwrap();

        // Get expected decoded values
        let expected_values: Vec<u16> = markers.iter().map(|(_, decoded)| *decoded).collect();

        // Find our test frames (filter by matching any of our expected decoded values)
        // Check stereo.0[0] which is the left channel of the first sample
        let test_frames: Vec<_> = frames
            .iter()
            .filter(|f| expected_values.contains(&f.0[0]))
            .collect();

        assert!(
            test_frames.len() >= 3,
            "Expected at least 3 playback frames, got {}",
            test_frames.len()
        );

        // Verify order (frames should arrive in sequence)
        for (i, frame) in test_frames.iter().enumerate() {
            assert_eq!(
                frame.0[0], expected_values[i],
                "Frame {} should have decoded value {}, got {}",
                i, expected_values[i], frame.0[0]
            );
        }
    }
}

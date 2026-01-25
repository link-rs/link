//! UI (User Interface) chip - handles buttons and user interaction.

mod audio;
mod eeprom;
mod log;
mod sframe;

pub use sframe::SFrameState;

pub use audio::{
    AudioError, AudioSystem, Frame, StereoFrame, ENCODED_FRAME_SIZE, FRAME_SIZE, STEREO_FRAME_SIZE,
};
pub use eeprom::Eeprom;
pub use log::{LogMessage, LogSender, MAX_LOG_SIZE};

use crate::info;
use crate::shared::{
    chunk, read_tlv_loop, Channel, ChannelId, Color, CriticalSectionRawMutex, Led, LoopbackMode,
    MgmtToUi, NetToUi, Sender, Tlv, UiToMgmt, UiToNet, WriteTlv,
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

/// Buffer capacity for audio frames through the event channel.
/// Sized for: audio (160) + chunk header (10) + SFrame header (17) + tag (16) = 203
const AUDIO_BUF_SIZE: usize = 256;

enum Event {
    Mgmt(Tlv<MgmtToUi>),
    Net(Tlv<NetToUi>),
    ButtonDown(Button),
    ButtonUp(Button),
    AudioFrame(heapless::Vec<u8, AUDIO_BUF_SIZE>),
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

    let handle_task = async {
        info!("ui: ready to handle events");

        // Track which button is currently held (if any)
        let mut active_button: Option<Button> = None;

        // SFrame state for encryption - initialize from EEPROM key at startup
        let mut sframe_state = {
            let mut eeprom = Eeprom::new(&mut i2c, &mut delay);
            let base_key = eeprom.get_sframe_key().unwrap_or([0u8; 16]);
            sframe::SFrameState::new(&base_key, 0)
        };

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
                        &mut sframe_state,
                    )
                    .await
                }
                Event::Net(tlv) => {
                    // Handle audio frames specially - they need decryption
                    if tlv.tlv_type == NetToUi::AudioFrame {
                        // New hactar format: channel_id (1 byte) + encrypted chunk
                        if tlv.value.len() < 2 {
                            continue;
                        }

                        // Extract channel_id (plaintext first byte)
                        let _channel_id = tlv.value[0];

                        // Decrypt the rest (SFrame header + encrypted chunk + auth tag)
                        let mut buf: heapless::Vec<u8, 256> = heapless::Vec::new();
                        let _ = buf.extend_from_slice(&tlv.value[1..]);
                        if sframe_state.unprotect(&[], &mut buf).is_ok() {
                            // Parse chunk to extract audio data
                            if let Some(parsed) = chunk::parse_chunk(&buf) {
                                let audio_data = &buf[parsed.audio_offset
                                    ..parsed.audio_offset + parsed.audio_length];
                                if let Some(frame) = Frame::from_bytes(audio_data) {
                                    playback_channel.send(frame).await;
                                }
                            }
                        }
                    } else if let Some(frame) = handle_net(tlv, &mut to_mgmt).await {
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
                Event::AudioFrame(mut buf) => 'audio: {
                    // buf contains 160 bytes of A-law encoded audio from audio_task
                    let Some(button) = active_button else {
                        break 'audio;
                    };

                    let mode = LoopbackMode::try_from(loopback_mode.load(Ordering::Relaxed))
                        .unwrap_or(LoopbackMode::Off);

                    // Raw loopback is handled in audio_task
                    if mode == LoopbackMode::Raw {
                        break 'audio;
                    }

                    // Alaw loopback: play directly (no encryption)
                    if mode == LoopbackMode::Alaw {
                        if let Some(frame) = Frame::from_bytes(&buf) {
                            // XXX(RLB) See below comments about timing.
                            to_mgmt.must_write_tlv(UiToMgmt::Log, b"x").await;
                            playback_channel.send(frame).await;
                        } else {
                            // XXX(RLB) See below comments about timing.  Both lines are
                            // empirically necessary.
                            to_mgmt.must_write_tlv(UiToMgmt::Log, b"x").await;
                            embassy_futures::yield_now().await;
                        }
                        break 'audio;
                    }

                    // XXX(RLB) Apparently this is necessary for alaw and sframe loopback to work.
                    // The following do not work:
                    //
                    // * A delay
                    // * A yield
                    // * An empty log TLV
                    //
                    // We need to come back to this and figure out what about these interacting
                    // event loops is causing this weird behavior.
                    to_mgmt.must_write_tlv(UiToMgmt::Log, b"x").await;

                    // Determine channel based on button
                    let channel_id = match button {
                        Button::A => ChannelId::Ptt,
                        Button::B => ChannelId::PttAi,
                    };

                    // Prepend chunk header in-place based on channel type
                    let header_ok = match channel_id {
                        ChannelId::Ptt => chunk::prepend_media_header(&mut buf, false).is_ok(),
                        ChannelId::PttAi => {
                            // TODO: Track request_id for AI requests
                            chunk::prepend_ai_request_header(&mut buf, 0, false).is_ok()
                        }
                        ChannelId::ChatAi => false, // Not used for audio
                    };
                    if !header_ok {
                        break 'audio;
                    }

                    // Encrypt in-place (prepends SFrame header, appends auth tag)
                    if sframe_state.protect(&[], &mut buf).is_err() {
                        break 'audio;
                    }

                    // SFrame loopback: decrypt and play locally
                    if mode == LoopbackMode::Sframe {
                        if sframe_state.unprotect(&[], &mut buf).is_ok() {
                            if let Some(parsed) = chunk::parse_chunk(&buf) {
                                let audio_data = &buf[parsed.audio_offset
                                    ..parsed.audio_offset + parsed.audio_length];
                                if let Some(frame) = Frame::from_bytes(audio_data) {
                                    // XXX(RLB) Same timing comments as above.
                                    embassy_futures::yield_now().await;
                                    playback_channel.send(frame).await;
                                }
                            }
                        }
                        break 'audio;
                    }

                    // Send to NET: channel_id (plaintext) + encrypted chunk
                    let channel_id_byte = [channel_id as u8];
                    to_net
                        .must_write_tlv_parts(UiToNet::AudioFrame, &[&channel_id_byte, &buf])
                        .await;
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

            // Get playback frame if available, otherwise use silence.
            // IMPORTANT: Use non-blocking try_receive() so the I2S read_write
            // timing controls the loop rate. A blocking receive with timeout
            // would add delay ON TOP of the I2S cycle, causing RX overruns.
            let tx_stereo = if let Ok(frame) = playback_channel.try_receive() {
                frame.decode_to_stereo()
            } else {
                StereoFrame::default()
            };

            let mut rx_stereo = StereoFrame::default();

            // Do the I2S read/write cycle
            match audio_system.read_write(&tx_stereo, &mut rx_stereo).await {
                Ok(_) => {
                    // Encode stereo to A-law mono into a buffer that can be extended
                    // in handle_task with chunk headers and encryption
                    let mut buf: heapless::Vec<u8, AUDIO_BUF_SIZE> = heapless::Vec::new();
                    rx_stereo.encode_into(&mut buf);
                    // Try to send the recorded frame - drop if channel is full
                    // This prevents blocking the audio task if event handler is slow
                    let _ = channel.try_send(Event::AudioFrame(buf));
                }
                Err(_) => {
                    // I2S error - frame dropped
                }
            }

            // Yield to let handle_task process events and populate playback_channel
            // before the next iteration's try_receive()
            // XXX(RLB) This seems necessary, but a bit awkward.  Might only be necessary in
            // loopback mode, so we should re-test once we're more in a PTT frame.
            embassy_futures::yield_now().await;
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
    sframe_state: &mut sframe::SFrameState,
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
            // Derive new key material and reset counter
            sframe_state.reset(&key, 0);
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
        let mut sframe_state = sframe::SFrameState::new(&[0u8; 16], 0);
        handle_mgmt(
            tlv,
            &mut to_mgmt,
            &mut to_net,
            &mut i2c,
            &mut delay,
            &loopback_mode,
            &mut sframe_state,
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
        let mut sframe_state = sframe::SFrameState::new(&[0u8; 16], 0);
        handle_mgmt(
            tlv,
            &mut to_mgmt,
            &mut to_net,
            &mut i2c,
            &mut delay,
            &loopback_mode,
            &mut sframe_state,
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
        let mut sframe_state = sframe::SFrameState::new(&[0u8; 16], 0);
        handle_mgmt(
            tlv,
            &mut to_mgmt,
            &mut to_net,
            &mut i2c,
            &mut delay,
            &loopback_mode,
            &mut sframe_state,
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
        let mut sframe_state = sframe::SFrameState::new(&[0u8; 16], 0);
        handle_mgmt(
            tlv,
            &mut to_mgmt,
            &mut to_net,
            &mut i2c,
            &mut delay,
            &loopback_mode,
            &mut sframe_state,
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
        let mut sframe_state = sframe::SFrameState::new(&[0u8; 16], 0);
        handle_mgmt(
            tlv,
            &mut to_mgmt,
            &mut to_net,
            &mut i2c,
            &mut delay,
            &loopback_mode,
            &mut sframe_state,
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
        let mut sframe_state = sframe::SFrameState::new(&[0u8; 16], 0);
        handle_mgmt(
            tlv,
            &mut to_mgmt,
            &mut to_net,
            &mut i2c,
            &mut delay,
            &loopback_mode,
            &mut sframe_state,
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
    /// Routes frames to frames_a (Ptt) or frames_b (PttAi) based on channel_id.
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
            use crate::shared::{ChannelId, Tlv};
            loop {
                let result: Result<Option<Tlv<UiToNet>>, _> = reader.read_tlv().await;
                if let Ok(Some(tlv)) = result {
                    match tlv.tlv_type {
                        // Legacy formats (backwards compatibility)
                        UiToNet::AudioFrameA => {
                            self.frames_a.lock().unwrap().push(tlv.value.to_vec());
                        }
                        UiToNet::AudioFrameB => {
                            self.frames_b.lock().unwrap().push(tlv.value.to_vec());
                        }
                        // New hactar format: channel_id (1 byte) + encrypted chunk
                        UiToNet::AudioFrame => {
                            if let Some(&channel_id) = tlv.value.first() {
                                let payload = tlv.value[1..].to_vec();
                                match ChannelId::try_from(channel_id) {
                                    Ok(ChannelId::Ptt) => {
                                        self.frames_a.lock().unwrap().push(payload);
                                    }
                                    Ok(ChannelId::PttAi) => {
                                        self.frames_b.lock().unwrap().push(payload);
                                    }
                                    _ => {}
                                }
                            }
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

        // Each frame should be 183-184 bytes:
        // 6 (chunk header) + 160 (A-law audio) + 1-2 (SFrame header) + 16 (auth tag)
        // SFrame header size varies: 1 byte when counter < 8, 2 bytes when counter >= 8
        for frame in frames.iter() {
            assert!(
                frame.len() >= 183 && frame.len() <= 184,
                "Frame should be 183-184 bytes (encrypted chunk), got {}",
                frame.len()
            );
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

        // Should have received AudioFrameB TLVs (PttAi channel)
        let frames = frames_b.lock().unwrap();
        assert!(
            frames.len() >= 2,
            "Expected at least 2 frames, got {}",
            frames.len()
        );

        // Each frame should be 187-188 bytes:
        // 10 (AIRequest chunk header) + 160 (A-law audio) + 1-2 (SFrame header) + 16 (auth tag)
        // SFrame header size varies: 1 byte when counter < 8, 2 bytes when counter >= 8
        for frame in frames.iter() {
            assert!(
                frame.len() >= 187 && frame.len() <= 188,
                "Frame should be 187-188 bytes (encrypted AIRequest chunk), got {}",
                frame.len()
            );
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

    /// Helper to encrypt a frame for testing in hactar format.
    /// Returns: channel_id (1 byte) + encrypted chunk (SFrame header + encrypted data + auth tag)
    fn encrypt_frame_for_test(
        frame: &crate::ui::Frame,
        sframe: &mut sframe::SFrameState,
    ) -> heapless::Vec<u8, 256> {
        use crate::shared::{chunk, ChannelId};

        // Serialize frame into chunk format
        let mut chunk_buf = [0u8; 200];
        let chunk_len = chunk::serialize_media_chunk(frame.as_bytes(), false, &mut chunk_buf);

        // Encrypt the chunk
        let mut encrypted: heapless::Vec<u8, 256> = heapless::Vec::new();
        encrypted
            .extend_from_slice(&chunk_buf[..chunk_len])
            .unwrap();
        sframe.protect(&[], &mut encrypted).unwrap();

        // Build output: channel_id (plaintext) + encrypted chunk
        let mut out: heapless::Vec<u8, 256> = heapless::Vec::new();
        out.push(ChannelId::Ptt as u8).unwrap();
        out.extend_from_slice(&encrypted).unwrap();
        out
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

        // Create SFrame state with default EEPROM key (all 0xFF, not zeros)
        let mut sframe_state = sframe::SFrameState::new(&[0xff; 16], 0);

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
                    let encrypted = encrypt_frame_for_test(&padding_frame, &mut sframe_state);
                    net_to_ui
                        .write_tlv(crate::shared::NetToUi::AudioFrame, &encrypted)
                        .await
                        .unwrap();
                }

                // Create a test frame with known A-law encoded data
                // Use encode_alaw to get predictable decoded values
                let mut test_frame = crate::ui::Frame::default();
                test_frame.0[0] = encode_alaw(1000);  // Encode known PCM values
                test_frame.0[1] = encode_alaw(2000);
                test_frame.0[159] = encode_alaw(3000); // Last sample in 160-byte frame

                // Encrypt and send the audio frame from NET to UI
                let encrypted = encrypt_frame_for_test(&test_frame, &mut sframe_state);
                net_to_ui
                    .write_tlv(crate::shared::NetToUi::AudioFrame, &encrypted)
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

        // Create SFrame state with default EEPROM key (all 0xFF, not zeros)
        let mut sframe_state = sframe::SFrameState::new(&[0xff; 16], 0);

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
                    let encrypted = encrypt_frame_for_test(&padding_frame, &mut sframe_state);
                    net_to_ui
                        .write_tlv(crate::shared::NetToUi::AudioFrame, &encrypted)
                        .await
                        .unwrap();
                }

                // Send multiple frames with distinct A-law markers
                for (alaw, _) in &markers {
                    let mut frame = crate::ui::Frame::default();
                    frame.0[0] = *alaw;
                    let encrypted = encrypt_frame_for_test(&frame, &mut sframe_state);
                    net_to_ui
                        .write_tlv(crate::shared::NetToUi::AudioFrame, &encrypted)
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

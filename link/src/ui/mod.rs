//! UI (User Interface) chip - handles buttons and user interaction.

mod audio;
mod eeprom;
mod sframe;

pub use audio::{AudioCodec, AudioControl, AudioError, AudioStream, Frame, FRAME_SIZE};
pub use eeprom::Eeprom;

use crate::info;
use crate::shared::{
    read_tlv_loop, Channel, Color, CriticalSectionRawMutex, Led, MgmtToUi, NetToUi, Sender, Tlv,
    UiToMgmt, UiToNet, WriteTlv,
};
use embedded_hal::delay::DelayNs;
use embedded_hal::digital::StatefulOutputPin;
use embedded_hal::i2c::I2c;
use embedded_hal_async::digital::Wait;
use embedded_io_async::{Read, Write};

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
    /// An audio frame was read from the microphone
    AudioFrame(Frame),
}

pub struct App<W, R, LR, LG, LB, BA, BB, BM, I, D, AS> {
    to_mgmt: W,
    to_net: W,
    from_mgmt: R,
    from_net: R,
    led: (LR, LG, LB),
    button_a: BA,
    button_b: BB,
    button_mic: BM,
    i2c: I,
    delay: D,
    audio_stream: AS,
}

impl<W, R, LR, LG, LB, BA, BB, BM, I, D, AS> App<W, R, LR, LG, LB, BA, BB, BM, I, D, AS>
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
    AS: AudioStream,
{
    pub fn new(
        to_mgmt: W,
        from_mgmt: R,
        to_net: W,
        from_net: R,
        led: (LR, LG, LB),
        button_a: BA,
        button_b: BB,
        button_mic: BM,
        i2c: I,
        delay: D,
        audio_stream: AS,
    ) -> Self {
        Self {
            to_mgmt,
            to_net,
            from_mgmt,
            from_net,
            led,
            button_a,
            button_b,
            button_mic,
            i2c,
            delay,
            audio_stream,
        }
    }

    /// Initialize the audio codec using the shared I2C bus.
    pub fn init_audio(&mut self) {
        let mut audio = AudioControl::new(&mut self.i2c);
        audio.init();
        audio.enable_input(true);
        audio.enable_output(true);
    }

    #[allow(unreachable_code)]
    pub async fn run(mut self) -> ! {
        info!("ui: starting");

        // Initialize audio codec before starting
        self.init_audio();

        let Self {
            mut to_mgmt,
            mut to_net,
            from_mgmt,
            from_net,
            led,
            button_a,
            button_b,
            button_mic,
            mut i2c,
            mut delay,
            mut audio_stream,
        } = self;

        // Initialize LED
        let mut led = Led::new(led.0, led.1, led.2);
        led.set(Color::Blue);

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

        let handle_task = async {
            info!("ui: ready to handle events");

            // Track which button is currently held (if any)
            let mut active_button: Option<Button> = None;

            // TX (microphone) frame statistics
            let mut tx_frame_count: u32 = 0;
            let mut tx_energy_sum: u64 = 0;

            // RX (playback) frame statistics
            let mut rx_frame_count: u32 = 0;
            let mut rx_energy_sum: u64 = 0;

            loop {
                match channel.receive().await {
                    Event::Mgmt(tlv) => {
                        handle_mgmt(tlv, &mut to_mgmt, &mut to_net, &mut i2c, &mut delay).await
                    }
                    Event::Net(tlv) => {
                        if let Some(frame) = handle_net(tlv, &mut to_mgmt).await {
                            // Track playback frame energy
                            rx_energy_sum += frame.energy() as u64;
                            rx_frame_count += 1;
                            if rx_frame_count % 50 == 0 {
                                let avg_energy = rx_energy_sum / 50;
                                info!(
                                    "ui: received {} playback frames, avg energy={}",
                                    rx_frame_count, avg_energy
                                );
                                rx_energy_sum = 0;
                            }

                            // Queue the playback frame
                            playback_channel.send(frame).await;
                        }
                    }
                    Event::ButtonDown(button) => {
                        info!("ui: button {:?} down", button);
                        if active_button.is_none() {
                            active_button = Some(button);
                            tx_frame_count = 0;
                            tx_energy_sum = 0;
                        }
                    }
                    Event::ButtonUp(button) => {
                        info!("ui: button {:?} up", button);
                        if active_button == Some(button) {
                            active_button = None;
                        }
                    }
                    Event::AudioFrame(frame) => {
                        // Audio frame read from microphone - send if button is held
                        if let Some(button) = active_button {
                            let tlv_type = match button {
                                Button::A => UiToNet::AudioFrameA,
                                Button::B => UiToNet::AudioFrameB,
                            };

                            // Log the energy of every transmitted frame
                            info!("ui: audio frame energy={}", frame.energy());

                            // Track energy
                            tx_energy_sum += frame.energy() as u64;

                            let bytes = frame.as_bytes();
                            to_net.must_write_tlv(tlv_type, &bytes).await;

                            tx_frame_count += 1;
                            if tx_frame_count % 50 == 0 {
                                let avg_energy = tx_energy_sum / 50;
                                info!(
                                    "ui: sent {} audio frames, avg energy={}",
                                    tx_frame_count, avg_energy
                                );
                                tx_energy_sum = 0;
                            }
                        }
                    }
                }
            }
        };

        // Audio I/O task: reads from microphone, writes queued playback frames
        let audio_task = async {
            audio_stream.start().await;
            loop {
                // Get a frame to play (or silence if queue is empty)
                let tx_frame = playback_channel.try_receive().unwrap_or_default();
                let mut rx_frame = Frame::default();

                // Do the I2S read/write cycle
                if audio_stream
                    .read_write(&tx_frame, &mut rx_frame)
                    .await
                    .is_ok()
                {
                    // Try to send the recorded frame - drop if channel is full
                    // This prevents blocking the audio task if event handler is slow
                    let _ = channel.try_send(Event::AudioFrame(rx_frame));
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
            to_mgmt.must_write_tlv(UiToMgmt::Ack, &[]).await;
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
    use crate::mocks::{mock_i2c_with_eeprom, MockDelay};
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

        handle_mgmt(tlv, &mut to_mgmt, &mut to_net, &mut i2c, &mut delay).await;

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

        handle_mgmt(tlv, &mut to_mgmt, &mut to_net, &mut i2c, &mut delay).await;

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

        handle_mgmt(tlv, &mut to_mgmt, &mut to_net, &mut i2c, &mut delay).await;

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

        handle_mgmt(tlv, &mut to_mgmt, &mut to_net, &mut i2c, &mut delay).await;

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

        handle_mgmt(tlv, &mut to_mgmt, &mut to_net, &mut i2c, &mut delay).await;

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

        handle_mgmt(tlv, &mut to_mgmt, &mut to_net, &mut i2c, &mut delay).await;

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
    use crate::mocks::{
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

        let ui_app = App::new(
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
        );

        let collector = TlvCollector::new();
        let frames_a = collector.frames_a();

        tokio::select! {
            _ = ui_app.run() => unreachable!(),
            _ = collector.collect_from(net_from_ui) => unreachable!(),
            _ = async {
                // Wait a bit for the app to start
                tokio::time::sleep(Duration::from_millis(10)).await;

                // Press button A
                button_a_ctrl.press().await;

                // Wait for at least 2 audio frames (20ms each + some margin)
                tokio::time::sleep(Duration::from_millis(60)).await;

                // Release button A
                button_a_ctrl.release().await;

                // Wait a bit to ensure no more frames after release
                tokio::time::sleep(Duration::from_millis(50)).await;
            } => {}
        }

        // Should have received at least 2 AudioFrameA TLVs while button was held
        let frames = frames_a.lock().unwrap();
        assert!(
            frames.len() >= 2,
            "Expected at least 2 frames, got {}",
            frames.len()
        );

        // Each frame should be 640 bytes (320 u16 samples)
        for frame in frames.iter() {
            assert_eq!(frame.len(), 640, "Frame should be 640 bytes");
        }
    }

    #[tokio::test]
    async fn mic_button_sends_audio_frame_a() {
        let (ui_to_mgmt, _mgmt_from_ui) = channel();
        let (_mgmt_to_ui, ui_from_mgmt) = channel();
        let (ui_to_net, net_from_ui) = channel();
        let (_net_to_ui, ui_from_net) = channel();

        let (button_mic, button_mic_ctrl) = ControllableButton::new();

        let ui_app = App::new(
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
        );

        let collector = TlvCollector::new();
        let frames_a = collector.frames_a();
        let frames_b = collector.frames_b();

        tokio::select! {
            _ = ui_app.run() => unreachable!(),
            _ = collector.collect_from(net_from_ui) => unreachable!(),
            _ = async {
                // Wait a bit for the app to start
                tokio::time::sleep(Duration::from_millis(10)).await;

                // Press mic button (active-low)
                button_mic_ctrl.press_active_low().await;

                // Wait for at least 2 audio frames
                tokio::time::sleep(Duration::from_millis(60)).await;

                // Release mic button (active-low)
                button_mic_ctrl.release_active_low().await;

                // Wait a bit
                tokio::time::sleep(Duration::from_millis(50)).await;
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

        let ui_app = App::new(
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
        );

        let collector = TlvCollector::new();
        let frames_b = collector.frames_b();

        tokio::select! {
            _ = ui_app.run() => unreachable!(),
            _ = collector.collect_from(net_from_ui) => unreachable!(),
            _ = async {
                // Wait a bit for the app to start
                tokio::time::sleep(Duration::from_millis(10)).await;

                // Press button B
                button_b_ctrl.press().await;

                // Wait for at least 2 audio frames
                tokio::time::sleep(Duration::from_millis(60)).await;

                // Release button B
                button_b_ctrl.release().await;

                // Wait a bit
                tokio::time::sleep(Duration::from_millis(50)).await;
            } => {}
        }

        // Should have received AudioFrameB TLVs
        let frames = frames_b.lock().unwrap();
        assert!(
            frames.len() >= 2,
            "Expected at least 2 frames, got {}",
            frames.len()
        );

        // Each frame should be 640 bytes
        for frame in frames.iter() {
            assert_eq!(frame.len(), 640, "Frame should be 640 bytes");
        }
    }

    #[tokio::test]
    async fn no_audio_when_button_not_pressed() {
        let (ui_to_mgmt, _mgmt_from_ui) = channel();
        let (_mgmt_to_ui, ui_from_mgmt) = channel();
        let (ui_to_net, net_from_ui) = channel();
        let (_net_to_ui, ui_from_net) = channel();

        let ui_app = App::new(
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
        );

        let collector = TlvCollector::new();
        let frames_a = collector.frames_a();
        let frames_b = collector.frames_b();

        tokio::select! {
            _ = ui_app.run() => unreachable!(),
            _ = collector.collect_from(net_from_ui) => unreachable!(),
            _ = async {
                // Wait for several audio frame periods without pressing any button
                tokio::time::sleep(Duration::from_millis(100)).await;
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

        let ui_app = App::new(
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
        );

        let collector = TlvCollector::new();
        let frames_a = collector.frames_a();

        tokio::select! {
            _ = ui_app.run() => unreachable!(),
            _ = collector.collect_from(net_from_ui) => unreachable!(),
            _ = async {
                // Wait for app to start
                tokio::time::sleep(Duration::from_millis(10)).await;

                // Press button A briefly
                button_a_ctrl.press().await;
                tokio::time::sleep(Duration::from_millis(50)).await;
                button_a_ctrl.release().await;

                // Record frame count after release
                tokio::time::sleep(Duration::from_millis(10)).await;
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

        let ui_app = App::new(
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
        );

        let collector = TlvCollector::new();
        let frames_a = collector.frames_a();
        let frames_b = collector.frames_b();

        tokio::select! {
            _ = ui_app.run() => unreachable!(),
            _ = collector.collect_from(net_from_ui) => unreachable!(),
            _ = async {
                // Wait for app to start
                tokio::time::sleep(Duration::from_millis(10)).await;

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
        use crate::mocks::CapturingAudioStream;
        use crate::shared::WriteTlv;

        let (ui_to_mgmt, _mgmt_from_ui) = channel();
        let (_mgmt_to_ui, ui_from_mgmt) = channel();
        let (ui_to_net, _net_from_ui) = channel();
        let (mut net_to_ui, ui_from_net) = channel();

        let (audio_stream, written_frames) = CapturingAudioStream::new();

        let ui_app = App::new(
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
        );

        tokio::select! {
            _ = ui_app.run() => unreachable!(),
            _ = async {
                // Wait for app to start
                tokio::time::sleep(Duration::from_millis(10)).await;

                // Create a test frame with known data
                let mut test_frame = crate::ui::Frame::default();
                test_frame.0[0] = 0x1234;
                test_frame.0[1] = 0x5678;
                test_frame.0[319] = 0xABCD;

                // Send the audio frame from NET to UI
                net_to_ui
                    .write_tlv(crate::shared::NetToUi::AudioFrame, &test_frame.as_bytes())
                    .await
                    .unwrap();

                // Wait for the frame to be processed and played
                tokio::time::sleep(Duration::from_millis(100)).await;
            } => {}
        }

        // Verify the frame was played out
        let frames = written_frames.lock().unwrap();
        assert!(
            frames.len() >= 1,
            "Expected at least 1 playback frame, got {}",
            frames.len()
        );

        // Find our test frame
        let found = frames
            .iter()
            .any(|f| f.0[0] == 0x1234 && f.0[1] == 0x5678 && f.0[319] == 0xABCD);
        assert!(found, "Test frame should have been played out");
    }

    #[tokio::test]
    async fn multiple_net_audio_frames_play_in_order() {
        use crate::mocks::CapturingAudioStream;
        use crate::shared::WriteTlv;

        let (ui_to_mgmt, _mgmt_from_ui) = channel();
        let (_mgmt_to_ui, ui_from_mgmt) = channel();
        let (ui_to_net, _net_from_ui) = channel();
        let (mut net_to_ui, ui_from_net) = channel();

        let (audio_stream, written_frames) = CapturingAudioStream::new();

        let ui_app = App::new(
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
        );

        tokio::select! {
            _ = ui_app.run() => unreachable!(),
            _ = async {
                // Wait for app to start
                tokio::time::sleep(Duration::from_millis(10)).await;

                // Send multiple frames with sequence numbers
                for i in 0u16..3 {
                    let mut frame = crate::ui::Frame::default();
                    frame.0[0] = i + 100; // Use 100, 101, 102 as markers
                    net_to_ui
                        .write_tlv(crate::shared::NetToUi::AudioFrame, &frame.as_bytes())
                        .await
                        .unwrap();
                    // Small delay between sends
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }

                // Wait for all frames to be processed
                tokio::time::sleep(Duration::from_millis(150)).await;
            } => {}
        }

        // Verify frames were played
        let frames = written_frames.lock().unwrap();

        // Find our test frames (filter out zeros)
        let test_frames: Vec<_> = frames
            .iter()
            .filter(|f| f.0[0] >= 100 && f.0[0] <= 102)
            .collect();

        assert!(
            test_frames.len() >= 3,
            "Expected at least 3 playback frames, got {}",
            test_frames.len()
        );

        // Verify order (frames should arrive in sequence)
        for (i, frame) in test_frames.iter().enumerate() {
            assert_eq!(
                frame.0[0],
                100 + i as u16,
                "Frame {} should have marker {}, got {}",
                i,
                100 + i,
                frame.0[0]
            );
        }
    }
}

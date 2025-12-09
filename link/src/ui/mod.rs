//! UI (User Interface) chip - handles buttons and user interaction.

mod eeprom;
pub use eeprom::Eeprom;

use crate::info;
use crate::shared::{
    read_tlv_loop, Channel, Color, Led, MgmtToUi, NetToUi, RawMutex, Sender, Tlv, UiToMgmt,
    UiToNet, WriteTlv,
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
}

pub struct App<W, R, LR, LG, LB, BA, BB, I, D> {
    to_mgmt: W,
    to_net: W,
    from_mgmt: R,
    from_net: R,
    led: (LR, LG, LB),
    button_a: BA,
    button_b: BB,
    eeprom: Eeprom<I, D>,
}

impl<W, R, LR, LG, LB, BA, BB, I, D> App<W, R, LR, LG, LB, BA, BB, I, D>
where
    W: Write,
    R: Read,
    LR: StatefulOutputPin,
    LG: StatefulOutputPin,
    LB: StatefulOutputPin,
    BA: Wait,
    BB: Wait,
    I: I2c,
    D: DelayNs,
{
    pub fn new(
        to_mgmt: W,
        from_mgmt: R,
        to_net: W,
        from_net: R,
        led: (LR, LG, LB),
        button_a: BA,
        button_b: BB,
        i2c: I,
        delay: D,
    ) -> Self {
        Self {
            to_mgmt,
            to_net,
            from_mgmt,
            from_net,
            led,
            button_a,
            button_b,
            eeprom: Eeprom::new(i2c, delay),
        }
    }

    #[allow(unreachable_code)]
    pub async fn run(self) -> ! {
        info!("ui: starting");

        let Self {
            mut to_mgmt,
            mut to_net,
            from_mgmt,
            from_net,
            led,
            button_a,
            button_b,
            mut eeprom,
        } = self;

        // Initialize LED
        let mut led = Led::new(led.0, led.1, led.2);
        led.set(Color::Blue);

        const MAX_QUEUE_DEPTH: usize = 4;
        let channel: Channel<RawMutex, Event, MAX_QUEUE_DEPTH> = Channel::new();

        let mgmt_read_task = read_tlv_loop(from_mgmt, channel.sender(), Event::Mgmt);
        let net_read_task = read_tlv_loop(from_net, channel.sender(), Event::Net);
        let button_a_task = button_monitor(button_a, Button::A, channel.sender());
        let button_b_task = button_monitor(button_b, Button::B, channel.sender());

        let handle_task = async {
            info!("ui: ready to handle events");
            loop {
                match channel.receive().await {
                    Event::Mgmt(tlv) => {
                        handle_mgmt(tlv, &mut to_mgmt, &mut to_net, &mut eeprom).await
                    }
                    Event::Net(tlv) => handle_net(tlv, &mut to_mgmt).await,
                    Event::ButtonDown(button) => {
                        info!("ui: button {:?} down", button);
                    }
                    Event::ButtonUp(button) => {
                        info!("ui: button {:?} up", button);
                    }
                }
            }
        };

        futures::join!(
            mgmt_read_task,
            net_read_task,
            button_a_task,
            button_b_task,
            handle_task
        );
        unreachable!()
    }
}

async fn button_monitor<'a, B: Wait, const N: usize>(
    mut button: B,
    which: Button,
    sender: Sender<'a, RawMutex, Event, N>,
) -> ! {
    loop {
        // Wait for button press (rising edge - active high with pull-down)
        let _ = button.wait_for_rising_edge().await;
        sender.send(Event::ButtonDown(which)).await;

        // Wait for button release (falling edge)
        let _ = button.wait_for_falling_edge().await;
        sender.send(Event::ButtonUp(which)).await;
    }
}

async fn handle_mgmt<M, N, I, D>(
    tlv: Tlv<MgmtToUi>,
    to_mgmt: &mut M,
    to_net: &mut N,
    eeprom: &mut Eeprom<I, D>,
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
                return;
            }
            let version =
                u32::from_be_bytes([tlv.value[0], tlv.value[1], tlv.value[2], tlv.value[3]]);
            if let Err(_) = eeprom.set_version(version) {
                info!("ui: failed to write version to EEPROM");
            }
        }
        MgmtToUi::GetSFrameKey => {
            info!("ui: get sframe key");
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
                return;
            }
            let mut key = [0u8; 16];
            key.copy_from_slice(&tlv.value[..16]);
            if let Err(_) = eeprom.set_sframe_key(&key) {
                info!("ui: failed to write sframe key to EEPROM");
            }
        }
    }
}

async fn handle_net<M>(tlv: Tlv<NetToUi>, to_mgmt: &mut M)
where
    M: WriteTlv<UiToMgmt>,
{
    match tlv.tlv_type {
        NetToUi::CircularPing => {
            info!("ui: net circular ping -> mgmt");
            to_mgmt
                .must_write_tlv(UiToMgmt::CircularPing, &tlv.value)
                .await;
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
        let mut eeprom = Eeprom::new(mock_i2c_with_eeprom(), MockDelay);

        // Set a known version
        eeprom.set_version(0xaabbccdd).unwrap();

        // Create GetVersion TLV
        let tlv = Tlv {
            tlv_type: MgmtToUi::GetVersion,
            value: Value::new(),
        };

        handle_mgmt(tlv, &mut to_mgmt, &mut to_net, &mut eeprom).await;

        assert_eq!(to_mgmt.written.len(), 1);
        assert_eq!(to_mgmt.written[0].0, UiToMgmt::Version);
        assert_eq!(to_mgmt.written[0].1, &[0xaa, 0xbb, 0xcc, 0xdd]);
    }

    #[tokio::test]
    async fn tlv_set_version() {
        let mut to_mgmt = MockTlvWriter::new();
        let mut to_net = DummyNetWriter;
        let mut eeprom = Eeprom::new(mock_i2c_with_eeprom(), MockDelay);

        // Create SetVersion TLV with version 0x11223344
        let mut value: Value = Value::new();
        value.extend_from_slice(&[0x11, 0x22, 0x33, 0x44]).unwrap();
        let tlv = Tlv {
            tlv_type: MgmtToUi::SetVersion,
            value,
        };

        handle_mgmt(tlv, &mut to_mgmt, &mut to_net, &mut eeprom).await;

        // Verify version was set (no response TLV for SetVersion)
        assert_eq!(to_mgmt.written.len(), 0);
        assert_eq!(eeprom.get_version().unwrap(), 0x11223344);
    }

    #[tokio::test]
    async fn tlv_get_sframe_key() {
        let mut to_mgmt = MockTlvWriter::new();
        let mut to_net = DummyNetWriter;
        let mut eeprom = Eeprom::new(mock_i2c_with_eeprom(), MockDelay);

        // Set a known key
        let key = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
        eeprom.set_sframe_key(&key).unwrap();

        // Create GetSFrameKey TLV
        let tlv = Tlv {
            tlv_type: MgmtToUi::GetSFrameKey,
            value: Value::new(),
        };

        handle_mgmt(tlv, &mut to_mgmt, &mut to_net, &mut eeprom).await;

        assert_eq!(to_mgmt.written.len(), 1);
        assert_eq!(to_mgmt.written[0].0, UiToMgmt::SFrameKey);
        assert_eq!(to_mgmt.written[0].1, &key);
    }

    #[tokio::test]
    async fn tlv_set_sframe_key() {
        let mut to_mgmt = MockTlvWriter::new();
        let mut to_net = DummyNetWriter;
        let mut eeprom = Eeprom::new(mock_i2c_with_eeprom(), MockDelay);

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

        handle_mgmt(tlv, &mut to_mgmt, &mut to_net, &mut eeprom).await;

        // Verify key was set (no response TLV for SetSFrameKey)
        assert_eq!(to_mgmt.written.len(), 0);
        assert_eq!(eeprom.get_sframe_key().unwrap(), key);
    }

    #[tokio::test]
    async fn tlv_set_version_invalid_length() {
        let mut to_mgmt = MockTlvWriter::new();
        let mut to_net = DummyNetWriter;
        let mut eeprom = Eeprom::new(mock_i2c_with_eeprom(), MockDelay);

        // Create SetVersion TLV with only 2 bytes (invalid)
        let mut value: Value = Value::new();
        value.extend_from_slice(&[0x11, 0x22]).unwrap();
        let tlv = Tlv {
            tlv_type: MgmtToUi::SetVersion,
            value,
        };

        handle_mgmt(tlv, &mut to_mgmt, &mut to_net, &mut eeprom).await;

        // Version should remain default (0xffffffff)
        assert_eq!(eeprom.get_version().unwrap(), 0xffffffff);
    }

    #[tokio::test]
    async fn tlv_set_sframe_key_invalid_length() {
        let mut to_mgmt = MockTlvWriter::new();
        let mut to_net = DummyNetWriter;
        let mut eeprom = Eeprom::new(mock_i2c_with_eeprom(), MockDelay);

        // Create SetSFrameKey TLV with only 8 bytes (invalid)
        let mut value: Value = Value::new();
        value.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]).unwrap();
        let tlv = Tlv {
            tlv_type: MgmtToUi::SetSFrameKey,
            value,
        };

        handle_mgmt(tlv, &mut to_mgmt, &mut to_net, &mut eeprom).await;

        // Key should remain default (0xff)
        assert_eq!(eeprom.get_sframe_key().unwrap(), [0xff; 16]);
    }
}

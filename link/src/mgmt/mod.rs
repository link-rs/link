//! MGMT (Management) chip - coordinates communication between all chips.

use crate::info;
use crate::shared::{
    Color, CtlToMgmt, Led, MgmtToCtl, MgmtToNet, MgmtToUi, ReadTlv, Tlv, Value, WriteTlv,
};
use embedded_hal::digital::{OutputPin, StatefulOutputPin};
use embedded_hal_async::delay::DelayNs;
use embedded_io_async::{Read, Write};

/// Holds the GPIO pins used to control the UI chip's reset behavior.
///
/// UI chip boot mode:
/// - BOOT0=1, BOOT1=0 = Enter bootloader (system memory)
/// - BOOT0=0, BOOT1=1 = Normal operation (boot from flash)
pub struct UiResetPins<Boot0, Boot1, Rst> {
    pub boot0: Boot0,
    pub boot1: Boot1,
    pub rst: Rst,
}

impl<Boot0, Boot1, Rst> UiResetPins<Boot0, Boot1, Rst>
where
    Boot0: StatefulOutputPin,
    Boot1: StatefulOutputPin,
    Rst: StatefulOutputPin,
{
    pub fn new(boot0: Boot0, boot1: Boot1, rst: Rst) -> Self {
        Self { boot0, boot1, rst }
    }

    /// Reset UI chip into bootloader mode.
    /// Sets BOOT0=1, BOOT1=0, then power cycles.
    pub async fn reset_to_bootloader<D: DelayNs>(&mut self, delay: &mut D) {
        // Set boot pins for bootloader mode (BOOT0=1, BOOT1=0)
        let _ = self.boot0.set_high();
        let _ = self.boot1.set_low();

        // Power cycle: RST low -> delay -> RST high
        let _ = self.rst.set_low();
        delay.delay_ms(10).await;
        let _ = self.rst.set_high();
    }

    /// Reset UI chip into user mode.
    /// Sets BOOT0=0, BOOT1=1, then power cycles.
    pub async fn reset_to_user<D: DelayNs>(&mut self, delay: &mut D) {
        // Set boot pins for normal mode (BOOT0=0, BOOT1=1)
        let _ = self.boot0.set_low();
        let _ = self.boot1.set_high();

        // Power cycle: RST low -> delay -> RST high
        let _ = self.rst.set_low();
        delay.delay_ms(10).await;
        let _ = self.rst.set_high();
    }

    /// Hold UI chip in reset (RST low).
    pub fn hold_reset(&mut self) {
        let _ = self.rst.set_low();
    }

    /// Release UI chip from reset (RST high).
    pub fn release_reset(&mut self) {
        let _ = self.rst.set_high();
    }
}

/// Holds the GPIO pins used to control the NET chip's reset behavior.
///
/// NET chip boot mode is inverted from UI chip:
/// - BOOT low = Enter bootloader
/// - BOOT high = Normal operation (boot from flash)
pub struct NetResetPins<Boot, Rst> {
    pub boot: Boot,
    pub rst: Rst,
}

impl<Boot, Rst> NetResetPins<Boot, Rst>
where
    Boot: OutputPin,
    Rst: OutputPin,
{
    pub fn new(boot: Boot, rst: Rst) -> Self {
        Self { boot, rst }
    }

    /// Reset NET chip into bootloader mode.
    /// Sequence matches C code: power cycle, then BOOT low, then power cycle again.
    /// BOOT must be low when RST goes high for ESP32 to enter bootloader.
    pub async fn reset_to_bootloader<D: DelayNs>(&mut self, delay: &mut D) {
        // First power cycle (clean slate)
        let _ = self.rst.set_low();
        delay.delay_ms(10).await;
        let _ = self.rst.set_high();

        // Set BOOT low for bootloader mode
        let _ = self.boot.set_low();

        // Second power cycle - ESP32 samples BOOT when RST goes high
        let _ = self.rst.set_low();
        delay.delay_ms(10).await;
        let _ = self.rst.set_high();
        // BOOT stays low
    }

    /// Reset NET chip into user mode.
    /// Sequence: BOOT high -> RST low -> delay -> RST high
    pub async fn reset_to_user<D: DelayNs>(&mut self, delay: &mut D) {
        let _ = self.boot.set_high();
        let _ = self.rst.set_low();
        delay.delay_ms(10).await;
        let _ = self.rst.set_high();
    }
}

/// Type alias for backwards compatibility.
pub type Esp32ResetPins<Boot, Rst> = NetResetPins<Boot, Rst>;

#[allow(unreachable_code)]
pub async fn run<W, R, RA, GA, BA, RB, GB, BB, UiBoot0, UiBoot1, UiRst, NetBoot, NetRst, D>(
    to_ctl: W,
    mut from_ctl: R,
    mut to_ui: W,
    mut from_ui: R,
    mut to_net: W,
    mut from_net: R,
    led_a: (RA, GA, BA),
    led_b: (RB, GB, BB),
    mut ui_reset_pins: UiResetPins<UiBoot0, UiBoot1, UiRst>,
    mut net_reset_pins: NetResetPins<NetBoot, NetRst>,
    mut delay: D,
) -> !
where
    W: Write,
    R: Read,
    RA: StatefulOutputPin,
    GA: StatefulOutputPin,
    BA: StatefulOutputPin,
    RB: StatefulOutputPin,
    GB: StatefulOutputPin,
    BB: StatefulOutputPin,
    UiBoot0: StatefulOutputPin,
    UiBoot1: StatefulOutputPin,
    UiRst: StatefulOutputPin,
    NetBoot: OutputPin,
    NetRst: OutputPin,
    D: DelayNs,
{
    use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};

    info!("mgmt: starting");

    // Initialize LEDs
    let mut led_a = Led::new(led_a.0, led_a.1, led_a.2);
    let mut led_b = Led::new(led_b.0, led_b.1, led_b.2);
    led_a.set(Color::Green);
    led_b.set(Color::Red);

    // UI and NET chips are held in reset at boot (RST low).
    // Wait for MGMT clocks to stabilize, then release them to boot.
    delay.delay_ms(50).await;
    info!("mgmt: releasing UI and NET from reset");
    let _ = ui_reset_pins.rst.set_high();
    let _ = net_reset_pins.rst.set_high();

    let to_ctl: Mutex<NoopRawMutex, _> = Mutex::new(to_ctl);
    let reset_state: Mutex<NoopRawMutex, _> = Mutex::new((ui_reset_pins, net_reset_pins, delay));

    let ui_task = async {
        let mut buffer = Value::default();
        loop {
            buffer.resize(buffer.capacity(), 0).unwrap();
            let Ok(n) = from_ui.read(&mut buffer).await else {
                info!("ui->mgmt: error!");
                continue;
            };
            buffer.truncate(n);
            info!("ui->ctl: {=[u8]:x}", buffer.as_slice());

            let mut to_ctl = to_ctl.lock().await;
            let _ = to_ctl.write_tlv(MgmtToCtl::FromUi, &buffer).await;
        }
    };

    let net_task = async {
        let mut buffer = Value::default();
        loop {
            buffer.resize(buffer.capacity(), 0).unwrap();
            let Ok(n) = from_net.read(&mut buffer).await else {
                continue;
            };
            buffer.truncate(n);
            info!("net->ctl: {=[u8]:x}", &buffer);

            let mut to_ctl = to_ctl.lock().await;
            let _ = to_ctl.write_tlv(MgmtToCtl::FromNet, &buffer).await;
        }
    };

    let ctl_task = async {
        use core::ops::DerefMut;
        loop {
            let Ok(Some(tlv)) = from_ctl.read_tlv().await else {
                continue;
            };

            let mut to_ctl = to_ctl.lock().await;
            let mut reset_state = reset_state.lock().await;
            let (ui_pins, net_pins, delay) = reset_state.deref_mut();
            handle_ctl(
                tlv,
                to_ctl.deref_mut(),
                &mut to_ui,
                &mut to_net,
                ui_pins,
                net_pins,
                delay,
            )
            .await;
        }
    };

    embassy_futures::join::join3(ctl_task, ui_task, net_task).await;
    unreachable!()
}

async fn handle_ctl<C, U, N, UiBoot0, UiBoot1, UiRst, NetBoot, NetRst, D>(
    tlv: Tlv<CtlToMgmt>,
    to_ctl: &mut C,
    to_ui: &mut U,
    to_net: &mut N,
    ui_reset_pins: &mut UiResetPins<UiBoot0, UiBoot1, UiRst>,
    net_reset_pins: &mut NetResetPins<NetBoot, NetRst>,
    delay: &mut D,
) where
    C: WriteTlv<MgmtToCtl>,
    U: WriteTlv<MgmtToUi> + Write,
    N: WriteTlv<MgmtToNet> + Write,
    UiBoot0: StatefulOutputPin,
    UiBoot1: StatefulOutputPin,
    UiRst: StatefulOutputPin,
    NetBoot: OutputPin,
    NetRst: OutputPin,
    D: DelayNs,
{
    match tlv.tlv_type {
        CtlToMgmt::Ping => {
            info!("mgmt: ctl ping, sending pong");
            to_ctl.must_write_tlv(MgmtToCtl::Pong, &tlv.value).await;
        }
        CtlToMgmt::ToUi => {
            info!("mgmt: ctl -> ui");
            info!("ctl -> ui: {=[u8]:x}", tlv.value.as_slice());
            to_ui.write_all(&tlv.value).await.unwrap();
            to_ui.flush().await.unwrap();
        }
        CtlToMgmt::ToNet => {
            info!("mgmt: ctl -> net");
            to_net.write_all(&tlv.value).await.unwrap();
            to_net.flush().await.unwrap();
            info!("ctl->net: {=[u8]:x}", &tlv.value);
        }
        CtlToMgmt::ResetUiToBootloader => {
            info!("mgmt: resetting UI to bootloader mode");
            ui_reset_pins.reset_to_bootloader(delay).await;
            to_ctl.must_write_tlv(MgmtToCtl::Ack, &[]).await;
        }
        CtlToMgmt::ResetUiToUser => {
            info!("mgmt: resetting UI to user mode");
            ui_reset_pins.reset_to_user(delay).await;
            to_ctl.must_write_tlv(MgmtToCtl::Ack, &[]).await;
        }
        CtlToMgmt::ResetNetToBootloader => {
            info!("mgmt: resetting NET to bootloader mode");
            net_reset_pins.reset_to_bootloader(delay).await;
            to_ctl.must_write_tlv(MgmtToCtl::Ack, &[]).await;
        }
        CtlToMgmt::ResetNetToUser => {
            info!("mgmt: resetting NET to user mode");
            net_reset_pins.reset_to_user(delay).await;
            to_ctl.must_write_tlv(MgmtToCtl::Ack, &[]).await;
        }
        CtlToMgmt::Hello => {
            info!("mgmt: hello handshake");
            // XOR the 4-byte value with b"LINK" and send back
            const MAGIC: &[u8; 4] = b"LINK";
            let mut response = [0u8; 4];
            for (i, byte) in tlv.value.iter().take(4).enumerate() {
                response[i] = byte ^ MAGIC[i];
            }
            to_ctl.must_write_tlv(MgmtToCtl::Hello, &response).await;
        }
        CtlToMgmt::WsEchoTest => {
            info!("mgmt: forwarding ws echo test to net");
            to_net.must_write_tlv(MgmtToNet::WsEchoTest, &[]).await;
        }
        CtlToMgmt::WsSpeedTest => {
            info!("mgmt: forwarding ws speed test to net");
            to_net.must_write_tlv(MgmtToNet::WsSpeedTest, &[]).await;
        }
        CtlToMgmt::HoldUiReset => {
            info!("mgmt: holding UI in reset");
            ui_reset_pins.hold_reset();
            to_ctl.must_write_tlv(MgmtToCtl::Ack, &[]).await;
        }
    }
}

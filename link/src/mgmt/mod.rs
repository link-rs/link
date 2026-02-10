//! MGMT (Management) chip - coordinates communication between all chips.

use crate::info;
use crate::shared::{
    Color, CtlToMgmt, CtlToNet, CtlToUi, Led, MgmtToCtl, ReadTlv, Tlv, Value, WriteTlv,
    uart_config::SetBaudRate,
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

    /// Set the BOOT/GPIO0 pin directly.
    /// - high = normal mode (boot from flash)
    /// - low = bootloader mode
    pub fn set_boot(&mut self, high: bool) {
        if high {
            let _ = self.boot.set_high();
        } else {
            let _ = self.boot.set_low();
        }
    }

    /// Set the RST/EN pin directly.
    /// - high = chip running
    /// - low = chip held in reset
    pub fn set_rst(&mut self, high: bool) {
        if high {
            let _ = self.rst.set_high();
        } else {
            let _ = self.rst.set_low();
        }
    }

    /// Hold NET chip in reset (RST low).
    pub fn hold_reset(&mut self) {
        let _ = self.rst.set_low();
    }
}

/// Type alias for backwards compatibility.
pub type Esp32ResetPins<Boot, Rst> = NetResetPins<Boot, Rst>;

/// Indicates a baud rate change requested by handle_ctl.
/// The caller is responsible for applying changes to RX sides after releasing locks.
enum BaudRateChange {
    None,
    /// Change CTL UART baud rate (TX already changed, caller should change RX)
    Ctl(u32),
    /// Change NET UART baud rate (TX already changed, caller should change RX)
    Net(u32),
}

#[allow(unreachable_code)]
pub async fn run<W, R, RA, GA, BA, RB, GB, BB, UiBoot0, UiBoot1, UiRst, NetBoot, NetRst, D>(
    to_ctl: W,
    mut from_ctl: R,
    mut to_ui: W,
    mut from_ui: R,
    mut to_net: W,
    from_net: R,
    led_a: (RA, GA, BA),
    led_b: (RB, GB, BB),
    mut ui_reset_pins: UiResetPins<UiBoot0, UiBoot1, UiRst>,
    mut net_reset_pins: NetResetPins<NetBoot, NetRst>,
    mut delay: D,
) -> !
where
    W: Write + SetBaudRate,
    R: Read + SetBaudRate,
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

    // Initialize LEDs (off by default)
    let led_a = Led::new(led_a.0, led_a.1, led_a.2);
    let led_b = Led::new(led_b.0, led_b.1, led_b.2);
    let led_a: Mutex<NoopRawMutex, _> = Mutex::new(led_a);
    let led_b: Mutex<NoopRawMutex, _> = Mutex::new(led_b);

    // Set LEDs to off initially
    led_a.lock().await.set(Color::Black);
    led_b.lock().await.set(Color::Black);

    // UI and NET chips are held in reset at boot (RST low).
    // Wait for MGMT clocks to stabilize, then release them to boot.
    delay.delay_ms(50).await;
    info!("mgmt: releasing UI and NET from reset");
    let _ = ui_reset_pins.rst.set_high();
    let _ = net_reset_pins.rst.set_high();

    // Wrap to_ctl in a mutex since it's shared between tasks
    let to_ctl: Mutex<NoopRawMutex, _> = Mutex::new(to_ctl);

    // Track pending NET RX baud rate changes with an atomic
    // (0 = no change pending, non-zero = new baud rate to apply)
    // This avoids deadlocks since net_task can poll this instead of holding a lock
    use core::sync::atomic::{AtomicU32, Ordering};
    let net_rx_pending_baud: AtomicU32 = AtomicU32::new(0);

    // LED A: Blue=ToNet, Red=FromNet
    // LED B: Blue=ToUi, Red=FromUi

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

            // Blink LED B red for FromUi
            led_b.lock().await.set(Color::Red);

            let mut to_ctl = to_ctl.lock().await;
            let _ = to_ctl.write_tlv(MgmtToCtl::FromUi, &buffer).await;

            led_b.lock().await.set(Color::Black);
        }
    };

    let net_task = async {
        let mut buffer = Value::default();
        let mut from_net = from_net;
        loop {
            // Check for pending NET RX baud rate change
            let pending = net_rx_pending_baud.load(Ordering::SeqCst);
            if pending != 0 {
                net_rx_pending_baud.store(0, Ordering::SeqCst);
                from_net.set_baud_rate(pending).await;
            }

            buffer.resize(buffer.capacity(), 0).unwrap();
            let Ok(n) = from_net.read(&mut buffer).await else {
                info!("net->mgmt: error!");
                continue;
            };
            buffer.truncate(n);
            info!("net->ctl: {=[u8]:x}", &buffer);

            // Blink LED A red for FromNet
            led_a.lock().await.set(Color::Red);

            let mut to_ctl = to_ctl.lock().await;
            let _ = to_ctl.write_tlv(MgmtToCtl::FromNet, &buffer).await;

            led_a.lock().await.set(Color::Black);
        }
    };

    let ctl_task = async {
        loop {
            let tlv = match from_ctl.read_tlv().await {
                Ok(Some(tlv)) => tlv,
                _ => continue,
            };

            // Save tlv_type before moving tlv
            let tlv_type = tlv.tlv_type;

            // Blink appropriate LED blue for outgoing data
            match tlv_type {
                CtlToMgmt::ToNet => led_a.lock().await.set(Color::Blue),
                CtlToMgmt::ToUi => led_b.lock().await.set(Color::Blue),
                _ => {}
            }

            // Get mutable access to to_ctl for the handler
            let mut to_ctl_guard = to_ctl.lock().await;

            let baud_change = handle_ctl(
                tlv,
                &mut *to_ctl_guard,
                &mut to_ui,
                &mut to_net,
                &mut ui_reset_pins,
                &mut net_reset_pins,
                &mut delay,
            )
            .await;

            // Apply any baud rate changes after releasing to_ctl lock
            drop(to_ctl_guard);

            match baud_change {
                BaudRateChange::None => {}
                BaudRateChange::Ctl(baud) => {
                    from_ctl.set_baud_rate(baud).await;
                    to_ctl.lock().await.set_baud_rate(baud).await;
                }
                BaudRateChange::Net(baud) => {
                    // Signal net_task to apply this baud rate change
                    // (net_task will check this atomic before each read)
                    net_rx_pending_baud.store(baud, Ordering::SeqCst);
                }
            }

            // Turn off LED after operation
            match tlv_type {
                CtlToMgmt::ToNet => led_a.lock().await.set(Color::Black),
                CtlToMgmt::ToUi => led_b.lock().await.set(Color::Black),
                _ => {}
            }
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
) -> BaudRateChange
where
    C: WriteTlv<MgmtToCtl> + Write + SetBaudRate,
    U: WriteTlv<CtlToUi> + Write,
    N: WriteTlv<CtlToNet> + Write + SetBaudRate,
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
            BaudRateChange::None
        }
        CtlToMgmt::ToUi => {
            info!("mgmt: ctl -> ui");
            info!("ctl -> ui: {=[u8]:x}", tlv.value.as_slice());
            to_ui.write_all(&tlv.value).await.unwrap();
            to_ui.flush().await.unwrap();
            BaudRateChange::None
        }
        CtlToMgmt::ToNet => {
            info!("mgmt: ctl -> net");
            to_net.write_all(&tlv.value).await.unwrap();
            to_net.flush().await.unwrap();
            info!("ctl->net: {=[u8]:x}", &tlv.value);
            BaudRateChange::None
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
            BaudRateChange::None
        }
        CtlToMgmt::SetPin => {
            use crate::shared::{Pin, PinValue};
            use num_enum::TryFromPrimitive;

            // Parse pin ID (byte 0) and value (byte 1)
            let pin_id = tlv.value.first().copied().unwrap_or(0);
            let value_id = tlv.value.get(1).copied().unwrap_or(0);

            if let (Ok(pin), Ok(value)) = (
                Pin::try_from_primitive(pin_id),
                PinValue::try_from_primitive(value_id),
            ) {
                let high = value == PinValue::High;
                match pin {
                    Pin::UiBoot0 => {
                        info!("mgmt: set UI BOOT0 pin = {:?}", value);
                        if high {
                            let _ = ui_reset_pins.boot0.set_high();
                        } else {
                            let _ = ui_reset_pins.boot0.set_low();
                        }
                    }
                    Pin::UiBoot1 => {
                        info!("mgmt: set UI BOOT1 pin = {:?}", value);
                        if high {
                            let _ = ui_reset_pins.boot1.set_high();
                        } else {
                            let _ = ui_reset_pins.boot1.set_low();
                        }
                    }
                    Pin::UiRst => {
                        info!("mgmt: set UI RST pin = {:?}", value);
                        if high {
                            let _ = ui_reset_pins.rst.set_high();
                        } else {
                            let _ = ui_reset_pins.rst.set_low();
                        }
                    }
                    Pin::NetBoot => {
                        info!("mgmt: set NET BOOT pin = {:?}", value);
                        net_reset_pins.set_boot(high);
                    }
                    Pin::NetRst => {
                        info!("mgmt: set NET RST pin = {:?}", value);
                        net_reset_pins.set_rst(high);
                    }
                }
            }
            to_ctl.must_write_tlv(MgmtToCtl::Ack, &[]).await;
            BaudRateChange::None
        }
        CtlToMgmt::SetNetBaudRate => {
            // Parse 4-byte BE u32 baud rate
            let baud_rate = u32::from_be_bytes([
                tlv.value.get(0).copied().unwrap_or(0),
                tlv.value.get(1).copied().unwrap_or(0),
                tlv.value.get(2).copied().unwrap_or(0),
                tlv.value.get(3).copied().unwrap_or(0),
            ]);
            info!("mgmt: setting NET baud rate to {}", baud_rate);
            // Flush pending NET TX data at old rate
            let _ = to_net.flush().await;
            // Change NET TX baud rate; caller will update RX after releasing locks
            to_net.set_baud_rate(baud_rate).await;
            // ACK goes to CTL at unchanged rate
            to_ctl.must_write_tlv(MgmtToCtl::Ack, &[]).await;
            BaudRateChange::Net(baud_rate)
        }
        CtlToMgmt::SetCtlBaudRate => {
            // Parse 4-byte BE u32 baud rate
            let baud_rate = u32::from_be_bytes([
                tlv.value.get(0).copied().unwrap_or(0),
                tlv.value.get(1).copied().unwrap_or(0),
                tlv.value.get(2).copied().unwrap_or(0),
                tlv.value.get(3).copied().unwrap_or(0),
            ]);
            info!("mgmt: setting CTL baud rate to {}", baud_rate);
            // CRITICAL: Send ACK FIRST at old baud rate
            to_ctl.must_write_tlv(MgmtToCtl::Ack, &[]).await;
            // Flush to ensure ACK is transmitted before rate change
            let _ = to_ctl.flush().await;
            // Small delay to ensure ACK bytes are fully transmitted on wire
            delay.delay_ms(5).await;
            // Change CTL TX baud rate; caller will update RX after returning
            to_ctl.set_baud_rate(baud_rate).await;
            BaudRateChange::Ctl(baud_rate)
        }
        CtlToMgmt::GetStackInfo => {
            info!("mgmt: get stack info");
            use cortex_m_stack::{stack, stack_size, stack_painted};
            use crate::shared::StackInfo;
            let range = stack();
            let base = range.end as u32;
            let top = range.start as u32;
            let size = stack_size() as u32;
            let used = size.saturating_sub(stack_painted() as u32);
            let info = StackInfo {
                stack_base: base,
                stack_top: top,
                stack_size: size,
                stack_used: used,
            };
            let mut buf = [0u8; 32];
            if let Some(serialized) = info.to_bytes(&mut buf) {
                to_ctl.must_write_tlv(MgmtToCtl::StackInfo, serialized).await;
            }
            BaudRateChange::None
        }
        CtlToMgmt::RepaintStack => {
            info!("mgmt: repaint stack");
            cortex_m_stack::repaint_stack();
            to_ctl.must_write_tlv(MgmtToCtl::Ack, &[]).await;
            BaudRateChange::None
        }
    }
}

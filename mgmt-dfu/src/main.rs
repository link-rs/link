#![no_std]
#![no_main]

//! `mgmt-dfu`: soft-reset-into-ST-bootloader for over-USB firmware
//! updates without a custom bootloader.
//!
//! Three things in one binary:
//!
//! 1. **Blue LED heartbeat** at 1 Hz on EV17 LED-A blue (PB1).
//! 2. **USB vendor-class echo**: enumerates with custom VID:PID and a
//!    single bulk-IN / bulk-OUT pair; anything the host writes to the OUT
//!    endpoint is mirrored back on the IN endpoint.
//! 3. **DFU detach handler**: composes a DFU runtime descriptor on the
//!    same USB device. When the host issues `DFU_DETACH`, we write a
//!    magic value into a `.uninit` RAM slot and trigger `SCB::sys_reset`.
//!    A `#[cortex_m_rt::pre_init]` hook checks the magic on the next
//!    boot, remaps system memory to `0x0000_0000` via `SYSCFG.MEM_MODE`,
//!    and jumps into ST's ROM DFU bootloader at `0x1FFF_C800`. The host
//!    then sees the standard `0483:df11` DFU device and can flash new
//!    firmware.
//!
//! The point: no custom bootloader. ROM DFU does the flashing; this
//! firmware just provides the soft-handoff.

use core::mem::MaybeUninit;

use cortex_m_rt::pre_init;
use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_stm32::gpio::{Level, Output, Speed};
use embassy_stm32::time::Hertz;
use embassy_stm32::usb::Driver;
use embassy_stm32::{bind_interrupts, peripherals, usb};
use embassy_time::Timer;
use embassy_usb::driver::{Endpoint, EndpointIn, EndpointOut};
use embassy_usb::{Builder, Config};
use embassy_usb_dfu::application::{usb_dfu, DfuAttributes, DfuState, Handler};
use static_cell::StaticCell;

use panic_reset as _;

/// Rendezvous magic checked by `pre_init` immediately after `SCB::sys_reset`.
/// Lives in `.uninit` so cortex-m-rt doesn't zero it on startup. RAM
/// survives a soft reset on STM32, so the value placed before the reset
/// is still readable on the next boot.
#[unsafe(link_section = ".uninit.BOOTLOADER_MAGIC")]
static mut BOOTLOADER_MAGIC: MaybeUninit<u32> = MaybeUninit::uninit();
const MAGIC_VALUE: u32 = 0xDEAD_B007;

/// ST F072 system memory address (where the ROM DFU bootloader lives).
/// See AN2606 — STM32F07x system memory boot mode.
const ST_BOOTLOADER_ADDR: u32 = 0x1FFF_C800;

/// Early hook: if the magic was set before the most recent `sys_reset`,
/// remap system memory to address 0 and jump into the ROM bootloader.
/// Runs after the stack is set up but before `.bss`/`.data` init and
/// before any application code or embassy init — chip is in a clean
/// post-reset state aside from preserved RAM.
#[pre_init]
unsafe fn check_bootloader_magic() {
    let magic_ptr = core::ptr::addr_of_mut!(BOOTLOADER_MAGIC) as *mut u32;
    if core::ptr::read_volatile(magic_ptr) != MAGIC_VALUE {
        return;
    }
    // One-shot: clear so we don't loop into the bootloader forever.
    core::ptr::write_volatile(magic_ptr, 0);

    // SYSCFG clock enable (APB2ENR bit 0) so the MEM_MODE write sticks.
    const RCC_APB2ENR: *mut u32 = 0x4002_1018 as *mut u32;
    core::ptr::write_volatile(RCC_APB2ENR, core::ptr::read_volatile(RCC_APB2ENR) | 1);

    // SYSCFG_CFGR1.MEM_MODE = 0b01 → System Flash mapped at 0x0000_0000.
    // This is where the ROM bootloader's vector table is, so a fetch
    // through 0 hits it.
    const SYSCFG_CFGR1: *mut u32 = 0x4001_0000 as *mut u32;
    let v = core::ptr::read_volatile(SYSCFG_CFGR1);
    core::ptr::write_volatile(SYSCFG_CFGR1, (v & !0b11) | 0b01);

    cortex_m::asm::dsb();
    cortex_m::asm::isb();

    cortex_m::asm::bootload(ST_BOOTLOADER_ADDR as *const u32);
}

/// Public soft-DFU trigger. Sets the magic, then `sys_reset`s.
fn request_bootloader_reset() -> ! {
    unsafe {
        core::ptr::write_volatile(
            core::ptr::addr_of_mut!(BOOTLOADER_MAGIC) as *mut u32,
            MAGIC_VALUE,
        );
    }
    cortex_m::asm::dsb();
    cortex_m::peripheral::SCB::sys_reset();
}

bind_interrupts!(struct Irqs {
    USB => usb::InterruptHandler<peripherals::USB>;
});

const VID: u16 = 0xc0de;
const PID: u16 = 0xcafb;
const USB_MAX_PACKET: u16 = 64;

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let rcc_config = {
        use embassy_stm32::rcc::*;
        let mut config = embassy_stm32::Config::default();
        config.rcc.hsi = true;
        config.rcc.hse = Some(Hse {
            freq: Hertz(16_000_000),
            mode: HseMode::Oscillator,
        });
        config.rcc.pll = Some(Pll {
            src: PllSource::HSE,
            prediv: PllPreDiv::DIV1,
            mul: PllMul::MUL3,
        });
        config.rcc.sys = Sysclk::PLL1_P;
        config.rcc.apb1_pre = APBPrescaler::DIV1;
        config.rcc.ls = LsConfig::default_lsi();
        config
    };
    let p = embassy_stm32::init(rcc_config);

    // EV17 LED A: own all three colors inside the blink task so the
    // Output handles aren't dropped (which would revert pins to Analog
    // mode and let them float). Polarity convention chosen empirically:
    // each pin starts at the level we believe extinguishes its LED.
    // Adjust `LED_ON_R` / `LED_ON_G` / `LED_ON_B` in the task below if
    // the visual result doesn't match.
    // Empirical EV17 LED A polarities:
    //   R (PB5): active-high → drive LOW to extinguish.
    //   G (PB4): active-low  → drive HIGH to extinguish.
    //   B (PB1): active-high → drive HIGH to light, LOW to extinguish.
    spawner.must_spawn(blink(
        Output::new(p.PB5, Level::Low, Speed::Low),
        Output::new(p.PB4, Level::High, Speed::Low),
        Output::new(p.PB1, Level::Low, Speed::Low),
    ));

    // USB peripheral on PA12 (D+) / PA11 (D-).
    let driver = Driver::new(p.USB, Irqs, p.PA12, p.PA11);

    let mut config = Config::new(VID, PID);
    config.manufacturer = Some("link-rs");
    config.product = Some("Link");
    config.serial_number = Some("Link EV17 XXXXXXXX");
    config.max_power = 100;
    config.max_packet_size_0 = 64;
    config.composite_with_iads = true;

    static CONFIG_DESC: StaticCell<[u8; 256]> = StaticCell::new();
    static BOS_DESC: StaticCell<[u8; 64]> = StaticCell::new();
    static MSOS_DESC: StaticCell<[u8; 0]> = StaticCell::new();
    static CONTROL_BUF: StaticCell<[u8; 64]> = StaticCell::new();
    static DFU_STATE: StaticCell<DfuState<DetachHandler>> = StaticCell::new();

    let mut builder = Builder::new(
        driver,
        config,
        CONFIG_DESC.init([0u8; 256]),
        BOS_DESC.init([0u8; 64]),
        MSOS_DESC.init([0u8; 0]),
        CONTROL_BUF.init([0u8; 64]),
    );

    // Vendor-class function: one bulk IN + one bulk OUT for echo.
    let (mut ep_in, mut ep_out) = {
        let mut func = builder.function(0xff, 0xff, 0xff);
        let mut iface = func.interface();
        let mut alt = iface.alt_setting(0xff, 0xff, 0xff, None);
        let ep_in = alt.endpoint_bulk_in(None, USB_MAX_PACKET);
        let ep_out = alt.endpoint_bulk_out(None, USB_MAX_PACKET);
        (ep_in, ep_out)
    };

    // DFU runtime function on the same device. `DFU_DETACH` from the
    // host triggers `DetachHandler::enter_dfu`, which sets the magic and
    // resets — landing us in the ROM bootloader.
    let dfu_state = DFU_STATE.init(DfuState::new(
        DetachHandler,
        DfuAttributes::CAN_DOWNLOAD | DfuAttributes::WILL_DETACH,
        embassy_time::Duration::from_millis(2500),
    ));
    usb_dfu(&mut builder, dfu_state, |_| {});

    let mut usb_dev = builder.build();
    let usb_fut = usb_dev.run();

    // Echo loop: read a packet from OUT, write it back on IN.
    let mut buf = [0u8; USB_MAX_PACKET as usize];
    let echo_fut = async {
        loop {
            ep_out.wait_enabled().await;
            ep_in.wait_enabled().await;
            loop {
                let n = match ep_out.read(&mut buf).await {
                    Ok(n) => n,
                    Err(_) => break,
                };
                if ep_in.write(&buf[..n]).await.is_err() {
                    break;
                }
            }
        }
    };

    join(usb_fut, echo_fut).await;
}

#[embassy_executor::task]
async fn blink(mut r: Output<'static>, mut g: Output<'static>, mut b: Output<'static>) {
    // Park R extinguished (active-high, drive low) and G extinguished
    // (active-low, drive high). Only B toggles.
    r.set_low();
    g.set_high();
    loop {
        b.set_high();
        Timer::after_millis(500).await;
        b.set_low();
        Timer::after_millis(500).await;
    }
}

/// `embassy_usb_dfu::application::Handler` impl: when the host issues
/// `DFU_DETACH` on the runtime DFU descriptor, embassy invokes
/// `enter_dfu` from the USB control transfer. We don't try to be polite
/// about completing the response — `WILL_DETACH` means the host expects
/// the device to disappear after the DETACH request, and our soft-reset
/// path does exactly that.
struct DetachHandler;
impl Handler for DetachHandler {
    fn enter_dfu(&mut self) {
        request_bootloader_reset();
    }
}

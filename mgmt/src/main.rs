#![no_std]
#![no_main]

use cortex_m::singleton;
use embassy_executor::Spawner;
use embassy_stm32::{
    bind_interrupts,
    gpio::{Input, Level, Output, Pull, Speed},
    mode::Async,
    peripherals,
    rcc::{Mco, McoPrescaler, McoSource},
    time::Hertz,
    usart,
    usart::{Config, DataBits, Parity, RingBufferedUartRx, StopBits, Uart, UartTx},
};
use embassy_time::Delay;
use embedded_io_async::{ErrorType, Read, Write};
use link::uart_config::SetBaudRate;
use {defmt_rtt as _, panic_probe as _};

#[derive(Copy, Clone)]
struct NoopOutputPin;

impl embedded_hal::digital::ErrorType for NoopOutputPin {
    type Error = core::convert::Infallible;
}

impl embedded_hal::digital::OutputPin for NoopOutputPin {
    fn set_low(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }

    fn set_high(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl embedded_hal::digital::StatefulOutputPin for NoopOutputPin {
    fn is_set_high(&mut self) -> Result<bool, Self::Error> {
        Ok(false)
    }

    fn is_set_low(&mut self) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

/// Stack monitor implementation using cortex-m-stack.
struct CortexMStackMonitor;

impl link::StackMonitor for CortexMStackMonitor {
    fn stack(&self) -> core::ops::Range<*mut u32> {
        cortex_m_stack::stack()
    }

    fn stack_size(&self) -> u32 {
        cortex_m_stack::stack_size()
    }

    fn stack_painted(&self) -> u32 {
        cortex_m_stack::stack_painted()
    }

    fn repaint_stack(&self) {
        cortex_m_stack::repaint_stack();
    }
}

const DMA_BUF_SIZE: usize = link::MAX_VALUE_SIZE;

/// Convert centralized UART config to STM32 HAL config.
fn uart_config_to_stm32(cfg: link::uart_config::Config) -> Config {
    use link::uart_config::{Parity as P, StopBits as S};
    let mut config = Config::default();
    config.baudrate = cfg.baudrate;
    config.data_bits = DataBits::DataBits8;
    config.parity = match cfg.parity {
        P::None => Parity::ParityNone,
        P::Even => Parity::ParityEven,
    };
    config.stop_bits = match cfg.stop_bits {
        S::One => StopBits::STOP1,
        S::Two => StopBits::STOP2,
    };
    config
}

/// Wrapper around UartTx that implements SetBaudRate.
struct UartTxWrapper<'d>(UartTx<'d, Async>);

impl<'d> ErrorType for UartTxWrapper<'d> {
    type Error = usart::Error;
}

impl<'d> Write for UartTxWrapper<'d> {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        self.0.write(buf).await?;
        Ok(buf.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.0.flush().await
    }
}

impl<'d> SetBaudRate for UartTxWrapper<'d> {
    async fn set_baud_rate(&mut self, baud_rate: u32) {
        let _ = self.0.set_baudrate(baud_rate);
    }
}

/// Wrapper around RingBufferedUartRx that implements SetBaudRate.
struct UartRxWrapper<'d>(RingBufferedUartRx<'d>);

impl<'d> ErrorType for UartRxWrapper<'d> {
    type Error = usart::Error;
}

impl<'d> Read for UartRxWrapper<'d> {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        self.0.read(buf).await
    }
}

impl<'d> SetBaudRate for UartRxWrapper<'d> {
    async fn set_baud_rate(&mut self, baud_rate: u32) {
        let _ = self.0.set_baudrate(baud_rate);
    }
}

bind_interrupts!(struct Irqs {
    USART1 => usart::InterruptHandler<peripherals::USART1>;
    USART2 => usart::InterruptHandler<peripherals::USART2>;
    USART3_4 => usart::InterruptHandler<peripherals::USART3>;
});

struct OptionByteHardwareVersionStore;

impl OptionByteHardwareVersionStore {
    const FLASH_KEYR: *mut u32 = 0x4002_2004 as *mut u32;
    const FLASH_OPTKEYR: *mut u32 = 0x4002_2008 as *mut u32;
    const FLASH_SR: *mut u32 = 0x4002_200C as *mut u32;
    const FLASH_CR: *mut u32 = 0x4002_2010 as *mut u32;
    const OB_DATA0_ADDR: *mut u16 = 0x1FFF_F804 as *mut u16;

    const FLASH_KEY1: u32 = 0x4567_0123;
    const FLASH_KEY2: u32 = 0xCDEF_89AB;
    const FLASH_OPTKEY1: u32 = 0x0819_2A3B;
    const FLASH_OPTKEY2: u32 = 0x4C5D_6E7F;

    const SR_BSY: u32 = 1 << 0;
    const CR_OPTPG: u32 = 1 << 4;
    const CR_OPTER: u32 = 1 << 5;
    const CR_STRT: u32 = 1 << 6;
    const CR_LOCK: u32 = 1 << 7;
    const CR_OPTWRE: u32 = 1 << 9;
    const CR_OBL_LAUNCH: u32 = 1 << 13;

    fn read_data0_byte() -> u8 {
        let raw = unsafe { core::ptr::read_volatile(Self::OB_DATA0_ADDR) };
        (raw & 0x00FF) as u8
    }

    fn wait_not_busy() {
        while unsafe { core::ptr::read_volatile(Self::FLASH_SR) } & Self::SR_BSY != 0 {}
    }

    fn unlock_flash_if_needed() {
        let cr = unsafe { core::ptr::read_volatile(Self::FLASH_CR) };
        if cr & Self::CR_LOCK != 0 {
            unsafe {
                core::ptr::write_volatile(Self::FLASH_KEYR, Self::FLASH_KEY1);
                core::ptr::write_volatile(Self::FLASH_KEYR, Self::FLASH_KEY2);
            }
        }
    }

    fn unlock_option_bytes_if_needed() {
        let cr = unsafe { core::ptr::read_volatile(Self::FLASH_CR) };
        if cr & Self::CR_OPTWRE == 0 {
            unsafe {
                core::ptr::write_volatile(Self::FLASH_OPTKEYR, Self::FLASH_OPTKEY1);
                core::ptr::write_volatile(Self::FLASH_OPTKEYR, Self::FLASH_OPTKEY2);
            }
        }
    }

    fn program_data0_byte(data0: u8) {
        cortex_m::interrupt::free(|_| {
            Self::wait_not_busy();
            Self::unlock_flash_if_needed();
            Self::unlock_option_bytes_if_needed();

            // Erase option bytes.
            unsafe {
                let mut cr = core::ptr::read_volatile(Self::FLASH_CR);
                cr |= Self::CR_OPTER;
                core::ptr::write_volatile(Self::FLASH_CR, cr);
                core::ptr::write_volatile(Self::FLASH_CR, cr | Self::CR_STRT);
            }
            Self::wait_not_busy();
            unsafe {
                let mut cr = core::ptr::read_volatile(Self::FLASH_CR);
                cr &= !Self::CR_OPTER;
                core::ptr::write_volatile(Self::FLASH_CR, cr);
            }

            // Program DATA0 (low byte) with inverted high byte.
            let halfword = ((!(data0) as u16) << 8) | data0 as u16;
            unsafe {
                let mut cr = core::ptr::read_volatile(Self::FLASH_CR);
                cr |= Self::CR_OPTPG;
                core::ptr::write_volatile(Self::FLASH_CR, cr);
                core::ptr::write_volatile(Self::OB_DATA0_ADDR, halfword);
            }
            Self::wait_not_busy();
            unsafe {
                let mut cr = core::ptr::read_volatile(Self::FLASH_CR);
                cr &= !Self::CR_OPTPG;
                core::ptr::write_volatile(Self::FLASH_CR, cr);
            }

            // Reload option bytes (causes reset).
            unsafe {
                let cr = core::ptr::read_volatile(Self::FLASH_CR) | Self::CR_OBL_LAUNCH;
                core::ptr::write_volatile(Self::FLASH_CR, cr);
            }
        });
    }
}

impl link::mgmt::HardwareVersionStore for OptionByteHardwareVersionStore {
    fn get_hardware_version(&self) -> u16 {
        Self::read_data0_byte() as u16
    }

    fn set_hardware_version(&mut self, version: u16) {
        Self::program_data0_byte((version & 0x00FF) as u8);
    }
}

async fn ev16_pin_assignments_init(
    p: embassy_stm32::Peripherals,
    hardware_version_store: OptionByteHardwareVersionStore,
) -> ! {
    // MCO on PA8: Output 6 MHz clock for UI chip
    let _mco = Mco::new(p.MCO, p.PA8, McoSource::PLL, McoPrescaler::DIV4);

    let ctl_config = uart_config_to_stm32(link::uart_config::CTL_MGMT);
    let ui_config = uart_config_to_stm32(link::uart_config::MGMT_UI);
    let net_config = uart_config_to_stm32(link::uart_config::MGMT_NET);

    let ctl_rx_buf = singleton!(: [u8; DMA_BUF_SIZE] = [0; DMA_BUF_SIZE]).unwrap();
    let ui_rx_buf = singleton!(: [u8; DMA_BUF_SIZE] = [0; DMA_BUF_SIZE]).unwrap();
    let net_rx_buf = singleton!(: [u8; DMA_BUF_SIZE] = [0; DMA_BUF_SIZE]).unwrap();

    let (to_ctl, from_ctl) = Uart::new(
        p.USART1, p.PA10, p.PA9, Irqs, p.DMA1_CH2, p.DMA1_CH3, ctl_config,
    )
    .unwrap()
    .split();
    let to_ctl = UartTxWrapper(to_ctl);
    let from_ctl = UartRxWrapper(from_ctl.into_ring_buffered(ctl_rx_buf));

    let (to_ui, from_ui) = Uart::new(
        p.USART2, p.PA3, p.PA2, Irqs, p.DMA1_CH4, p.DMA1_CH5, ui_config,
    )
    .unwrap()
    .split();
    let to_ui = UartTxWrapper(to_ui);
    let from_ui = UartRxWrapper(from_ui.into_ring_buffered(ui_rx_buf));

    let (to_net, from_net) = Uart::new(
        p.USART3, p.PB11, p.PB10, Irqs, p.DMA1_CH7, p.DMA1_CH6, net_config,
    )
    .unwrap()
    .split();
    let to_net = UartTxWrapper(to_net);
    let from_net = UartRxWrapper(from_net.into_ring_buffered(net_rx_buf));

    // EV16 LED A: R=PA4 (inverted), G=PA6, B=PA7
    let led_a = (
        link::InvertedPin(Output::new(p.PA4, Level::Low, Speed::Low)),
        Output::new(p.PA6, Level::Low, Speed::Low),
        Output::new(p.PA7, Level::Low, Speed::Low),
    );

    // EV16 LED B: R=PB0, G=PB6, B=PB15
    let led_b = (
        Output::new(p.PB0, Level::Low, Speed::Low),
        Output::new(p.PB6, Level::Low, Speed::Low),
        Output::new(p.PB15, Level::Low, Speed::Low),
    );

    let ui_boot0 = Output::new(p.PA15, Level::Low, Speed::Low);
    let ui_boot1 = Output::new(p.PB8, Level::High, Speed::Low);
    let ui_rst = Output::new(p.PB3, Level::Low, Speed::Low);
    let ui_reset_pins = link::mgmt::UiResetPins::new(ui_boot0, ui_boot1, ui_rst);

    let net_boot = Output::new(p.PB5, Level::High, Speed::Low);
    let net_rst = Output::new(p.PB4, Level::Low, Speed::Low);
    let net_reset_pins = link::mgmt::NetResetPins::new(net_boot, net_rst);

    link::mgmt::run(
        to_ctl,
        from_ctl,
        to_ui,
        from_ui,
        to_net,
        from_net,
        led_a,
        led_b,
        ui_reset_pins,
        net_reset_pins,
        Delay,
        CortexMStackMonitor,
        hardware_version_store,
    )
    .await;
}

async fn ev17_pin_assignments_init(
    p: embassy_stm32::Peripherals,
    hardware_version_store: OptionByteHardwareVersionStore,
) -> ! {
    // MCO on PA8: Output 6 MHz clock for UI chip
    let _mco = Mco::new(p.MCO, p.PA8, McoSource::PLL, McoPrescaler::DIV4);

    let ctl_config = uart_config_to_stm32(link::uart_config::CTL_MGMT);
    let ui_config = uart_config_to_stm32(link::uart_config::MGMT_UI);
    let net_config = uart_config_to_stm32(link::uart_config::MGMT_NET);

    let ctl_rx_buf = singleton!(: [u8; DMA_BUF_SIZE] = [0; DMA_BUF_SIZE]).unwrap();
    let ui_rx_buf = singleton!(: [u8; DMA_BUF_SIZE] = [0; DMA_BUF_SIZE]).unwrap();
    let net_rx_buf = singleton!(: [u8; DMA_BUF_SIZE] = [0; DMA_BUF_SIZE]).unwrap();

    let (to_ctl, from_ctl) = Uart::new(
        p.USART1, p.PA10, p.PA9, Irqs, p.DMA1_CH2, p.DMA1_CH3, ctl_config,
    )
    .unwrap()
    .split();
    let to_ctl = UartTxWrapper(to_ctl);
    let from_ctl = UartRxWrapper(from_ctl.into_ring_buffered(ctl_rx_buf));

    let (to_ui, from_ui) = Uart::new(
        p.USART2, p.PA3, p.PA2, Irqs, p.DMA1_CH4, p.DMA1_CH5, ui_config,
    )
    .unwrap()
    .split();
    let to_ui = UartTxWrapper(to_ui);
    let from_ui = UartRxWrapper(from_ui.into_ring_buffered(ui_rx_buf));

    let (to_net, from_net) = Uart::new(
        p.USART3, p.PB11, p.PB10, Irqs, p.DMA1_CH7, p.DMA1_CH6, net_config,
    )
    .unwrap()
    .split();
    let to_net = UartTxWrapper(to_net);
    let from_net = UartRxWrapper(from_net.into_ring_buffered(net_rx_buf));

    // EV17 LED A: R=PB5, G=PB4, B=PB1
    let led_a = (
        Output::new(p.PB5, Level::Low, Speed::Low),
        Output::new(p.PB4, Level::Low, Speed::Low),
        Output::new(p.PB1, Level::Low, Speed::Low),
    );

    // EV17 board updates:
    // - PB0 is BATTERY_MON (ADC input)
    // - PC14 is MGMT_DEBUG1 (output)
    // - PB15 is NC (floating)
    let _battery_mon = Input::new(p.PB0, Pull::None);
    let _mgmt_debug1 = Output::new(p.PC14, Level::Low, Speed::Low);
    let _pb15_nc = Input::new(p.PB15, Pull::None);

    // LED B no longer exists on EV17; provide inert pins for status API compatibility.
    let led_b = (NoopOutputPin, NoopOutputPin, NoopOutputPin);

    let ui_boot0 = Output::new(p.PA15, Level::Low, Speed::Low);
    let ui_boot1 = Output::new(p.PB8, Level::High, Speed::Low);
    let ui_rst = Output::new(p.PB3, Level::Low, Speed::Low);
    let ui_reset_pins = link::mgmt::UiResetPins::new(ui_boot0, ui_boot1, ui_rst);

    let net_boot = Output::new(p.PB6, Level::High, Speed::Low);
    let net_rst = Output::new(p.PC13, Level::Low, Speed::Low);
    let net_reset_pins = link::mgmt::NetResetPins::new(net_boot, net_rst);

    link::mgmt::run(
        to_ctl,
        from_ctl,
        to_ui,
        from_ui,
        to_net,
        from_net,
        led_a,
        led_b,
        ui_reset_pins,
        net_reset_pins,
        Delay,
        CortexMStackMonitor,
        hardware_version_store,
    )
    .await;
}

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    // Clock configuration matching the C firmware:
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

        // XXX(RLB) This configuration is specified in the clock tree and the C code, but crashes
        // the Rust code.  When this line is enabled with LsConfig::default_lsi(), there is a crash
        // inside of Embassy trying to read LSI config.  When this line is enabled with
        // LsConfig::off(), there is a crash in defmt.
        // config.rcc.ahb_pre = AHBPrescaler::DIV2;

        config.rcc.apb1_pre = APBPrescaler::DIV1;
        config.rcc.ls = LsConfig::default_lsi();

        config
    };
    let hardware_version_store = OptionByteHardwareVersionStore;
    let hardware_version =
        <OptionByteHardwareVersionStore as link::mgmt::HardwareVersionStore>::get_hardware_version(
            &hardware_version_store,
        );

    let p = embassy_stm32::init(rcc_config);

    if hardware_version == 16 {
        ev16_pin_assignments_init(p, hardware_version_store).await;
    } else {
        ev17_pin_assignments_init(p, hardware_version_store).await;
    }
}

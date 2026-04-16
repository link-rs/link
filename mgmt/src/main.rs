#![no_std]
#![no_main]

use cortex_m::singleton;
use embassy_executor::Spawner;
use embassy_stm32::{
    bind_interrupts,
    gpio::{Level, Output, Speed},
    mode::Async,
    peripherals,
    rcc::{Mco, McoPrescaler, McoSource},
    time::Hertz,
    usart,
    usart::{Config, DataBits, Parity, RingBufferedUartRx, StopBits, Uart, UartTx},
};
use embassy_time::Delay;
use embedded_io_async::{ErrorType, Read, Write};
use link::{mgmt::Board, uart_config::SetBaudRate, StackMonitor};
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

/// Board implementation for MGMT STM32.
struct Stm32Board;

impl StackMonitor for Stm32Board {
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

impl Board for Stm32Board {
    fn board_version(&self) -> u8 {
        embassy_stm32::pac::FLASH.obr().read().data0()
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

/// Common peripheral setup: MCO clock output, UARTs, and UI reset pins.
fn setup_common(
    mco: embassy_stm32::Peri<'static, peripherals::MCO>,
    pa8: embassy_stm32::Peri<'static, peripherals::PA8>,
    usart1: embassy_stm32::Peri<'static, peripherals::USART1>,
    usart2: embassy_stm32::Peri<'static, peripherals::USART2>,
    usart3: embassy_stm32::Peri<'static, peripherals::USART3>,
    pa9: embassy_stm32::Peri<'static, peripherals::PA9>,
    pa10: embassy_stm32::Peri<'static, peripherals::PA10>,
    pa2: embassy_stm32::Peri<'static, peripherals::PA2>,
    pa3: embassy_stm32::Peri<'static, peripherals::PA3>,
    pb10: embassy_stm32::Peri<'static, peripherals::PB10>,
    pb11: embassy_stm32::Peri<'static, peripherals::PB11>,
    dma1_ch2: embassy_stm32::Peri<'static, peripherals::DMA1_CH2>,
    dma1_ch3: embassy_stm32::Peri<'static, peripherals::DMA1_CH3>,
    dma1_ch4: embassy_stm32::Peri<'static, peripherals::DMA1_CH4>,
    dma1_ch5: embassy_stm32::Peri<'static, peripherals::DMA1_CH5>,
    dma1_ch6: embassy_stm32::Peri<'static, peripherals::DMA1_CH6>,
    dma1_ch7: embassy_stm32::Peri<'static, peripherals::DMA1_CH7>,
    pa15: embassy_stm32::Peri<'static, peripherals::PA15>,
    pb3: embassy_stm32::Peri<'static, peripherals::PB3>,
    pb8: embassy_stm32::Peri<'static, peripherals::PB8>,
) -> (
    Mco<'static, peripherals::MCO>,
    UartTxWrapper<'static>,
    UartRxWrapper<'static>,
    UartTxWrapper<'static>,
    UartRxWrapper<'static>,
    UartTxWrapper<'static>,
    UartRxWrapper<'static>,
    link::mgmt::UiResetPins<Output<'static>, Output<'static>, Output<'static>>,
) {
    // MCO on PA8: Output 6 MHz clock for UI chip
    let mco = Mco::new(mco, pa8, McoSource::PLL, McoPrescaler::DIV4);

    let ctl_config = uart_config_to_stm32(link::uart_config::CTL_MGMT);
    let ui_config = uart_config_to_stm32(link::uart_config::MGMT_UI);
    let net_config = uart_config_to_stm32(link::uart_config::MGMT_NET);

    let ctl_rx_buf = singleton!(: [u8; DMA_BUF_SIZE] = [0; DMA_BUF_SIZE]).unwrap();
    let ui_rx_buf = singleton!(: [u8; DMA_BUF_SIZE] = [0; DMA_BUF_SIZE]).unwrap();
    let net_rx_buf = singleton!(: [u8; DMA_BUF_SIZE] = [0; DMA_BUF_SIZE]).unwrap();

    let (to_ctl, from_ctl) = Uart::new(usart1, pa10, pa9, Irqs, dma1_ch2, dma1_ch3, ctl_config)
        .unwrap()
        .split();
    let to_ctl = UartTxWrapper(to_ctl);
    let from_ctl = UartRxWrapper(from_ctl.into_ring_buffered(ctl_rx_buf));

    let (to_ui, from_ui) = Uart::new(usart2, pa3, pa2, Irqs, dma1_ch4, dma1_ch5, ui_config)
        .unwrap()
        .split();
    let to_ui = UartTxWrapper(to_ui);
    let from_ui = UartRxWrapper(from_ui.into_ring_buffered(ui_rx_buf));

    let (to_net, from_net) = Uart::new(usart3, pb11, pb10, Irqs, dma1_ch7, dma1_ch6, net_config)
        .unwrap()
        .split();
    let to_net = UartTxWrapper(to_net);
    let from_net = UartRxWrapper(from_net.into_ring_buffered(net_rx_buf));

    // UI reset pins (directly under MGMT control)
    let ui_boot0 = Output::new(pa15, Level::Low, Speed::Low);
    let ui_boot1 = Output::new(pb8, Level::High, Speed::Low);
    let ui_rst = Output::new(pb3, Level::Low, Speed::Low);
    let ui_reset_pins = link::mgmt::UiResetPins::new(ui_boot0, ui_boot1, ui_rst);

    (
        mco,
        to_ctl,
        from_ctl,
        to_ui,
        from_ui,
        to_net,
        from_net,
        ui_reset_pins,
    )
}

#[allow(dead_code)]
async fn run_ev16(p: embassy_stm32::Peripherals) -> ! {
    let (_mco, to_ctl, from_ctl, to_ui, from_ui, to_net, from_net, ui_reset_pins) = setup_common(
        p.MCO, p.PA8, p.USART1, p.USART2, p.USART3, p.PA9, p.PA10, p.PA2, p.PA3, p.PB10, p.PB11,
        p.DMA1_CH2, p.DMA1_CH3, p.DMA1_CH4, p.DMA1_CH5, p.DMA1_CH6, p.DMA1_CH7, p.PA15, p.PB3,
        p.PB8,
    );

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

    // EV16 NET reset: BOOT=PB5, RST=PB4
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
        Stm32Board,
    )
    .await;
}

async fn run_ev17(p: embassy_stm32::Peripherals) -> ! {
    let (_mco, to_ctl, from_ctl, to_ui, from_ui, to_net, from_net, ui_reset_pins) = setup_common(
        p.MCO, p.PA8, p.USART1, p.USART2, p.USART3, p.PA9, p.PA10, p.PA2, p.PA3, p.PB10, p.PB11,
        p.DMA1_CH2, p.DMA1_CH3, p.DMA1_CH4, p.DMA1_CH5, p.DMA1_CH6, p.DMA1_CH7, p.PA15, p.PB3,
        p.PB8,
    );

    // EV17 board updates:
    // - PB0 is BATTERY_MON (ADC input)
    // - PC14 is MGMT_DEBUG1 (output)
    // - PB15 is NC (floating)

    // EV17 LED A: R=PB5, G=PB4, B=PB1
    let led_a = (
        Output::new(p.PB5, Level::Low, Speed::Low),
        Output::new(p.PB4, Level::Low, Speed::Low),
        Output::new(p.PB1, Level::Low, Speed::Low),
    );

    // LED B no longer exists on EV17
    let led_b = (NoopOutputPin, NoopOutputPin, NoopOutputPin);

    // EV17 NET reset: BOOT=PB6, RST=PC13
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
        Stm32Board,
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
    let p = embassy_stm32::init(rcc_config);

    match Stm32Board.board_version() {
        16 => run_ev16(p).await,
        17 => run_ev17(p).await,
        _ => loop {},
    }
}

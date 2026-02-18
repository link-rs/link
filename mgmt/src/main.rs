#![no_std]
#![no_main]

use cortex_m::singleton;
use embassy_executor::Spawner;
use embassy_stm32::{
    bind_interrupts, dma,
    gpio::{Level, Output, Speed},
    mode::Async,
    peripherals,
    rcc::{Mco, McoConfig, McoPrescaler, McoSource},
    time::Hertz,
    usart,
    usart::{Config, DataBits, Parity, RingBufferedUartRx, StopBits, Uart, UartTx},
};
use embassy_time::Delay;
use embedded_io_async::{ErrorType, Read, Write};
use link::uart_config::SetBaudRate;
use {defmt_rtt as _, panic_probe as _};

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
    DMA1_CHANNEL2_3 => dma::InterruptHandler<peripherals::DMA1_CH2>, dma::InterruptHandler<peripherals::DMA1_CH3>;
    DMA1_CHANNEL4_5_6_7 => dma::InterruptHandler<peripherals::DMA1_CH4>, dma::InterruptHandler<peripherals::DMA1_CH5>, dma::InterruptHandler<peripherals::DMA1_CH6>, dma::InterruptHandler<peripherals::DMA1_CH7>;
});

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

    // MCO on PA8: Output 6 MHz clock for UI chip
    let mut mco_config = McoConfig::default();
    mco_config.prescaler = McoPrescaler::DIV4;
    mco_config.speed = Speed::Low;
    let _mco = Mco::new(p.MCO, p.PA8, McoSource::PLL, mco_config);

    // UART configs from centralized definitions
    // CTL UART always boots at fixed high speed (1000000)
    let ctl_config = uart_config_to_stm32(link::uart_config::CTL_MGMT);

    // UI UART at high speed (1000000)
    let ui_config = uart_config_to_stm32(link::uart_config::MGMT_UI);

    let net_config = uart_config_to_stm32(link::uart_config::MGMT_NET);

    // DMA buffers for ring-buffered RX
    let ctl_rx_buf = singleton!(: [u8; DMA_BUF_SIZE] = [0; DMA_BUF_SIZE]).unwrap();
    let ui_rx_buf = singleton!(: [u8; DMA_BUF_SIZE] = [0; DMA_BUF_SIZE]).unwrap();
    let net_rx_buf = singleton!(: [u8; DMA_BUF_SIZE] = [0; DMA_BUF_SIZE]).unwrap();

    // UART to CTL (uses even parity for bootloader compatibility)
    let (to_ctl, from_ctl) = Uart::new(
        p.USART1, p.PA10, p.PA9, p.DMA1_CH2, p.DMA1_CH3, Irqs, ctl_config,
    )
    .unwrap()
    .split();
    let to_ctl = UartTxWrapper(to_ctl);
    let from_ctl = UartRxWrapper(from_ctl.into_ring_buffered(ctl_rx_buf));

    // UART to UI (uses even parity for bootloader compatibility)
    let (to_ui, from_ui) = Uart::new(
        p.USART2, p.PA3, p.PA2, p.DMA1_CH4, p.DMA1_CH5, Irqs, ui_config,
    )
    .unwrap()
    .split();
    let to_ui = UartTxWrapper(to_ui);
    let from_ui = UartRxWrapper(from_ui.into_ring_buffered(ui_rx_buf));

    // UART to NET (no parity)
    let (to_net, from_net) = Uart::new(
        p.USART3, p.PB11, p.PB10, p.DMA1_CH7, p.DMA1_CH6, Irqs, net_config,
    )
    .unwrap()
    .split();
    let to_net = UartTxWrapper(to_net);
    let from_net = UartRxWrapper(from_net.into_ring_buffered(net_rx_buf));

    // RGB LEDs (R, G, B pin tuples)
    // LED A: R=PA4 (inverted), G=PA6, B=PA7
    let led_a = (
        link::InvertedPin(Output::new(p.PA4, Level::Low, Speed::Low)),
        Output::new(p.PA6, Level::Low, Speed::Low),
        Output::new(p.PA7, Level::Low, Speed::Low),
    );

    // LED B: R=PB0, G=PB6, B=PB15
    let led_b = (
        Output::new(p.PB0, Level::Low, Speed::Low),
        Output::new(p.PB6, Level::Low, Speed::Low),
        Output::new(p.PB15, Level::Low, Speed::Low),
    );

    // UI chip reset control pins
    // RST starts low to hold UI in reset until MGMT clocks are stable
    let ui_boot0 = Output::new(p.PA15, Level::Low, Speed::Low);
    let ui_boot1 = Output::new(p.PB8, Level::High, Speed::Low);
    let ui_rst = Output::new(p.PB3, Level::Low, Speed::Low);
    let ui_reset_pins = link::mgmt::UiResetPins::new(ui_boot0, ui_boot1, ui_rst);

    // NET chip reset control pins
    // RST starts low to hold NET in reset until MGMT clocks are stable
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
    )
    .await;
}

#![no_std]
#![no_main]

use cortex_m::singleton;
use embassy_executor::Spawner;
use embassy_stm32::{
    bind_interrupts,
    exti::ExtiInput,
    gpio::{Level, Output, Pull, Speed},
    peripherals,
    rcc::{
        AHBPrescaler, APBPrescaler, Hse, HseMode, LsConfig, Mco, McoConfig, McoPrescaler,
        McoSource, Pll, PllMul, PllPreDiv, PllSource, Sysclk,
    },
    time::Hertz,
    usart,
    usart::{Config, DataBits, Parity, StopBits, Uart},
};
use {defmt_rtt as _, panic_probe as _};

const DMA_BUF_SIZE: usize = 64;

bind_interrupts!(
    struct Irqs {
        USART1 => usart::InterruptHandler<peripherals::USART1>;
        USART2 => usart::InterruptHandler<peripherals::USART2>;
        USART3_4 => usart::InterruptHandler<peripherals::USART3>;
    }
);

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    // Clock configuration matching the C firmware:
    // HSE (16 MHz) -> PLL (×3) -> SYSCLK (48 MHz) -> AHB (÷2) -> HCLK (24 MHz)
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
        //
        // Disabling this for now, which I think just has the effect of running the peripherals at
        // the full 48MHz instead of 24MHz.
        //
        // config.rcc.ahb_pre = AHBPrescaler::DIV2;

        config.rcc.apb1_pre = APBPrescaler::DIV1;
        config.rcc.ls = LsConfig::default_lsi();

        config
    };
    let p = embassy_stm32::init(rcc_config);

    // MCO on PA8: Output 6 MHz clock for UI chip
    // PLL (48 MHz) with prescaler to get 6 MHz
    // Using DIV8 since McoSource::PLL may or may not include internal ÷2
    let mut mco_config = McoConfig::default();
    mco_config.prescaler = McoPrescaler::DIV4;
    mco_config.speed = Speed::Low;
    let _mco = Mco::new(p.MCO, p.PA8, McoSource::PLL, mco_config);

    let mut config = Config::default();
    config.baudrate = 115200;
    config.data_bits = DataBits::DataBits8;
    config.stop_bits = StopBits::STOP1;
    config.parity = Parity::ParityNone;

    // Hold the NET boot and reset pins high
    let _net_nrst = Output::new(p.PB4, Level::High, Speed::Low);
    let _net_boot = Output::new(p.PB5, Level::High, Speed::Low);

    // DMA buffers for ring-buffered RX
    let ctl_rx_buf = singleton!(: [u8; DMA_BUF_SIZE] = [0; DMA_BUF_SIZE]).unwrap();
    let ui_rx_buf = singleton!(: [u8; DMA_BUF_SIZE] = [0; DMA_BUF_SIZE]).unwrap();
    let net_rx_buf = singleton!(: [u8; DMA_BUF_SIZE] = [0; DMA_BUF_SIZE]).unwrap();

    // UART to CTL
    let (to_ctl, from_ctl) = Uart::new(
        p.USART1, p.PA10, p.PA9, Irqs, p.DMA1_CH2, p.DMA1_CH3, config,
    )
    .unwrap()
    .split();
    let from_ctl = from_ctl.into_ring_buffered(ctl_rx_buf);

    // UART to UI
    let (to_ui, from_ui) = Uart::new(p.USART2, p.PA3, p.PA2, Irqs, p.DMA1_CH4, p.DMA1_CH5, config)
        .unwrap()
        .split();
    let from_ui = from_ui.into_ring_buffered(ui_rx_buf);

    // UART to NET
    let (to_net, from_net) = Uart::new(
        p.USART3, p.PB11, p.PB10, Irqs, p.DMA1_CH7, p.DMA1_CH6, config,
    )
    .unwrap()
    .split();
    let from_net = from_net.into_ring_buffered(net_rx_buf);

    // Signal pins for NET synchronization (active low directly after)
    // PB13 = output to NET (active when we're ready)
    // PB14 = input from NET (wait for NET to be ready)
    let _signal_to_net = Output::new(p.PB13, Level::Low, Speed::Low);
    let _signal_from_net = ExtiInput::new(p.PB14, p.EXTI14, Pull::Down);

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

    link::mgmt::App::new(
        to_ctl, from_ctl, to_ui, from_ui, to_net, from_net, led_a, led_b,
    )
    .run()
    .await;
}

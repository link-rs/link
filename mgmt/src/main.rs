#![no_std]
#![no_main]

use cortex_m::singleton;
use embassy_executor::Spawner;
use embassy_stm32::{
    bind_interrupts,
    gpio::{Level, Output, Speed},
    peripherals,
    rcc::{Mco, McoConfig, McoPrescaler, McoSource},
    time::Hertz,
    usart,
    usart::{Config, DataBits, Parity, StopBits, Uart},
};
use embassy_time::{Duration, Timer};
use {defmt_rtt as _, panic_probe as _};

/// Async delay implementation using embassy_time::Timer.
struct EmbassyDelay;

impl link::mgmt::AsyncDelay for EmbassyDelay {
    async fn delay_ms(&mut self, ms: u32) {
        Timer::after(Duration::from_millis(ms as u64)).await;
    }
}

const DMA_BUF_SIZE: usize = link::shared::MAX_VALUE_SIZE;

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

    // UART config for CTL and UI (even parity for STM32 bootloader compatibility)
    let mut stm_config = Config::default();
    stm_config.baudrate = 115200;
    stm_config.data_bits = DataBits::DataBits8;
    stm_config.stop_bits = StopBits::STOP1;
    stm_config.parity = Parity::ParityEven;

    // UART config for NET (no parity)
    let mut net_config = Config::default();
    net_config.baudrate = 115200;
    net_config.data_bits = DataBits::DataBits8;
    net_config.stop_bits = StopBits::STOP1;
    net_config.parity = Parity::ParityNone;

    // DMA buffers for ring-buffered RX
    let ctl_rx_buf = singleton!(: [u8; DMA_BUF_SIZE] = [0; DMA_BUF_SIZE]).unwrap();
    let ui_rx_buf = singleton!(: [u8; DMA_BUF_SIZE] = [0; DMA_BUF_SIZE]).unwrap();
    let net_rx_buf = singleton!(: [u8; DMA_BUF_SIZE] = [0; DMA_BUF_SIZE]).unwrap();

    // UART to CTL (uses even parity for bootloader compatibility)
    let (to_ctl, from_ctl) = Uart::new(
        p.USART1, p.PA10, p.PA9, Irqs, p.DMA1_CH2, p.DMA1_CH3, stm_config,
    )
    .unwrap()
    .split();
    let from_ctl = from_ctl.into_ring_buffered(ctl_rx_buf);

    // UART to UI (uses even parity for bootloader compatibility)
    let (to_ui, from_ui) = Uart::new(
        p.USART2, p.PA3, p.PA2, Irqs, p.DMA1_CH4, p.DMA1_CH5, stm_config,
    )
    .unwrap()
    .split();
    let from_ui = from_ui.into_ring_buffered(ui_rx_buf);

    // UART to NET (no parity)
    let (to_net, from_net) = Uart::new(
        p.USART3, p.PB11, p.PB10, Irqs, p.DMA1_CH7, p.DMA1_CH6, net_config,
    )
    .unwrap()
    .split();
    let from_net = from_net.into_ring_buffered(net_rx_buf);

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
    // PA15 -> UI BOOT0, PB8 -> UI BOOT1, PB3 -> UI RST
    // Normal state: BOOT0=0, BOOT1=1, RST high
    let ui_boot0 = Output::new(p.PA15, Level::Low, Speed::Low);
    let ui_boot1 = Output::new(p.PB8, Level::High, Speed::Low);
    let ui_rst = Output::new(p.PB3, Level::High, Speed::Low);
    let ui_reset_pins = link::mgmt::UiResetPins::new(ui_boot0, ui_boot1, ui_rst);

    // NET chip reset control pins
    // PB5 -> NET BOOT, PB4 -> NET RST
    // NET chip boot mode is inverted from UI chip:
    //   BOOT high = boot from flash (normal)
    //   BOOT low = boot from bootloader
    // Normal state: BOOT high, RST high
    let net_boot = Output::new(p.PB5, Level::High, Speed::Low);
    let net_rst = Output::new(p.PB4, Level::High, Speed::Low);
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
        EmbassyDelay,
    )
    .await;
}

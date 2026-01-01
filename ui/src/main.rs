#![no_std]
#![no_main]

mod wm8960;

use cortex_m::singleton;
use embassy_executor::Spawner;
use embassy_stm32::{
    bind_interrupts,
    exti::ExtiInput,
    gpio::{Level, Output, Pull, Speed},
    i2c::I2c,
    i2s::{self, I2S},
    peripherals,
    time::Hertz,
    usart::{self, Config, DataBits, Parity, StopBits, Uart},
    Peri,
};
use embassy_time::Delay;
use embedded_hal::delay::DelayNs;
use embedded_hal::i2c::I2c as I2cTrait;
use link::ui::{AudioError, AudioSystem, StereoFrame, STEREO_FRAME_SIZE};
use {defmt_rtt as _, panic_probe as _};

const I2S_BUF_SIZE: usize = STEREO_FRAME_SIZE * 2;

/// Board-level audio system wrapping the I2S peripheral.
pub struct BoardAudioSystem<'d> {
    i2s: I2S<'d, u16>,
}

impl<'d> BoardAudioSystem<'d> {
    pub fn new<I: I2cTrait, D: DelayNs>(
        spi: Peri<'d, peripherals::SPI3>,
        ws: Peri<'d, peripherals::PA15>,
        ck: Peri<'d, peripherals::PC10>,
        sd_tx: Peri<'d, peripherals::PB5>,
        sd_rx: Peri<'d, peripherals::PB4>,
        dma_tx: Peri<'d, peripherals::DMA1_CH7>,
        tx_buf: &'d mut [u16; I2S_BUF_SIZE],
        dma_rx: Peri<'d, peripherals::DMA1_CH0>,
        rx_buf: &'d mut [u16; I2S_BUF_SIZE],
        i2c: &mut I,
        delay: &mut D,
    ) -> Self {
        // 1. Configure WM8960 codec FIRST (before I2S clocks start)
        let mut codec = wm8960::Codec::new(i2c);
        codec.init(delay);
        codec.enable_input(true);
        codec.enable_output(true);

        // 2. Allow codec to stabilize
        delay.delay_ms(20);

        // 3. Construct I2S (codec is ready, clocks are stable)
        let mut config = i2s::Config::default();
        config.mode = i2s::Mode::Slave;
        config.standard = i2s::Standard::Philips;
        config.format = i2s::Format::Data16Channel32;
        config.master_clock = false;
        config.frequency = Hertz(8_000);
        config.clock_polarity = i2s::ClockPolarity::IdleLow;

        let i2s = I2S::new_full_duplex(
            spi, ws, ck, sd_tx, sd_rx, dma_tx, tx_buf, dma_rx, rx_buf, config,
        );

        Self { i2s }
    }
}

impl<'d> AudioSystem for BoardAudioSystem<'d> {
    fn set_input_enabled<I: I2cTrait>(&mut self, i2c: &mut I, enable: bool) {
        wm8960::Codec::new(i2c).enable_input(enable);
    }

    fn set_output_enabled<I: I2cTrait>(&mut self, i2c: &mut I, enable: bool) {
        wm8960::Codec::new(i2c).enable_output(enable);
    }

    async fn start(&mut self) {
        self.i2s.start();
    }

    async fn stop(&mut self) {
        self.i2s.stop().await;
    }

    async fn read_write(
        &mut self,
        tx: &StereoFrame,
        rx: &mut StereoFrame,
    ) -> Result<(), AudioError> {
        match self.i2s.read_write(&tx.0, &mut rx.0).await {
            Ok(_) => Ok(()),
            Err(i2s::Error::Overrun) => Err(AudioError::Overrun),
            Err(i2s::Error::DmaUnsynced) => Err(AudioError::DmaUnsynced),
            Err(_) => Err(AudioError::Overrun),
        }
    }
}

// =============================================================================
// Main
// =============================================================================

const DMA_BUF_SIZE: usize = link::shared::MAX_VALUE_SIZE;

bind_interrupts!(struct Irqs {
    USART1 => usart::InterruptHandler<peripherals::USART1>;
    USART2 => usart::InterruptHandler<peripherals::USART2>;
});

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let config = {
        use embassy_stm32::{rcc::*, time::Hertz};

        let mut config = embassy_stm32::Config::default();

        config.rcc.hse = Some(Hse {
            freq: Hertz(6_000_000),
            mode: HseMode::Bypass,
        });
        config.rcc.sys = Sysclk::PLL1_P;
        config.rcc.pll_src = PllSource::HSE;
        config.rcc.pll = Some(Pll {
            prediv: PllPreDiv::DIV3,
            mul: PllMul::MUL168,
            divp: Some(PllPDiv::DIV2),
            divq: Some(PllQDiv::DIV7),
            divr: None,
        });

        config.rcc.ahb_pre = AHBPrescaler::DIV1;
        config.rcc.apb1_pre = APBPrescaler::DIV4;
        config.rcc.apb2_pre = APBPrescaler::DIV2;
        config.rcc.ls = LsConfig {
            rtc: RtcClockSource::LSI,
            lsi: true,
            lse: None,
        };

        config.rcc.plli2s = Some(Pll {
            prediv: PllPreDiv::DIV3,
            mul: PllMul::MUL50,
            divp: None,
            divq: None,
            divr: Some(PllRDiv::DIV2),
        });

        config
    };
    let p = embassy_stm32::init(config);

    // CLAUDE Same comments on UART config here as for the MGMT chip (mgmt/src/main.rs)

    // UART config for MGMT (even parity for STM32 bootloader compatibility)
    let mut mgmt_config = Config::default();
    mgmt_config.baudrate = 115200;
    mgmt_config.data_bits = DataBits::DataBits8;
    mgmt_config.stop_bits = StopBits::STOP1;
    mgmt_config.parity = Parity::ParityEven;

    // UART config for NET
    let mut net_config = Config::default();
    net_config.baudrate = 460800;
    net_config.data_bits = DataBits::DataBits8;
    net_config.stop_bits = StopBits::STOP2;
    net_config.parity = Parity::ParityNone;

    // DMA buffers for ring-buffered RX
    let mgmt_rx_buf = singleton!(: [u8; DMA_BUF_SIZE] = [0; DMA_BUF_SIZE]).unwrap();
    let net_rx_buf = singleton!(: [u8; DMA_BUF_SIZE] = [0; DMA_BUF_SIZE]).unwrap();

    // I2S DMA buffers
    let i2s_tx_buf = singleton!(: [u16; I2S_BUF_SIZE] = [0; I2S_BUF_SIZE]).unwrap();
    let i2s_rx_buf = singleton!(: [u16; I2S_BUF_SIZE] = [0; I2S_BUF_SIZE]).unwrap();

    // UART to MGMT (USART1: PA10 RX, PA9 TX)
    let (to_mgmt, from_mgmt) = Uart::new(
        p.USART1,
        p.PA10,
        p.PA9,
        Irqs,
        p.DMA2_CH7,
        p.DMA2_CH2,
        mgmt_config,
    )
    .unwrap()
    .split();
    let from_mgmt = from_mgmt.into_ring_buffered(mgmt_rx_buf);

    // UART to NET (USART2: PA3 RX, PA2 TX)
    let (to_net, from_net) = Uart::new(
        p.USART2, p.PA3, p.PA2, Irqs, p.DMA1_CH6, p.DMA1_CH5, net_config,
    )
    .unwrap()
    .split();
    let from_net = from_net.into_ring_buffered(net_rx_buf);

    // RGB LED (initially black)
    let led = (
        Output::new(p.PA6, Level::High, Speed::Low),
        Output::new(p.PC5, Level::High, Speed::Low),
        Output::new(p.PB3, Level::High, Speed::Low),
    );

    // Buttons
    let button_a = ExtiInput::new(p.PC0, p.EXTI0, Pull::Up);
    let button_b = ExtiInput::new(p.PC1, p.EXTI1, Pull::Up);
    let button_mic = ExtiInput::new(p.PA4, p.EXTI4, Pull::Up);

    // Shared I2C bus for EEPROM and audio codec (I2C1: PB6 SCL, PB7 SDA)
    let mut i2c = {
        use embassy_stm32::{gpio::Speed, i2c::Config, time::Hertz};

        let mut config = Config::default();
        config.frequency = Hertz(100_000);
        config.gpio_speed = Speed::VeryHigh;
        config.sda_pullup = false;
        config.scl_pullup = false;
        config.timeout = embassy_time::Duration::from_millis(1000);

        I2c::new_blocking(p.I2C1, p.PB6, p.PB7, config)
    };
    let mut delay = Delay;

    // Audio system: initializes codec via I2C, then constructs I2S
    let audio_system = BoardAudioSystem::new(
        p.SPI3, p.PA15, p.PC10, p.PB5, p.PB4, p.DMA1_CH7, i2s_tx_buf, p.DMA1_CH0, i2s_rx_buf,
        &mut i2c, &mut delay,
    );

    link::ui::App::new(
        to_mgmt,
        from_mgmt,
        to_net,
        from_net,
        led,
        button_a,
        button_b,
        button_mic,
        i2c,
        delay,
        audio_system,
    )
    .run()
    .await;
}

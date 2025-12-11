#![no_std]
#![no_main]

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
};
use embassy_time::Delay;
use link::ui::{AudioError, AudioStream, Frame, FRAME_SIZE};
use {defmt_rtt as _, panic_probe as _};

/// Audio stream wrapper around the Embassy I2S driver.
pub struct I2sAudioStream<'d> {
    i2s: I2S<'d, u16>,
}

impl<'d> I2sAudioStream<'d> {
    pub fn new(i2s: I2S<'d, u16>) -> Self {
        Self { i2s }
    }
}

impl<'d> AudioStream for I2sAudioStream<'d> {
    async fn start(&mut self) {
        self.i2s.start();
    }

    async fn stop(&mut self) {
        self.i2s.stop().await;
    }

    async fn read(&mut self) -> Frame {
        let zero = Frame::default();
        let mut out = Frame::default();

        // Loop to retry on overrun failures
        loop {
            match self.i2s.read_write(&zero.0, &mut out.0).await {
                Ok(_) => return out,
                Err(i2s::Error::Overrun) => {
                    self.i2s.clear();
                    // and retry
                }
                Err(e) => defmt::panic!("i2s read error: {:?}", e),
            }
        }
    }

    async fn write(&mut self, frame: &Frame) {
        let mut ignore = [0u16; FRAME_SIZE];
        self.i2s.read_write(&frame.0, &mut ignore).await.unwrap();
    }

    async fn read_write(&mut self, tx: &Frame, rx: &mut Frame) -> Result<(), AudioError> {
        match self.i2s.read_write(&tx.0, &mut rx.0).await {
            Ok(_) => Ok(()),
            Err(i2s::Error::Overrun) => Err(AudioError::Overrun),
            Err(i2s::Error::DmaUnsynced) => Err(AudioError::DmaUnsynced),
            Err(_) => Err(AudioError::Overrun), // Map other errors to Overrun
        }
    }
}

const DMA_BUF_SIZE: usize = 64;
const I2S_BUF_SIZE: usize = FRAME_SIZE * 2;

bind_interrupts!(struct Irqs {
    USART1 => usart::InterruptHandler<peripherals::USART1>;
    USART2 => usart::InterruptHandler<peripherals::USART2>;
});

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    // Clock configuration for I2S support
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

        // XXX(RLB) The prediv = M value here must be the same as the PLL config above.  The
        // CubeMX clock tree shows one M value for both PLLs.
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

    // UART config for MGMT
    let mut mgmt_config = Config::default();
    mgmt_config.baudrate = 115200;
    mgmt_config.data_bits = DataBits::DataBits8;
    mgmt_config.stop_bits = StopBits::STOP1;
    mgmt_config.parity = Parity::ParityNone;

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

    // RGB LED (R, G, B pin tuple): R=PA6, G=PC5, B=PB3
    let led = (
        Output::new(p.PA6, Level::Low, Speed::Low),
        Output::new(p.PC5, Level::Low, Speed::Low),
        Output::new(p.PB3, Level::Low, Speed::Low),
    );

    // Buttons (active low with pull-up)
    let button_a = ExtiInput::new(p.PC0, p.EXTI0, Pull::Up);
    let button_b = ExtiInput::new(p.PC1, p.EXTI1, Pull::Up);
    let button_mic = ExtiInput::new(p.PA4, p.EXTI4, Pull::Up);

    // Shared I2C bus for EEPROM and audio codec (I2C1: PB6 SCL, PB7 SDA)
    let i2c = {
        use embassy_stm32::{gpio::Speed, i2c::Config, time::Hertz};

        let mut config = Config::default();
        config.frequency = Hertz(100_000);
        config.gpio_speed = Speed::VeryHigh;
        config.sda_pullup = false;
        config.scl_pullup = false;
        config.timeout = embassy_time::Duration::from_millis(1000);

        I2c::new_blocking(p.I2C1, p.PB6, p.PB7, config)
    };
    let delay = Delay;

    // I2S audio stream (SPI3: WS=PA15, CK=PC10, SD_TX=PB5, SD_RX=PB4)
    let i2s = {
        let mut config = i2s::Config::default();
        config.mode = i2s::Mode::Slave;
        config.standard = i2s::Standard::Philips;
        config.format = i2s::Format::Data16Channel32;
        config.master_clock = false;
        config.frequency = Hertz(8_000);
        config.clock_polarity = i2s::ClockPolarity::IdleLow;

        I2S::new_full_duplex(
            p.SPI3, p.PA15, // WS
            p.PC10, // CK
            p.PB5,  // SD (TX/MOSI)
            p.PB4,  // SD (RX/MISO - ext_sd for full duplex)
            p.DMA1_CH7, i2s_tx_buf, p.DMA1_CH0, i2s_rx_buf, config,
        )
    };
    let audio_stream = I2sAudioStream::new(i2s);

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
        audio_stream,
    )
    .run()
    .await;
}

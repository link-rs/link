//! Audio debug firmware - simplified audio loopback and tone generation.
//!
//! Behavior:
//! - No buttons pressed: Raw audio loopback (mic -> speaker)
//! - Button A pressed: A-law (G.711) encode/decode loopback
//! - Button B pressed: Play 400Hz sawtooth wave (full i16 range)

#![no_std]
#![no_main]

mod wm8960;

use embassy_executor::Spawner;
use embassy_stm32::{
    bind_interrupts,
    exti::ExtiInput,
    gpio::{Input, Level, Output, Pull, Speed},
    i2c::I2c,
    i2s::{self, I2S},
    peripherals,
    time::Hertz,
    Peri,
};
use embassy_time::Delay;
use embedded_hal::delay::DelayNs;
use embedded_hal::i2c::I2c as I2cTrait;
use portable_atomic::{AtomicBool, Ordering};
use {defmt_rtt as _, panic_probe as _};

use audio_codec_algorithms::{decode_alaw, encode_alaw};

// Audio constants
const ENCODED_FRAME_SIZE: usize = 160;
const STEREO_FRAME_SIZE: usize = ENCODED_FRAME_SIZE * 2;
const I2S_BUF_SIZE: usize = STEREO_FRAME_SIZE * 2;

// Global atomic flags for button state
static BUTTON_A_PRESSED: AtomicBool = AtomicBool::new(false);
static BUTTON_B_PRESSED: AtomicBool = AtomicBool::new(false);

/// Stereo PCM frame for I2S hardware.
#[derive(Clone, Debug)]
struct StereoFrame([u16; STEREO_FRAME_SIZE]);

impl Default for StereoFrame {
    fn default() -> Self {
        Self([0; STEREO_FRAME_SIZE])
    }
}

impl StereoFrame {
    /// Encode stereo to A-law mono (left channel only).
    fn encode(&self) -> [u8; ENCODED_FRAME_SIZE] {
        let mut encoded = [0u8; ENCODED_FRAME_SIZE];
        for i in 0..ENCODED_FRAME_SIZE {
            let sample = self.0[i * 2] as i16;
            encoded[i] = encode_alaw(sample);
        }
        encoded
    }

    /// Decode A-law mono to stereo (duplicate to both channels).
    fn from_alaw(alaw: &[u8; ENCODED_FRAME_SIZE]) -> Self {
        let mut stereo = Self::default();
        for i in 0..ENCODED_FRAME_SIZE {
            let sample = decode_alaw(alaw[i]) as u16;
            stereo.0[i * 2] = sample; // Left
            stereo.0[i * 2 + 1] = sample; // Right
        }
        stereo
    }
}

/// Audio system wrapping the I2S peripheral.
struct AudioSystem<'d> {
    i2s: I2S<'d, u16>,
}

impl<'d> AudioSystem<'d> {
    fn new<I: I2cTrait, D: DelayNs>(
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
        // Configure WM8960 codec FIRST (before I2S clocks start)
        let mut codec = wm8960::Codec::new(i2c);
        codec.init(delay);
        codec.enable_input(true);
        codec.enable_output(true);

        // Allow codec to stabilize
        delay.delay_ms(20);

        // Construct I2S (codec is ready, clocks are stable)
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

    fn start(&mut self) {
        self.i2s.start();
    }

    async fn read_write(&mut self, tx: &StereoFrame, rx: &mut StereoFrame) -> Result<(), ()> {
        self.i2s.read_write(&tx.0, &mut rx.0).await.map_err(|_| ())
    }
}

bind_interrupts!(
    struct Irqs {}
);

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    defmt::info!("audio-debug: starting");

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

    // I2S DMA buffers
    let i2s_tx_buf = cortex_m::singleton!(: [u16; I2S_BUF_SIZE] = [0; I2S_BUF_SIZE]).unwrap();
    let i2s_rx_buf = cortex_m::singleton!(: [u16; I2S_BUF_SIZE] = [0; I2S_BUF_SIZE]).unwrap();

    // RGB LED (initially green to indicate ready)
    let mut led_r = Output::new(p.PA6, Level::High, Speed::Low);
    let mut led_g = Output::new(p.PC5, Level::Low, Speed::Low); // Green ON
    let mut led_b = Output::new(p.PB3, Level::High, Speed::Low);
    let _ = (&mut led_r, &mut led_g, &mut led_b); // Suppress unused warnings

    // Buttons
    let button_a = Input::new(p.PC0, Pull::Up);
    let button_b = Input::new(p.PC1, Pull::Up);

    // Shared I2C bus for audio codec (I2C1: PB6 SCL, PB7 SDA)
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
    let mut audio_system = AudioSystem::new(
        p.SPI3, p.PA15, p.PC10, p.PB5, p.PB4, p.DMA1_CH7, i2s_tx_buf, p.DMA1_CH0, i2s_rx_buf,
        &mut i2c, &mut delay,
    );

    audio_system.start();

    // Generate a 400Hz square wave
    let square_wave = StereoFrame(core::array::from_fn(|i| {
        const AMPLITUDE: u16 = 0x1ff;
        const FREQ: u16 = 40; // Period in samples (doubled for stereo)
        ((((i / 2) as u16) / (FREQ / 2)) % 2) * AMPLITUDE
    }));

    // Generate a 400Hz stereo sawtooth wave
    let sawtooth_wave = StereoFrame(core::array::from_fn(|i| {
        const MIN: isize = i16::MIN as isize;
        const WAVELEN: usize = 20; // 400 Hz @ 8kHz sample rate
        const STEP: isize = ((u16::MAX as usize) / WAVELEN) as isize;
        let i = i / 2; // stereo duplication
        let i = (i % WAVELEN) as isize; // periodicity
        ((MIN + i * STEP) as i16) as u16
    }));

    // Buffer to hold last received frame
    let mut rx_frame = StereoFrame::default();

    // Play one second of square wave
    let mut zero_stereo = StereoFrame::default();
    for _i in 0..50 {
        // 50 frames at 20ms each = 1 seconds
        let _ = audio_system
            .read_write(&square_wave, &mut zero_stereo)
            .await;
    }

    loop {
        let alaw_loopback = button_a.is_high();
        let play_sawtooth = button_b.is_high();

        let tx_frame = if alaw_loopback {
            let encoded = rx_frame.encode();
            StereoFrame::from_alaw(&encoded)
        } else if play_sawtooth {
            sawtooth_wave.clone()
        } else {
            rx_frame.clone()
        };

        let _ = audio_system.read_write(&tx_frame, &mut rx_frame).await;
    }
}

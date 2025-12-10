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
    usart::{self, Config, DataBits, Parity, StopBits, Uart},
};
use embassy_time::Delay;
use link::ui::{AudioError, AudioStream, Frame, FRAME_SIZE};
use {defmt_rtt as _, panic_probe as _};

/// Audio stream wrapper around the Embassy I2S driver.
#[allow(dead_code)]
pub struct I2sAudioStream<'d> {
    i2s: I2S<'d, u16>,
}

#[allow(dead_code)]
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

/// Stub audio stream for use when I2S hardware is not yet configured.
/// This is a placeholder that does nothing - useful for testing other functionality.
pub struct StubAudioStream;

impl AudioStream for StubAudioStream {
    async fn start(&mut self) {}
    async fn stop(&mut self) {}
    async fn read(&mut self) -> Frame {
        Frame::default()
    }
    async fn write(&mut self, _frame: &Frame) {}
    async fn read_write(&mut self, _tx: &Frame, rx: &mut Frame) -> Result<(), AudioError> {
        *rx = Frame::default();
        Ok(())
    }
}

const DMA_BUF_SIZE: usize = 64;

bind_interrupts!(struct Irqs {
    USART1 => usart::InterruptHandler<peripherals::USART1>;
    USART2 => usart::InterruptHandler<peripherals::USART2>;
});

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let p = embassy_stm32::init(Default::default());

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

    // I2C for EEPROM (I2C1: PB6 SCL, PB7 SDA)
    let i2c_eeprom = I2c::new_blocking(p.I2C1, p.PB6, p.PB7, Default::default());
    let delay = Delay;

    // I2C for audio codec (I2C2: PB10 SCL, PB11 SDA)
    let i2c_audio = I2c::new_blocking(p.I2C2, p.PB10, p.PB11, Default::default());
    let audio_codec = link::ui::AudioControl::new(i2c_audio);

    // Audio stream (stub for now - I2S requires additional setup)
    // TODO: Initialize I2S with proper pins and DMA buffers
    let audio_stream = StubAudioStream;

    link::ui::App::new(
        to_mgmt,
        from_mgmt,
        to_net,
        from_net,
        led,
        button_a,
        button_b,
        button_mic,
        i2c_eeprom,
        delay,
        audio_codec,
        audio_stream,
    )
    .run()
    .await;
}

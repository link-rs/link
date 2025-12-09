#![no_std]
#![no_main]

use cortex_m::singleton;
use embassy_executor::Spawner;
use embassy_stm32::{
    bind_interrupts,
    gpio::{Level, Output, Speed},
    peripherals,
    usart::{self, Config, DataBits, Parity, StopBits, Uart},
};
use {defmt_rtt as _, panic_probe as _};

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

    link::ui::App::new(to_mgmt, from_mgmt, to_net, from_net, led)
        .run()
        .await;
}

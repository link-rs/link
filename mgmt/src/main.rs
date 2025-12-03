#![no_std]
#![no_main]

use cortex_m::singleton;
use embassy_executor::Spawner;
use embassy_stm32::{
    bind_interrupts, peripherals, usart,
    usart::{Config, DataBits, Parity, StopBits, Uart},
};
use {defmt_rtt as _, panic_probe as _};

const DMA_BUF_SIZE: usize = 256;

bind_interrupts!(struct Irqs {
    USART1 => usart::InterruptHandler<peripherals::USART1>;
    USART2 => usart::InterruptHandler<peripherals::USART2>;
    USART3_4 => usart::InterruptHandler<peripherals::USART3>;
});

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let p = embassy_stm32::init(Default::default());

    let mut config = Config::default();
    config.baudrate = 115200;
    config.data_bits = DataBits::DataBits8;
    config.stop_bits = StopBits::STOP1;
    config.parity = Parity::ParityNone;

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

    link::mgmt::run(to_ctl, from_ctl, to_ui, from_ui, to_net, from_net).await;
}

#![no_std]
#![no_main]

use cortex_m::singleton;
use embassy_executor::Spawner;
use embassy_stm32::{
    bind_interrupts,
    mode::Async,
    peripherals,
    time::Hertz,
    usart::{self, Config, DataBits, Parity, StopBits, Uart, UartTx},
};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex, signal::Signal};
use embassy_time::{Duration, Timer};
use heapless::String;
use {defmt_rtt as _, panic_probe as _};

bind_interrupts!(
    struct Irqs {
        USART1 => usart::InterruptHandler<peripherals::USART1>;
    }
);

const BAUD_RATE: usize = 460800;
const DMA_BUF_SIZE: usize = 256;

type TxMutex = Mutex<CriticalSectionRawMutex, UartTx<'static, Async>>;

#[embassy_executor::task]
async fn counter_task(
    tx: &'static TxMutex,
    rx_count: &'static embassy_sync::signal::Signal<
        embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex,
        u32,
    >,
) {
    let mut count: u32 = 0;
    loop {
        // Check if we received any bytes
        if let Some(rx_total) = rx_count.try_take() {
            use core::fmt::Write;
            let mut msg: String<64> = String::new();
            let _ = write!(msg, "Counter: {} | RX bytes: {}\r\n", count, rx_total);

            let mut tx = tx.lock().await;
            let _ = tx.write(msg.as_bytes()).await;
            drop(tx);
        } else {
            use core::fmt::Write;
            let mut msg: String<32> = String::new();
            let _ = write!(msg, "Counter: {}\r\n", count);

            let mut tx = tx.lock().await;
            let _ = tx.write(msg.as_bytes()).await;
            drop(tx);
        }

        count = count.wrapping_add(1);
        Timer::after(Duration::from_secs(1)).await;
    }
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    // Clock configuration - same as main MGMT firmware
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
        config.rcc.apb1_pre = APBPrescaler::DIV1;
        config.rcc.ls = LsConfig::default_lsi();

        config
    };
    let p = embassy_stm32::init(rcc_config);

    // Get baud rate from build-time environment variable
    let baud_rate: u32 = env!("BAUD_RATE").parse().unwrap_or(460800);

    defmt::info!("MGMT Echo Test - Baud Rate: {}", BAUD_RATE);

    // Configure UART: no parity (screen doesn't handle parity well)
    let mut uart_config = Config::default();
    uart_config.baudrate = baud_rate;
    uart_config.data_bits = DataBits::DataBits8;
    uart_config.parity = Parity::ParityNone;
    uart_config.stop_bits = StopBits::STOP1;

    // DMA buffer for RX
    let rx_buf = singleton!(: [u8; DMA_BUF_SIZE] = [0; DMA_BUF_SIZE]).unwrap();

    // UART to CTL (USART1: PA10 RX, PA9 TX)
    let uart = Uart::new(
        p.USART1,
        p.PA10,
        p.PA9,
        Irqs,
        p.DMA1_CH2,
        p.DMA1_CH3,
        uart_config,
    )
    .unwrap();

    let (tx, rx) = uart.split();
    let mut rx = rx.into_ring_buffered(rx_buf);

    // Create shared TX wrapped in mutex
    let tx_mutex = singleton!(: TxMutex = Mutex::new(tx)).unwrap();

    // Create signal for RX byte count
    let rx_signal = singleton!(: Signal<CriticalSectionRawMutex, u32> = Signal::new()).unwrap();

    // Spawn counter task
    spawner.spawn(counter_task(tx_mutex, rx_signal).unwrap());

    // Simple echo loop - read and write back as fast as possible
    let mut buf = [0u8; 64];
    let mut total_rx: u32 = 0;
    loop {
        match rx.read(&mut buf).await {
            Ok(n) => {
                total_rx = total_rx.wrapping_add(n as u32);
                rx_signal.signal(total_rx);

                // Echo back with markers so it's visible
                let mut tx = tx_mutex.lock().await;
                let _ = tx.write(b"ECHO[").await;
                let _ = tx.write(&buf[..n]).await;
                let _ = tx.write(b"]\r\n").await;
            }
            Err(_) => {
                // Ignore errors, just keep trying
            }
        }
    }
}

#![no_std]
#![no_main]

use defmt::info;
use embassy_executor::Spawner;
use esp_hal::{
    clock::CpuClock,
    timer::systimer::SystemTimer,
    uart::{Config, StopBits, Uart},
};

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    defmt::error!("panic: {:?}", info);
    loop {}
}

extern crate alloc;
esp_bootloader_esp_idf::esp_app_desc!();

#[esp_hal_embassy::main]
async fn main(_spawner: Spawner) {
    rtt_target::rtt_init_defmt!();

    info!("net: initializing");

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    esp_alloc::heap_allocator!(size: 64 * 1024);

    let timer0 = SystemTimer::new(peripherals.SYSTIMER);
    esp_hal_embassy::init(timer0.alarm0);

    // UART to MGMT (UART0: GPIO43 TX, GPIO44 RX, 115200 8N1)
    let mgmt_uart = Uart::new(
        peripherals.UART0,
        Config::default().with_baudrate(115200),
    )
    .unwrap()
    .with_tx(peripherals.GPIO43)
    .with_rx(peripherals.GPIO44)
    .into_async();
    let (from_mgmt, to_mgmt) = mgmt_uart.split();

    // UART to UI (UART1: GPIO17 TX, GPIO18 RX, 460800 8N2)
    let ui_uart = Uart::new(
        peripherals.UART1,
        Config::default()
            .with_baudrate(460800)
            .with_stop_bits(StopBits::_2),
    )
    .unwrap()
    .with_tx(peripherals.GPIO17)
    .with_rx(peripherals.GPIO18)
    .into_async();
    let (from_ui, to_ui) = ui_uart.split();

    link::net::run(to_mgmt, from_mgmt, to_ui, from_ui).await;
}

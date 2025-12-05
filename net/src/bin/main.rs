#![no_std]
#![no_main]

use defmt::info;
use embassy_executor::Spawner;
use esp_hal::{
    clock::CpuClock,
    gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull},
    timer::systimer::SystemTimer,
    uart::{
        Config, CtsConfig, HwFlowControl, Parity, RtsConfig, RxConfig, StopBits, SwFlowControl,
        Uart,
    },
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

    let flow_ctl_disabled = HwFlowControl {
        cts: CtsConfig::Disabled,
        rts: RtsConfig::Disabled,
    };

    // UART to MGMT (UART0: GPIO43 TX, GPIO44 RX, 115200 8N1)
    // Note: UART0 uses the default RXD0/TXD0 pins
    let mgmt_config = Config::default()
        .with_baudrate(115200)
        .with_parity(Parity::None)
        .with_rx(RxConfig::default().with_fifo_full_threshold(1))
        .with_sw_flow_ctrl(SwFlowControl::Disabled)
        .with_hw_flow_ctrl(flow_ctl_disabled);
    let mgmt_uart = Uart::new(peripherals.UART0, mgmt_config)
        .unwrap()
        .with_tx(peripherals.GPIO43)
        .with_rx(peripherals.GPIO44)
        .into_async();
    let (from_mgmt, to_mgmt) = mgmt_uart.split();

    // UART to UI (UART1: GPIO17 TX, GPIO18 RX, 460800 8N2)
    let ui_config = Config::default()
        .with_baudrate(460800)
        .with_stop_bits(StopBits::_2)
        .with_rx(RxConfig::default().with_fifo_full_threshold(1))
        .with_hw_flow_ctrl(flow_ctl_disabled);
    let ui_uart = Uart::new(peripherals.UART1, ui_config)
        .unwrap()
        .with_tx(peripherals.GPIO17)
        .with_rx(peripherals.GPIO18)
        .into_async();
    let (from_ui, to_ui) = ui_uart.split();

    // Signal pins for MGMT synchronization
    // GPIO15 = output to MGMT (signal that we're ready)
    // GPIO16 = input from MGMT (wait for MGMT to be ready)
    let signal_to_mgmt = Output::new(peripherals.GPIO15, Level::Low, OutputConfig::default());
    let signal_from_mgmt = Input::new(peripherals.GPIO16, InputConfig::default().with_pull(Pull::Down));

    link::net::run(
        to_mgmt,
        from_mgmt,
        to_ui,
        from_ui,
        signal_to_mgmt,
        signal_from_mgmt,
    )
    .await;
}

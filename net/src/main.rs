#![no_std]
#![no_main]

use defmt::info;
use embassy_executor::Spawner;
use esp_bootloader_esp_idf::partitions;
use esp_hal::{
    clock::CpuClock,
    gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull},
    timer::systimer::SystemTimer,
    uart::{
        Config, CtsConfig, HwFlowControl, Parity, RtsConfig, RxConfig, StopBits, SwFlowControl,
        Uart,
    },
};
use esp_storage::FlashStorage;

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

    // Signal pins for MGMT synchronization (not yet used)
    // GPIO15 = output to MGMT (signal that we're ready)
    // GPIO16 = input from MGMT (wait for MGMT to be ready)
    let _signal_to_mgmt = Output::new(peripherals.GPIO15, Level::Low, OutputConfig::default());
    let _signal_from_mgmt = Input::new(peripherals.GPIO16, InputConfig::default().with_pull(Pull::Down));

    // RGB LED (R, G, B pin tuple): R=GPIO38, G=GPIO37, B=GPIO36
    let led = (
        Output::new(peripherals.GPIO38, Level::Low, OutputConfig::default()),
        Output::new(peripherals.GPIO37, Level::Low, OutputConfig::default()),
        Output::new(peripherals.GPIO36, Level::Low, OutputConfig::default()),
    );

    // Flash storage for NET settings (WiFi credentials, MOQ URL)
    // Read partition table to find the NVS partition
    let mut flash = FlashStorage::new();
    let mut pt_buf = [0u8; partitions::PARTITION_TABLE_MAX_LEN];
    let pt = partitions::read_partition_table(&mut flash, &mut pt_buf)
        .expect("Failed to read partition table");
    let nvs = pt
        .find_partition(partitions::PartitionType::Data(
            partitions::DataPartitionSubType::Nvs,
        ))
        .expect("Failed to find NVS partition")
        .expect("NVS partition not found");
    let flash_offset = nvs.offset();
    info!("net: NVS partition at offset {:#x}", flash_offset);

    link::net::App::new(to_mgmt, from_mgmt, to_ui, from_ui, led, flash, flash_offset)
        .run()
        .await;
}

# bootloader

A `no_std` Rust library for communicating with embedded device bootloaders over serial connections.

## Supported Protocols

- **STM32 (AN3155)**: USART bootloader protocol as described in [ST Application Note AN3155](https://www.st.com/resource/en/application_note/an3155-usart-protocol-used-in-the-stm32-bootloader-stmicroelectronics.pdf)
- **ESP32-S3**: ROM bootloader protocol as described in the [Espressif Serial Protocol Documentation](https://docs.espressif.com/projects/esptool/en/latest/esp32s3/advanced-topics/serial-protocol.html)

## Usage

Add to your `Cargo.toml`:

```toml
[dependencies]
bootloader = "0.1"
```

## STM32 Bootloader

The `stm` module implements the STM32 USART bootloader protocol.

### Supported Commands

| Command | Code | Description |
|---------|------|-------------|
| Get | 0x00 | Get bootloader version and supported commands |
| Get Version | 0x01 | Get protocol version |
| Get ID | 0x02 | Get chip product ID |
| Read Memory | 0x11 | Read up to 256 bytes from memory |
| Go | 0x21 | Jump to address and execute code |
| Write Memory | 0x31 | Write up to 256 bytes to memory |
| Erase | 0x43 | Erase flash pages (legacy, single-byte page numbers) |
| Extended Erase | 0x44 | Erase flash pages (two-byte page numbers) |
| Write Protect | 0x63 | Enable write protection for sectors |
| Write Unprotect | 0x73 | Disable write protection |
| Readout Protect | 0x82 | Enable read protection |
| Readout Unprotect | 0x92 | Disable read protection (erases flash!) |

### Example

```rust
use bootloader::stm::{Bootloader, SpecialErase};

async fn flash_firmware<R, W>(reader: R, writer: W, firmware: &[u8]) -> Result<(), bootloader::stm::Error<R::Error>>
where
    R: embedded_io_async::Read,
    W: embedded_io_async::Write,
    W::Error: Into<R::Error>,
{
    let mut bl = Bootloader::new(reader, writer);

    // Initialize communication (sends 0x7F for auto-baud detection)
    bl.init().await?;

    // Get bootloader info
    let info = bl.get().await?;
    println!("Bootloader version: 0x{:02X}", info.version);

    // Get chip ID
    let chip_id = bl.get_id().await?;
    println!("Chip ID: 0x{:04X}", chip_id);

    // Erase flash (use extended_erase for STM32F4, erase for STM32F0)
    bl.extended_erase(None, Some(SpecialErase::MassErase)).await?;

    // Write firmware in 256-byte chunks
    let base_address = 0x0800_0000u32;
    for (i, chunk) in firmware.chunks(256).enumerate() {
        let address = base_address + (i * 256) as u32;
        bl.write_memory(address, chunk).await?;
    }

    // Verify by reading back
    let mut buf = [0u8; 256];
    bl.read_memory(base_address, &mut buf).await?;
    assert_eq!(&buf[..firmware.len().min(256)], &firmware[..firmware.len().min(256)]);

    // Jump to application
    bl.go(base_address).await?;

    Ok(())
}
```

### Entering Bootloader Mode

To use this library, the STM32 must be in bootloader mode. This is typically done by:

1. Setting BOOT0 pin high
2. Resetting the device

The bootloader will then wait for the 0x7F initialization byte on the USART.

## ESP32-S3 Bootloader

The `esp` module implements the ESP32-S3 ROM bootloader protocol using SLIP framing.

### Supported Commands

| Command | Code | Description |
|---------|------|-------------|
| Sync | 0x08 | Synchronize with bootloader |
| Read Reg | 0x0A | Read 32-bit register |
| Write Reg | 0x09 | Write 32-bit register |
| Mem Begin | 0x05 | Start RAM download |
| Mem Data | 0x07 | RAM data transmission |
| Mem End | 0x06 | Finish RAM download |
| Flash Begin | 0x02 | Initiate flash download |
| Flash Data | 0x03 | Flash data transmission |
| Flash End | 0x04 | Complete flash download |
| Flash Defl Begin | 0x10 | Start compressed flash download |
| Flash Defl Data | 0x11 | Compressed flash data |
| Flash Defl End | 0x12 | End compressed flash download |
| SPI Attach | 0x0D | Enable SPI interface |
| SPI Set Params | 0x0B | Configure SPI flash parameters |
| Change Baudrate | 0x0F | Modify baud rate |
| SPI Flash MD5 | 0x13 | Hash flash region |
| Get Security Info | 0x14 | Read security data |

### Example

```rust
use bootloader::esp::{Bootloader, write_flash};

async fn flash_esp32<R, W>(reader: R, writer: W, firmware: &[u8]) -> Result<(), bootloader::esp::Error<R::Error>>
where
    R: embedded_io_async::Read,
    W: embedded_io_async::Write,
    W::Error: Into<R::Error>,
{
    let mut bl = Bootloader::new(reader, writer);

    // Synchronize with bootloader
    bl.sync().await?;

    // Attach SPI flash
    bl.spi_attach(0).await?;

    // Write firmware to flash at address 0x10000
    write_flash(&mut bl, 0x10000, firmware, 0x4000).await?;

    Ok(())
}
```

### Low-Level Flash Operations

```rust
// Begin flash operation
let packet_count = bl.flash_begin(
    firmware.len() as u32,  // total size
    0x4000,                  // block size (16KB)
    0x10000,                 // flash offset
).await?;

// Send data in chunks
for (seq, chunk) in firmware.chunks(0x4000).enumerate() {
    bl.flash_data(chunk, seq as u32).await?;
}

// End flash operation
bl.flash_end(false).await?;  // false = don't reboot
```

### Memory Operations

```rust
use bootloader::esp::write_mem;

// Write to RAM and optionally execute
write_mem(
    &mut bl,
    0x4008_0000,  // RAM address
    &data,
    0x1000,       // block size
    true,         // execute after write
    0x4008_0000,  // entry point
).await?;
```

### Register Access

```rust
// Read a register
let value = bl.read_reg(0x3FF0_0000).await?;

// Write a register
bl.write_reg(
    0x3FF0_0000,  // address
    0x1234,       // value
    0xFFFF_FFFF,  // mask (all bits)
    0,            // delay (microseconds)
).await?;
```

### Entering Bootloader Mode

To use this library, the ESP32-S3 must be in bootloader mode. This is typically done by:

1. Holding GPIO0 low
2. Resetting the device (pulse EN/RST low)
3. Releasing GPIO0

The bootloader initializes at 115200 baud and waits for SYNC commands.

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.

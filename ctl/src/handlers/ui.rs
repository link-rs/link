//! UI chip command handlers.

use super::Core;
use crate::{
    AudioAction, AudioModeAction, CaptureMode, GetSetHex, GetSetU32, LogsAction, LoopbackAction,
    PinAction, PinLevel, PlayMode, ResetAction, StackAction, UiAction, VolumeAction,
};
use indicatif::{ProgressBar, ProgressStyle};
use link::ctl::SetTimeout;
use link::ctl::audio_capture::{AudioSink, CaptureSession, PlaybackSession};
use link::ctl::flash::FlashPhase;
use link::protocol_config::timeouts;
use link::{AdjDirection, AudioMode, PinValue, UiLoopbackMode, UiToCtl};
use std::io::Write;
use std::sync::mpsc;
use std::time::Duration;

pub async fn handle_ui(
    action: UiAction,
    core: &mut Core,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        UiAction::Ping { data } => {
            println!("Sending UI ping with data: {}", data);
            core.ui_ping(data.as_bytes()).await?;
            println!("Received pong!");
            Ok(())
        }
        UiAction::Info => {
            println!("UI Bootloader Info");
            println!("==================\n");
            println!("Resetting UI chip to bootloader mode...");

            let delay = |ms| tokio::time::sleep(Duration::from_millis(ms));
            let info = core
                .get_ui_bootloader_info(delay)
                .await
                .map_err(|_| "Failed to get bootloader info")?;

            println!(
                "Bootloader Version: {}.{} (0x{:02X})",
                info.version_major(),
                info.version_minor(),
                info.bootloader_version
            );
            println!(
                "Chip ID: 0x{:04X} ({})",
                info.chip_id,
                link::ctl::stm::chip_name(info.chip_id)
            );

            println!("\nSupported Commands ({}):", info.command_count);
            for i in 0..info.command_count {
                let cmd = info.commands[i];
                println!("  0x{:02X} - {}", cmd, link::ctl::stm::command_name(cmd));
            }

            if let Some(flash) = info.flash_sample {
                println!("\nFlash Memory Sample (0x08000000):");
                print!("  ");
                for (i, byte) in flash.iter().enumerate() {
                    print!("{:02X} ", byte);
                    if (i + 1) % 16 == 0 && i + 1 < flash.len() {
                        print!("\n  ");
                    }
                }
                println!();

                println!("\nVector Table Analysis:");
                if let Some(sp) = info.sp() {
                    println!("  Initial SP:      0x{:08X}", sp);
                }
                if let Some(reset) = info.reset_handler() {
                    println!("  Reset Handler:   0x{:08X}", reset);
                }
                if info.sp_valid() {
                    println!("  (SP appears valid - points to SRAM)");
                }
                if info.reset_valid() {
                    println!("  (Reset handler appears valid - points to Flash, Thumb mode)");
                }
            } else {
                println!("\nFlash Memory: Could not read (read protection may be enabled)");
            }

            println!("UI chip reset back to user mode.");
            println!("Done!");
            Ok(())
        }
        UiAction::Flash { file, no_verify } => {
            println!("UI Flash");
            println!("========\n");

            let firmware = std::fs::read(&file)?;
            println!("Firmware: {} ({} bytes)", file.display(), firmware.len());
            if no_verify {
                println!("Verification: skipped");
            }

            println!("Resetting UI chip to bootloader mode...\n");

            let pb = ProgressBar::new(firmware.len() as u64);
            let bytes_style = ProgressStyle::default_bar()
                .template("{prefix:>12} [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({percent}%)")
                .unwrap()
                .progress_chars("#>-");
            let sectors_style = ProgressStyle::default_bar()
                .template("{prefix:>12} [{bar:40.cyan/blue}] {pos}/{len} sectors ({percent}%)")
                .unwrap()
                .progress_chars("#>-");
            pb.set_style(sectors_style.clone());

            let mut current_phase = None;
            let delay = |ms| tokio::time::sleep(Duration::from_millis(ms));
            let verify = !no_verify;
            let result = core
                .flash_ui(&firmware, delay, verify, |phase, progress, total| {
                    if current_phase != Some(phase) {
                        current_phase = Some(phase);
                        match phase {
                            FlashPhase::Compressing => {}
                            FlashPhase::Erasing => {
                                pb.set_style(sectors_style.clone());
                                pb.set_prefix("Erasing");
                            }
                            FlashPhase::Writing => {
                                pb.set_style(bytes_style.clone());
                                pb.set_prefix("Writing");
                            }
                            FlashPhase::Verifying => {
                                pb.set_style(bytes_style.clone());
                                pb.set_prefix("Verifying");
                            }
                        }
                        pb.set_length(total as u64);
                        pb.set_position(0);
                    }
                    pb.set_position(progress as u64);
                })
                .await;

            pb.finish_and_clear();

            match result {
                Ok(()) => {
                    println!("Flash complete!");
                    println!("UI chip reset back to user mode.");
                    Ok(())
                }
                Err(e) => Err(format!("Flash failed: {:?}", e).into()),
            }
        }
        UiAction::Version { action } => match action.unwrap_or_default() {
            GetSetU32::Get => {
                let version = core.get_version().await?;
                println!("{}", version);
                Ok(())
            }
            GetSetU32::Set { value } => {
                core.set_version(value).await?;
                println!("Version set to {}", value);
                Ok(())
            }
        },
        UiAction::SFrameKey { action } => match action.unwrap_or_default() {
            GetSetHex::Get => {
                let key = core.get_sframe_key().await?;
                println!("{}", hex::encode(key));
                Ok(())
            }
            GetSetHex::Set { value } => {
                let key_bytes = hex::decode(&value).map_err(|_| "Invalid hex string")?;
                core.set_sframe_key(&key_bytes).await?;
                println!("SFrame key set to {}", value);
                Ok(())
            }
        },
        UiAction::Loopback { action } => match action.unwrap_or_default() {
            LoopbackAction::Get => {
                let mode = core.ui_get_loopback().await?;
                println!("{:?}", mode);
                Ok(())
            }
            LoopbackAction::Off => {
                core.ui_set_loopback(UiLoopbackMode::Off).await?;
                println!("UI loopback: off");
                Ok(())
            }
            LoopbackAction::Raw => {
                core.ui_set_loopback(UiLoopbackMode::Raw).await?;
                println!("UI loopback: raw");
                Ok(())
            }
            LoopbackAction::Alaw => {
                core.ui_set_loopback(UiLoopbackMode::Alaw).await?;
                println!("UI loopback: alaw");
                Ok(())
            }
            LoopbackAction::Sframe => {
                core.ui_set_loopback(UiLoopbackMode::Sframe).await?;
                println!("UI loopback: sframe");
                Ok(())
            }
        },
        UiAction::Boot0 {
            action: PinAction::Set { level },
        } => {
            let value = match level {
                PinLevel::High => PinValue::High,
                PinLevel::Low => PinValue::Low,
            };
            core.set_ui_boot0(value).await?;
            println!("UI BOOT0: {:?}", value);
            Ok(())
        }
        UiAction::Boot1 {
            action: PinAction::Set { level },
        } => {
            let value = match level {
                PinLevel::High => PinValue::High,
                PinLevel::Low => PinValue::Low,
            };
            core.set_ui_boot1(value).await?;
            println!("UI BOOT1: {:?}", value);
            Ok(())
        }
        UiAction::Rst {
            action: PinAction::Set { level },
        } => {
            let value = match level {
                PinLevel::High => PinValue::High,
                PinLevel::Low => PinValue::Low,
            };
            core.set_ui_rst(value).await?;
            println!("UI RST: {:?}", value);
            Ok(())
        }
        UiAction::Reset { action } => match action.unwrap_or_default() {
            ResetAction::User => {
                let delay = |ms| tokio::time::sleep(Duration::from_millis(ms));
                core.reset_ui_to_user(delay).await?;
                println!("UI chip reset to user mode");
                Ok(())
            }
            ResetAction::Bootloader => {
                let delay = |ms| tokio::time::sleep(Duration::from_millis(ms));
                core.reset_ui_to_bootloader(delay).await?;
                println!("UI chip reset to bootloader mode");
                Ok(())
            }
            ResetAction::Hold => {
                core.hold_ui_reset().await?;
                println!("UI chip held in reset");
                Ok(())
            }
            ResetAction::Release => {
                core.set_ui_rst(PinValue::High).await?;
                println!("UI chip released from reset");
                Ok(())
            }
        },
        UiAction::Monitor { reset } => {
            if reset {
                println!("Resetting UI chip...");
                let delay = |ms| tokio::time::sleep(Duration::from_millis(ms));
                core.reset_ui_to_user(delay).await?;
            }
            println!("Monitoring UI chip logs (ESC to stop)...\n");

            // Set a short timeout for non-blocking reads
            if let Err(e) = core
                .port_mut()
                .set_timeout(Duration::from_millis(timeouts::MONITOR_MS))
            {
                eprintln!("Warning: couldn't set timeout: {}", e);
            }

            use crossterm::event::{self, Event, KeyCode, KeyEvent};
            use crossterm::terminal;

            // Enable raw mode to capture ESC
            terminal::enable_raw_mode()?;

            let result = async {
                loop {
                    // Check for key press (non-blocking)
                    if event::poll(Duration::from_millis(0))? {
                        if let Event::Key(KeyEvent {
                            code: KeyCode::Esc, ..
                        }) = event::read()?
                        {
                            return Ok::<(), Box<dyn std::error::Error>>(());
                        }
                    }

                    // Check for TLV data from UI (timeout-aware)
                    match core.try_read_ui_log().await {
                        Ok(Some(msg)) => {
                            // Use \r\n for raw terminal mode
                            print!("[UI] {}\r\n", msg);
                            std::io::stdout().flush().ok();
                        }
                        Ok(None) => {
                            // Timeout or non-log TLV, continue
                        }
                        Err(e) => {
                            if e.is_timeout() {
                                continue;
                            }
                            return Err(format!("Read error: {:?}", e).into());
                        }
                    }
                }
            }
            .await;

            // Always restore terminal mode and timeout
            terminal::disable_raw_mode()?;

            // Restore timeout to normal
            if let Err(e) = core
                .port_mut()
                .set_timeout(Duration::from_secs(timeouts::NORMAL_SECS))
            {
                eprintln!("Warning: couldn't restore timeout: {}", e);
            }

            println!("\nMonitor stopped.");

            result
        }
        UiAction::Stack { action } => match action.unwrap_or_default() {
            StackAction::Info => {
                let info = core.ui_get_stack_info().await?;
                println!("Stack Base:  0x{:08X}", info.stack_base);
                println!("Stack Top:   0x{:08X}", info.stack_top);
                println!(
                    "Stack Size:  {} bytes ({:.1} KB)",
                    info.stack_size,
                    info.stack_size as f64 / 1024.0
                );
                println!(
                    "Stack Used:  {} bytes ({:.1}%)",
                    info.stack_used,
                    info.usage_percent()
                );
                println!("Stack Free:  {} bytes", info.stack_free());
                Ok(())
            }
            StackAction::Repaint => {
                core.ui_repaint_stack().await?;
                println!("Stack repainted");
                Ok(())
            }
        },
        UiAction::Logs { action } => match action.unwrap_or_default() {
            LogsAction::Get => {
                let enabled = core.ui_get_logs_enabled().await?;
                println!("{}", if enabled { "on" } else { "off" });
                Ok(())
            }
            LogsAction::On => {
                core.ui_set_logs_enabled(true).await?;
                println!("UI logs: on");
                Ok(())
            }
            LogsAction::Off => {
                core.ui_set_logs_enabled(false).await?;
                println!("UI logs: off");
                Ok(())
            }
        },
        UiAction::ClearStorage => {
            core.ui_clear_storage().await?;
            println!("UI storage cleared");
            Ok(())
        }
        UiAction::Volume { action } => match action.unwrap_or_default() {
            VolumeAction::Get => {
                let volume = core.ui_get_volume().await?;
                println!("{}", volume);
                Ok(())
            }
            VolumeAction::Set { value } => {
                let volume = core.ui_set_volume(value).await?;
                println!("Volume set to {}", volume);
                Ok(())
            }
            VolumeAction::Up { amount } => {
                let volume = core
                    .ui_adjust_volume(AdjDirection::Up, amount.unwrap_or(1))
                    .await?;
                println!("Volume set to {}", volume);
                Ok(())
            }
            VolumeAction::Down { amount } => {
                let volume = core
                    .ui_adjust_volume(AdjDirection::Down, amount.unwrap_or(1))
                    .await?;
                println!("Volume set to {}", volume);
                Ok(())
            }
        },
        UiAction::MicPreamp { action } => match action.unwrap_or_default() {
            VolumeAction::Get => {
                let preamp = core.ui_get_mic_preamp().await?;
                println!("{}", preamp);
                Ok(())
            }
            VolumeAction::Set { value } => {
                let preamp = core.ui_set_mic_preamp(value).await?;
                println!("Mic preamp set to {}", preamp);
                Ok(())
            }
            VolumeAction::Up { amount } => {
                let preamp = core
                    .ui_adjust_mic_preamp(AdjDirection::Up, amount.unwrap_or(1))
                    .await?;
                println!("Mic preamp set to {}", preamp);
                Ok(())
            }
            VolumeAction::Down { amount } => {
                let preamp = core
                    .ui_adjust_mic_preamp(AdjDirection::Down, amount.unwrap_or(1))
                    .await?;
                println!("Mic preamp set to {}", preamp);
                Ok(())
            }
        },
        UiAction::AudioMode { action } => match action.unwrap_or_default() {
            AudioModeAction::Get => {
                let mode = core.ui_get_audio_mode().await?;
                println!("{}", mode);
                Ok(())
            }
            AudioModeAction::Ctl => {
                core.ui_set_audio_mode(AudioMode::Ctl).await?;
                println!("Audio mode: ctl");
                Ok(())
            }
            AudioModeAction::Net => {
                core.ui_set_audio_mode(AudioMode::Net).await?;
                println!("Audio mode: net");
                Ok(())
            }
        },
    }
}

/// Handle audio capture and playback commands.
pub async fn handle_audio(
    action: AudioAction,
    core: &mut Core,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        AudioAction::Capture { mode } => match mode {
            CaptureMode::Live => capture_live(core).await,
            CaptureMode::Wav { basename } => capture_wav(core, &basename).await,
        },
        AudioAction::Play { mode } => match mode {
            PlayMode::Wav { file } => play_wav(core, &file).await,
            PlayMode::Live => play_live(core).await,
        },
    }
}

/// AudioSink that sends samples to a cpal output stream via channel.
struct CpalSink {
    tx: mpsc::Sender<Vec<i16>>,
}

impl AudioSink for CpalSink {
    fn write_samples(&mut self, samples: &[i16]) {
        let _ = self.tx.send(samples.to_vec());
    }
}

/// Capture audio from the UI chip and play it through the computer speakers.
async fn capture_live(core: &mut Core) -> Result<(), Box<dyn std::error::Error>> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use rubato::{FftFixedIn, Resampler};
    use std::sync::{Arc, Mutex};

    println!("Pinging ui until its ready");
    match ping_ui_until_pong(core).await {
        Ok(()) => {}
        Err(err) => {
            println!("Failed to ping ui");
            return Err(err);
        }
    }

    // Get the SFrame key from UI chip
    println!("Reading SFrame key from UI chip...");
    let sframe_key = core.get_sframe_key().await?;
    println!("SFrame key: {}", hex::encode(&sframe_key));

    // Set audio mode to CTL
    println!("Setting audio mode to CTL...");
    core.ui_set_audio_mode(AudioMode::Ctl).await?;

    // Create capture session
    let mut session = CaptureSession::new(&sframe_key);

    // Set up cpal audio output
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or("No audio output device found")?;
    println!("Audio output device: {}", device.name()?);

    // Find a supported output configuration - prefer 48kHz for easy upsampling from 8kHz
    let supported_configs: Vec<_> = device.supported_output_configs()?.collect();
    let mut selected_config = None;

    // First try to find 48kHz mono
    for config in &supported_configs {
        if config.channels() == 1
            && config.min_sample_rate().0 <= 48000
            && config.max_sample_rate().0 >= 48000
        {
            selected_config = Some(config.clone().with_sample_rate(cpal::SampleRate(48000)));
            break;
        }
    }

    // Fall back to any mono config at 48kHz or 44.1kHz
    if selected_config.is_none() {
        for config in &supported_configs {
            if config.channels() == 1 {
                let rate = if config.min_sample_rate().0 <= 48000
                    && config.max_sample_rate().0 >= 48000
                {
                    48000
                } else if config.min_sample_rate().0 <= 44100 && config.max_sample_rate().0 >= 44100
                {
                    44100
                } else {
                    config.max_sample_rate().0
                };
                selected_config = Some(config.clone().with_sample_rate(cpal::SampleRate(rate)));
                break;
            }
        }
    }

    // Fall back to stereo if no mono available
    if selected_config.is_none() {
        for config in &supported_configs {
            if config.channels() == 2 {
                let rate = if config.min_sample_rate().0 <= 48000
                    && config.max_sample_rate().0 >= 48000
                {
                    48000
                } else if config.min_sample_rate().0 <= 44100 && config.max_sample_rate().0 >= 44100
                {
                    44100
                } else {
                    config.max_sample_rate().0
                };
                selected_config = Some(config.clone().with_sample_rate(cpal::SampleRate(rate)));
                break;
            }
        }
    }

    let supported = selected_config.ok_or("No supported audio output configuration found")?;
    let sample_rate = supported.sample_rate().0;
    let channels = supported.channels() as usize;
    let sample_format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();

    println!(
        "Output format: {}Hz, {} channel(s), {:?}",
        sample_rate, channels, sample_format
    );

    // Create rubato resampler: 8kHz -> output rate
    // Use FftFixedIn which is efficient and high quality
    let resampler = FftFixedIn::<f32>::new(8000, sample_rate as usize, 160, 2, 1)
        .map_err(|e| format!("Failed to create resampler: {}", e))?;

    // Shared state between capture thread and audio callback
    let resampler = Arc::new(Mutex::new(resampler));
    let resampled_buffer: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));

    // Create channel for sending samples from capture to resampler
    let (tx, rx) = mpsc::channel::<Vec<i16>>();
    let sink = CpalSink { tx };

    // Clone for the resampling thread
    let resampler_clone = Arc::clone(&resampler);
    let buffer_clone = Arc::clone(&resampled_buffer);

    // Spawn a thread to handle resampling (rubato isn't real-time safe in audio callback)
    std::thread::spawn(move || {
        let mut input_buffer: Vec<f32> = Vec::new();

        while let Ok(samples) = rx.recv() {
            // Convert i16 to f32 and accumulate
            for sample in samples {
                input_buffer.push(sample as f32 / 32768.0);
            }

            // Process when we have enough samples (160 = one frame at 8kHz)
            let mut resampler = resampler_clone.lock().unwrap();
            while input_buffer.len() >= 160 {
                let chunk: Vec<f32> = input_buffer.drain(..160).collect();
                let input = vec![chunk];

                if let Ok(output) = resampler.process(&input, None) {
                    if !output.is_empty() {
                        let mut buffer = buffer_clone.lock().unwrap();
                        buffer.extend_from_slice(&output[0]);
                    }
                }
            }
        }
    });

    let buffer_for_callback = Arc::clone(&resampled_buffer);

    // Build the output stream
    let stream = device.build_output_stream(
        &config,
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            let mut buffer = buffer_for_callback.lock().unwrap();
            let samples_per_channel = data.len() / channels;
            let available = buffer.len().min(samples_per_channel);

            let mut idx = 0;
            for i in 0..available {
                let sample = buffer[i];
                data[idx] = sample;
                idx += 1;
                // For stereo, duplicate to both channels
                if channels == 2 {
                    data[idx] = sample;
                    idx += 1;
                }
            }
            // Remove consumed samples
            buffer.drain(..available);

            // Fill remaining with silence
            for sample in &mut data[idx..] {
                *sample = 0.0;
            }
        },
        |err| eprintln!("Audio stream error: {}", err),
        None,
    )?;

    stream.play()?;

    println!("\nCapturing audio (press and hold PTT button to talk)...");
    println!("Press ESC to stop.\n");

    // Set a short timeout for non-blocking reads
    if let Err(e) = core
        .port_mut()
        .set_timeout(Duration::from_millis(timeouts::MONITOR_MS))
    {
        eprintln!("Warning: couldn't set timeout: {}", e);
    }

    // Use crossterm for ESC detection
    use crossterm::event::{self, Event, KeyCode, KeyEvent};
    use crossterm::terminal;

    terminal::enable_raw_mode()?;

    let mut sink = sink;
    let mut capturing = false;
    let mut frame_count = 0u32;

    let result = async {
        loop {
            // Check for key press (non-blocking)
            if event::poll(Duration::from_millis(0))? {
                if let Event::Key(KeyEvent {
                    code: KeyCode::Esc, ..
                }) = event::read()?
                {
                    return Ok::<(), Box<dyn std::error::Error>>(());
                }
            }

            // Try to read a TLV from UI
            match core.try_read_tlv_ui().await {
                Ok(Some(tlv)) => {
                    match tlv.tlv_type {
                        UiToCtl::AudioStart => {
                            if !capturing {
                                capturing = true;
                                frame_count = 0;
                                print!("\r[CAPTURING] ");
                                std::io::stdout().flush().ok();
                            }
                        }
                        UiToCtl::AudioEnd => {
                            if capturing {
                                capturing = false;
                                print!("\r[IDLE] {} frames captured\r\n", frame_count);
                                std::io::stdout().flush().ok();
                            }
                        }
                        UiToCtl::AudioFrame => {
                            if capturing {
                                match session.process_frame(&tlv.value, &mut sink) {
                                    Ok(true) => {
                                        frame_count += 1;
                                        print!("\r[CAPTURING] {} frames", frame_count);
                                        std::io::stdout().flush().ok();
                                    }
                                    Ok(false) => {
                                        // Invalid frame, skip
                                    }
                                    Err(e) => {
                                        print!("\r[ERROR] {}\r\n", e);
                                        std::io::stdout().flush().ok();
                                    }
                                }
                            }
                        }
                        UiToCtl::Log => {
                            // Print log messages
                            if let Ok(msg) = core::str::from_utf8(&tlv.value) {
                                print!("\r[UI] {}\r\n", msg);
                                std::io::stdout().flush().ok();
                            }
                        }
                        _ => {
                            // Ignore other TLVs
                        }
                    }
                }
                Ok(None) => {
                    // Timeout, continue
                }
                Err(e) => {
                    if e.is_timeout() {
                        continue;
                    }
                    return Err(format!("Read error: {:?}", e).into());
                }
            }
        }
    }
    .await;

    // Cleanup
    terminal::disable_raw_mode()?;
    drop(stream);

    // Restore audio mode to NET
    println!("\nRestoring audio mode to NET...");
    core.ui_set_audio_mode(AudioMode::Net).await?;

    // Restore timeout
    if let Err(e) = core
        .port_mut()
        .set_timeout(Duration::from_secs(timeouts::NORMAL_SECS))
    {
        eprintln!("Warning: couldn't restore timeout: {}", e);
    }

    println!("Capture stopped.");

    result
}

/// AudioSink that collects samples into a Vec.
struct VecSink {
    samples: Vec<i16>,
}

impl AudioSink for VecSink {
    fn write_samples(&mut self, samples: &[i16]) {
        self.samples.extend_from_slice(samples);
    }
}

/// Save samples to a numbered WAV file.
fn save_wav_file(
    basename: &str,
    file_number: u32,
    samples: &[i16],
) -> Result<String, Box<dyn std::error::Error>> {
    let filename = format!("{}_{:03}.wav", basename, file_number);

    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 8000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };

    let mut writer = hound::WavWriter::create(&filename, spec)?;
    for sample in samples {
        writer.write_sample(*sample)?;
    }
    writer.finalize()?;

    Ok(filename)
}

async fn ping_ui_until_pong(core: &mut Core) -> Result<(), Box<dyn std::error::Error>> {
    let original_timeout = core.port_mut().timeout();
    let ping_data = b"hello";

    // Need to wait for net to stop dumping logs
    core.port_mut().set_timeout(Duration::from_millis(1000))?;

    for _ in 0..10 {
        match core.ui_ping(ping_data).await {
            Ok(()) => {
                core.port_mut().set_timeout(original_timeout)?;
                return Ok(());
            }
            Err(err) => {
                println!("Ping failed {err}");
                continue;
            }
        }
    }

    core.port_mut().set_timeout(original_timeout)?;
    Err("Ui did not respond to ping after 10 attempts".into())
}

/// Capture audio from the UI chip and save to numbered WAV files.
/// Each PTT press creates a new file: basename_001.wav, basename_002.wav, etc.
async fn capture_wav(core: &mut Core, basename: &str) -> Result<(), Box<dyn std::error::Error>> {
    // Ping the ui until it responds or times out.
    println!("Pinging ui until its ready");
    match ping_ui_until_pong(core).await {
        Ok(()) => {}
        Err(err) => {
            println!("Failed to ping ui");
            return Err(err);
        }
    }

    // Get the SFrame key from UI chip
    println!("Reading SFrame key from UI chip...");
    let sframe_key = core.get_sframe_key().await?;
    println!("SFrame key: {}", hex::encode(&sframe_key));

    // Set audio mode to CTL
    println!("Setting audio mode to CTL...");
    core.ui_set_audio_mode(AudioMode::Ctl).await?;

    // Create capture session
    let mut session = CaptureSession::new(&sframe_key);

    // Create sink to collect samples for current recording
    let mut sink = VecSink {
        samples: Vec::new(),
    };

    println!(
        "\nCapturing audio to {}_XXX.wav (press and hold PTT button to talk)...",
        basename
    );
    println!("Press ESC to stop.\n");

    // Set a short timeout for non-blocking reads
    if let Err(e) = core
        .port_mut()
        .set_timeout(Duration::from_millis(timeouts::MONITOR_MS))
    {
        eprintln!("Warning: couldn't set timeout: {}", e);
    }

    // Use crossterm for ESC detection
    use crossterm::event::{self, Event, KeyCode, KeyEvent};
    use crossterm::terminal;

    terminal::enable_raw_mode()?;

    let mut capturing = false;
    let mut frame_count = 0u32;
    let mut file_number = 0u32;
    let mut files_saved = Vec::new();

    let result = async {
        loop {
            // Check for key press (non-blocking)
            if event::poll(Duration::from_millis(0))? {
                if let Event::Key(KeyEvent {
                    code: KeyCode::Esc, ..
                }) = event::read()?
                {
                    return Ok::<(), Box<dyn std::error::Error>>(());
                }
            }

            // Try to read a TLV from UI
            match core.try_read_tlv_ui().await {
                Ok(Some(tlv)) => {
                    match tlv.tlv_type {
                        UiToCtl::AudioStart => {
                            if !capturing {
                                capturing = true;
                                frame_count = 0;
                                sink.samples.clear();
                                file_number += 1;
                                print!("\r[RECORDING #{}] ", file_number);
                                std::io::stdout().flush().ok();
                            }
                        }
                        UiToCtl::AudioEnd => {
                            if capturing {
                                capturing = false;

                                // Save the recorded audio to a file
                                if !sink.samples.is_empty() {
                                    match save_wav_file(basename, file_number, &sink.samples) {
                                        Ok(filename) => {
                                            let duration_secs = sink.samples.len() as f64 / 8000.0;
                                            print!(
                                                "\r[SAVED] {} ({:.2}s, {} samples)\r\n",
                                                filename,
                                                duration_secs,
                                                sink.samples.len()
                                            );
                                            files_saved.push(filename);
                                        }
                                        Err(e) => {
                                            print!("\r[ERROR] Failed to save: {}\r\n", e);
                                        }
                                    }
                                } else {
                                    print!("\r[IDLE] No audio captured\r\n");
                                }
                                std::io::stdout().flush().ok();
                            }
                        }
                        UiToCtl::AudioFrame => {
                            if capturing {
                                match session.process_frame(&tlv.value, &mut sink) {
                                    Ok(true) => {
                                        frame_count += 1;
                                        print!(
                                            "\r[RECORDING #{}] {} frames, {} samples",
                                            file_number,
                                            frame_count,
                                            sink.samples.len()
                                        );
                                        std::io::stdout().flush().ok();
                                    }
                                    Ok(false) => {
                                        print!(
                                            "\r[SKIP] invalid frame ({} bytes)\r\n",
                                            tlv.value.len()
                                        );
                                        std::io::stdout().flush().ok();
                                    }
                                    Err(e) => {
                                        print!("\r[ERROR] {}\r\n", e);
                                        std::io::stdout().flush().ok();
                                    }
                                }
                            }
                        }
                        UiToCtl::Log => {
                            // Print log messages
                            if let Ok(msg) = core::str::from_utf8(&tlv.value) {
                                print!("\r[UI] {}\r\n", msg);
                                std::io::stdout().flush().ok();
                            }
                        }
                        _ => {
                            // Ignore other TLVs
                        }
                    }
                }
                Ok(None) => {
                    // Timeout, continue
                }
                Err(e) => {
                    if e.is_timeout() {
                        continue;
                    }
                    return Err(format!("Read error: {:?}", e).into());
                }
            }
        }
    }
    .await;

    // Cleanup
    terminal::disable_raw_mode()?;

    // Restore audio mode to NET
    println!("\nRestoring audio mode to NET...");
    core.ui_set_audio_mode(AudioMode::Net).await?;

    // Restore timeout
    if let Err(e) = core
        .port_mut()
        .set_timeout(Duration::from_secs(timeouts::NORMAL_SECS))
    {
        eprintln!("Warning: couldn't restore timeout: {}", e);
    }

    // Summary
    if files_saved.is_empty() {
        println!("No files saved.");
    } else {
        println!("Saved {} file(s):", files_saved.len());
        for f in &files_saved {
            println!("  {}", f);
        }
    }

    result
}

/// Play a WAV file to the UI chip speaker.
async fn play_wav(
    core: &mut Core,
    path: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    // Read and validate WAV file
    println!("Reading WAV file: {}", path.display());
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();

    if spec.channels != 1 {
        return Err(format!("WAV file must be mono, got {} channels", spec.channels).into());
    }
    if spec.sample_rate != 8000 {
        return Err(format!("WAV file must be 8kHz, got {} Hz", spec.sample_rate).into());
    }
    if spec.bits_per_sample != 16 {
        return Err(format!("WAV file must be 16-bit, got {} bits", spec.bits_per_sample).into());
    }
    if spec.sample_format != hound::SampleFormat::Int {
        return Err("WAV file must use integer sample format".into());
    }

    // Read all samples
    let samples: Vec<i16> = reader.samples::<i16>().collect::<Result<Vec<_>, _>>()?;

    let duration_secs = samples.len() as f64 / 8000.0;
    println!(
        "Loaded {:.2} seconds of audio ({} samples)",
        duration_secs,
        samples.len()
    );

    println!("Pinging ui until its ready");
    match ping_ui_until_pong(core).await {
        Ok(()) => {}
        Err(err) => {
            println!("Failed to ping ui");
            return Err(err);
        }
    }

    // Get the SFrame key from UI chip
    println!("Reading SFrame key from UI chip...");
    let sframe_key = core.get_sframe_key().await?;
    println!("SFrame key: {}", hex::encode(&sframe_key));

    // Set audio mode to CTL
    println!("Setting audio mode to CTL...");
    core.ui_set_audio_mode(AudioMode::Ctl).await?;

    // Create playback session
    let mut session = PlaybackSession::new(&sframe_key);

    // Chunk size - 160 samples = 20ms at 8kHz (standard for telephony)
    const CHUNK_SIZE: usize = 160;
    const CHANNEL_ID: u8 = 0;

    let total_chunks = (samples.len() + CHUNK_SIZE - 1) / CHUNK_SIZE;
    println!("\nPlaying {} chunks to device...", total_chunks);

    // Send audio start
    core.ui_send_audio_start().await?;

    // Send audio frames
    for (i, chunk) in samples.chunks(CHUNK_SIZE).enumerate() {
        let frame = session.create_frame(chunk, CHANNEL_ID)?;
        core.ui_send_audio_frame(&frame).await?;

        // Progress
        if (i + 1) % 50 == 0 || i + 1 == total_chunks {
            let progress = (i + 1) as f64 / total_chunks as f64 * 100.0;
            print!("\rProgress: {:.0}% ({}/{})", progress, i + 1, total_chunks);
            std::io::stdout().flush().ok();
        }

        // Pace the sending to roughly match real-time (20ms per chunk)
        // This prevents overwhelming the device with too much data at once
        tokio::time::sleep(Duration::from_millis(18)).await;
    }

    // Send audio end
    core.ui_send_audio_end().await?;

    println!("\n\nRestoring audio mode to NET...");
    core.ui_set_audio_mode(AudioMode::Net).await?;

    println!("Playback complete.");
    Ok(())
}

/// Stream audio from computer microphone to the UI chip speaker.
async fn play_live(core: &mut Core) -> Result<(), Box<dyn std::error::Error>> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use rubato::{FftFixedIn, Resampler};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    println!("Pinging ui until its ready");
    match ping_ui_until_pong(core).await {
        Ok(()) => {}
        Err(err) => {
            println!("Failed to ping ui");
            return Err(err);
        }
    }

    // Get the SFrame key from UI chip
    println!("Reading SFrame key from UI chip...");
    let sframe_key = core.get_sframe_key().await?;
    println!("SFrame key: {}", hex::encode(&sframe_key));

    // Set audio mode to CTL
    println!("Setting audio mode to CTL...");
    core.ui_set_audio_mode(AudioMode::Ctl).await?;

    // Create playback session
    let mut session = PlaybackSession::new(&sframe_key);

    // Set up cpal audio input
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or("No audio input device found")?;
    println!("Audio input device: {}", device.name()?);

    // Create channel for receiving samples from the input callback
    let (tx, rx) = mpsc::channel::<Vec<f32>>();

    // Find a supported input configuration - use f32 for rubato compatibility
    let supported_configs: Vec<_> = device.supported_input_configs()?.collect();
    let mut selected_config = None;

    // First try to find mono config at 48kHz or 44.1kHz
    for config in &supported_configs {
        if config.channels() == 1 {
            let rate = if config.min_sample_rate().0 <= 48000 && config.max_sample_rate().0 >= 48000
            {
                48000
            } else if config.min_sample_rate().0 <= 44100 && config.max_sample_rate().0 >= 44100 {
                44100
            } else {
                config.max_sample_rate().0
            };
            selected_config = Some(config.clone().with_sample_rate(cpal::SampleRate(rate)));
            break;
        }
    }

    // Fall back to stereo if no mono available
    if selected_config.is_none() {
        for config in &supported_configs {
            if config.channels() == 2 {
                let rate = if config.min_sample_rate().0 <= 48000
                    && config.max_sample_rate().0 >= 48000
                {
                    48000
                } else if config.min_sample_rate().0 <= 44100 && config.max_sample_rate().0 >= 44100
                {
                    44100
                } else {
                    config.max_sample_rate().0
                };
                selected_config = Some(config.clone().with_sample_rate(cpal::SampleRate(rate)));
                break;
            }
        }
    }

    let supported = selected_config.ok_or("No supported audio input configuration found")?;
    let sample_rate = supported.sample_rate().0 as usize;
    let channels = supported.channels() as usize;
    let config: cpal::StreamConfig = supported.into();

    println!("Input format: {}Hz, {} channel(s)", sample_rate, channels);

    // Create rubato resampler: input rate -> 8kHz
    // chunk_size chosen to give ~160 output samples (one frame) per process call
    let input_chunk_size = (sample_rate * 160) / 8000; // e.g., 960 for 48kHz
    let mut resampler = FftFixedIn::<f32>::new(sample_rate, 8000, input_chunk_size, 2, 1)
        .map_err(|e| format!("Failed to create resampler: {}", e))?;

    // Flag to stop the stream
    let running = Arc::new(AtomicBool::new(true));
    let running_clone = running.clone();

    // Build the input stream with f32 samples
    let stream = device.build_input_stream(
        &config,
        move |data: &[f32], _: &cpal::InputCallbackInfo| {
            if running_clone.load(Ordering::Relaxed) {
                let _ = tx.send(data.to_vec());
            }
        },
        |err| eprintln!("Audio stream error: {}", err),
        None,
    )?;

    stream.play()?;

    println!("\nStreaming from microphone to device...");
    println!("Press ESC to stop.\n");

    // Use crossterm for ESC detection
    use crossterm::event::{self, Event, KeyCode, KeyEvent};
    use crossterm::terminal;

    terminal::enable_raw_mode()?;

    // Chunk size - 160 samples = 20ms at 8kHz
    const CHUNK_SIZE: usize = 160;
    const CHANNEL_ID: u8 = 0;

    let mut input_buffer: Vec<f32> = Vec::new();
    let mut output_buffer: Vec<i16> = Vec::new();
    let mut frame_count = 0u32;
    let mut streaming = false;

    let result = async {
        loop {
            // Check for key press (non-blocking)
            if event::poll(Duration::from_millis(0))? {
                if let Event::Key(KeyEvent {
                    code: KeyCode::Esc, ..
                }) = event::read()?
                {
                    return Ok::<(), Box<dyn std::error::Error>>(());
                }
            }

            // Try to receive samples from the input stream
            while let Ok(samples) = rx.try_recv() {
                if !streaming {
                    streaming = true;
                    core.ui_send_audio_start().await?;
                    print!("\r[STREAMING] ");
                    std::io::stdout().flush().ok();
                }

                // Mix stereo to mono if needed, accumulate into input buffer
                for chunk in samples.chunks(channels) {
                    let sample = if channels == 2 {
                        (chunk[0] + chunk[1]) / 2.0
                    } else {
                        chunk[0]
                    };
                    input_buffer.push(sample);
                }

                // Process through resampler when we have enough samples
                while input_buffer.len() >= input_chunk_size {
                    let chunk: Vec<f32> = input_buffer.drain(..input_chunk_size).collect();
                    let input = vec![chunk];

                    if let Ok(output) = resampler.process(&input, None) {
                        if !output.is_empty() {
                            // Convert f32 to i16
                            for sample in &output[0] {
                                let s = (*sample * 32767.0).clamp(-32768.0, 32767.0) as i16;
                                output_buffer.push(s);
                            }
                        }
                    }
                }

                // Send complete chunks
                while output_buffer.len() >= CHUNK_SIZE {
                    let chunk: Vec<i16> = output_buffer.drain(..CHUNK_SIZE).collect();
                    let frame = session.create_frame(&chunk, CHANNEL_ID)?;
                    core.ui_send_audio_frame(&frame).await?;
                    frame_count += 1;

                    if frame_count % 50 == 0 {
                        print!("\r[STREAMING] {} frames", frame_count);
                        std::io::stdout().flush().ok();
                    }
                }
            }

            // Small sleep to prevent busy-waiting
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
    .await;

    // Cleanup
    running.store(false, Ordering::Relaxed);
    terminal::disable_raw_mode()?;
    drop(stream);

    if streaming {
        core.ui_send_audio_end().await?;
    }

    // Restore audio mode to NET
    println!("\n\nRestoring audio mode to NET...");
    core.ui_set_audio_mode(AudioMode::Net).await?;

    println!("Streaming stopped. {} frames sent.", frame_count);

    result
}

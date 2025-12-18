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
    time::Hertz,
    usart::{self, Config, DataBits, Parity, StopBits, Uart},
    Peri,
};
use embassy_time::Delay;
use embedded_hal::delay::DelayNs;
use embedded_hal::i2c::I2c as I2cTrait;
use link::ui::{AudioError, AudioSystem, Frame, FRAME_SIZE};
use {defmt_rtt as _, panic_probe as _};

// =============================================================================
// WM8960 Codec Control
// =============================================================================

const WM8960_I2C_ADDR: u8 = 0x1a;

/// WM8960 codec control helper.
struct Wm8960Codec<'a, I> {
    i2c: &'a mut I,
    regs: Wm8960Registers,
}

impl<'a, I: I2cTrait> Wm8960Codec<'a, I> {
    fn new(i2c: &'a mut I) -> Self {
        Self {
            i2c,
            regs: Wm8960Registers::default(),
        }
    }

    /// Initialize the audio codec with default settings.
    fn init(&mut self, delay: &mut impl DelayNs) {
        self.power_on(delay);
        delay.delay_ms(100);

        self.left_input_path(true);
        delay.delay_ms(100);
        self.left_output_path(true);
        delay.delay_ms(100);
        self.left_adc(true);
        delay.delay_ms(100);
        self.left_dac(true);
        delay.delay_ms(100);
        self.configure_dac(true, true, false);
        delay.delay_ms(100);
        self.enable_i2s();
        delay.delay_ms(100);
    }

    fn power_on(&mut self, delay: &mut impl DelayNs) {
        const RESET_SIGNAL: [u8; 2] = [0x1e, 0x00];
        let _ = self.i2c.write(WM8960_I2C_ADDR, &RESET_SIGNAL);

        // Wait for reset to complete before writing any registers
        delay.delay_ms(100);

        self.regs.modify(&mut self.i2c, |r| {
            r.set(PowerMgmt1VrefEnable(true));
        });

        delay.delay_ms(100);

        self.regs.modify(&mut self.i2c, |r| {
            r.set(PowerMgmt1VmidSelect(0b01));
        });

        delay.delay_ms(100);

        self.regs.modify(&mut self.i2c, |r| {
            r.set(MicrophoneBiasEnable(true));
        });
    }

    fn left_input_path(&mut self, enable: bool) {
        self.regs.modify(&mut self.i2c, |r| {
            r.set(PowerMgmt1AinLeftEnable(enable));
            r.set(LeftMicEnable(enable));
            r.set(Linput3Boost(0b000));
            r.set(LeftInput3ToOutputMixer(false));
            r.set(LeftInput3ToOutputMixerVolume(0b000));
            r.set(LeftInput3ToNonInverting(false));
            r.set(RightInputAnalogMute(true));
            r.set(Rinput2Boost(0b000));
            r.set(Rinput3Boost(0b000));
            r.set(RightInput3ToOutputMixer(false));
            r.set(RightInput3ToOutputMixerVolume(0b000));
            r.set(LeftInput1ToInverting(true));
            r.set(LeftInput2ToNonInverting(true));
            r.set(LeftInputToBoost(true));
            r.set(LeftInputAnalogMute(false));
            r.set(Linput2Boost(0b000));
            r.set(LeftBoostGain(0b00));
            r.set(InputPgaVolumeUpdate(true));
            r.set(LeftPgaVolume(0b01_0111));
        });
    }

    fn left_output_path(&mut self, enable: bool) {
        self.regs.modify(&mut self.i2c, |r| {
            r.set(LeftOutput1Enable(enable));
            r.set(LeftOutputMixEnable(enable));
            r.set(LeftInput3ToOutputMixer(false));
            r.set(LeftInput3ToOutputMixerVolume(0b000));
            r.set(LeftSpeakerVolumeUpdate(true));
            r.set(LeftSpeakerVolume(0b000_0000));
            r.set(HeadphoneOutVolumeUpdate(true));
            r.set(LeftHeadphoneVolume(0b111_1111));
        });
    }

    fn right_output_path(&mut self, enable: bool) {
        self.regs.modify(&mut self.i2c, |r| {
            r.set(RightOutput1Enable(enable));
            r.set(RightOutputMixEnable(enable));
            r.set(RightInput3ToOutputMixer(false));
            r.set(RightInput3ToOutputMixerVolume(0b000));
            r.set(RightSpeakerVolumeUpdate(true));
            r.set(RightSpeakerVolume(0b000_0000));
            r.set(HeadphoneOutVolumeUpdateRight(true));
            r.set(RightHeadphoneVolume(0b111_1111));
        });
    }

    fn enable_i2s(&mut self) {
        self.regs.modify(&mut self.i2c, |r| {
            r.set(AudioInterfaceMasterMode(true));
            r.set(AudioWordLength(0b00));
            r.set(AudioFormat(0b10));
            r.set(PllEnable(true));
            r.set(MasterClockDisable(false));
            r.set(PllN(0b1000));
            r.set(PllKMsb(0b0011_0001));
            r.set(PllKMid(0b0010_0110));
            r.set(PllKLsb(0b1110_1001));
            r.set(Adc1Divider(0b110));
            r.set(DacDivider(0b110));
            r.set(SysClkDiv(0b00));
            r.set(ClockSelect(true));
            r.set(BclkFrequency(0b1100));
            r.set(ClassDSysclkDivider(0b111));
            r.set(AdcAlcSampleRateSelect(0b101));
        });
    }

    fn left_dac(&mut self, enable: bool) {
        self.regs.modify(&mut self.i2c, |r| {
            r.set(LeftDacEnable(enable));
            r.set(LeftDacToOutputMixer(enable));
            r.set(DacVolumeUpdate(true));
            r.set(LeftDacDigitalVolume(0b1111_1111));
        });
    }

    fn right_dac(&mut self, enable: bool) {
        self.regs.modify(&mut self.i2c, |r| {
            r.set(RightDacEnable(enable));
            r.set(RightDacToOutputMixer(enable));
            r.set(DacVolumeUpdateRight(true));
            r.set(RightDacDigitalVolume(0b1111_1111));
        });
    }

    fn left_adc(&mut self, enable: bool) {
        self.regs.modify(&mut self.i2c, |r| {
            r.set(PowerMgmt1EnableAdcLeft(enable));
            r.set(AdcHighPassDisable(true));
        });
    }

    fn configure_dac(&mut self, mono_mix: bool, soft_mute_mode: bool, mute: bool) {
        self.regs.modify(&mut self.i2c, |r| {
            r.set(DacMonoMix(mono_mix));
            r.set(DacSoftMuteMode(soft_mute_mode));
            r.set(DacSoftMuteEnable(mute));
        });
    }

    fn enable_input(&mut self, enabled: bool) {
        self.left_input_path(enabled);
        self.left_adc(enabled);
    }

    fn enable_output(&mut self, enabled: bool) {
        self.left_output_path(enabled);
        self.right_output_path(enabled);
        self.left_dac(enabled);
        self.right_dac(enabled);
    }
}

// WM8960 Register infrastructure
type RegAddr = u8;

struct Wm8960Registers {
    regs: [u16; 56],
}

impl Default for Wm8960Registers {
    fn default() -> Self {
        let init: [(u8, u16); 56] = [
            (0x00, 0b0_1001_0111),
            (0x01, 0b0_1001_0111),
            (0x02, 0b0_0000_0000),
            (0x03, 0b0_0000_0000),
            (0x04, 0b0_0000_0000),
            (0x05, 0b0_0000_1000),
            (0x06, 0b0_0000_0000),
            (0x07, 0b0_0000_1010),
            (0x08, 0b1_1100_0000),
            (0x09, 0b0_0000_0000),
            (0x0A, 0b0_1111_1111),
            (0x0B, 0b0_1111_1111),
            (0x0C, 0b0_0000_0000),
            (0x0D, 0b0_0000_0000),
            (0x0E, 0b0_0000_0000),
            (0x0F, 0b0_0000_0000),
            (0x10, 0b0_0000_0000),
            (0x11, 0b0_0111_1011),
            (0x12, 0b1_0000_0000),
            (0x13, 0b0_0011_0010),
            (0x14, 0b0_0000_0000),
            (0x15, 0b0_1100_0011),
            (0x16, 0b0_1100_0011),
            (0x17, 0b1_1100_0000),
            (0x18, 0b0_0000_0000),
            (0x19, 0b0_0000_0000),
            (0x1A, 0b0_0000_0000),
            (0x1B, 0b0_0000_0000),
            (0x1C, 0b0_0000_0000),
            (0x1D, 0b0_0000_0000),
            (0x1E, 0b0_0000_0000),
            (0x1F, 0b0_0000_0000),
            (0x20, 0b1_0000_0000),
            (0x21, 0b1_0000_0000),
            (0x22, 0b0_0101_0000),
            (0x23, 0b0_0101_0000),
            (0x24, 0b0_0101_0000),
            (0x25, 0b0_0101_0000),
            (0x26, 0b0_0000_0000),
            (0x27, 0b0_0000_0000),
            (0x28, 0b0_0000_0000),
            (0x29, 0b0_0000_0000),
            (0x2A, 0b0_0100_0000),
            (0x2B, 0b0_0000_0000),
            (0x2C, 0b0_0000_0000),
            (0x2D, 0b0_0101_0000),
            (0x2E, 0b0_0101_0000),
            (0x2F, 0b0_0000_0000),
            (0x30, 0b0_0000_0010),
            (0x31, 0b0_0011_0111),
            (0x32, 0b0_0100_1101),
            (0x33, 0b0_1000_0000),
            (0x34, 0b0_0000_1000),
            (0x35, 0b0_0011_0001),
            (0x36, 0b0_0010_0110),
            (0x37, 0b0_1110_1001),
        ];
        let mut regs = [0u16; 56];
        for (i, (addr, val)) in init.iter().enumerate() {
            regs[i] = ((*addr as u16) << 9) | (val & 0x01FF);
        }
        Self { regs }
    }
}

impl Wm8960Registers {
    fn modify<I, F>(&mut self, i2c: &mut I, f: F)
    where
        I: I2cTrait,
        F: FnOnce(&mut RegisterView),
    {
        let mut r = RegisterView::new(&mut self.regs);
        f(&mut r);
        for i in r
            .modified
            .iter()
            .enumerate()
            .filter_map(|(i, m)| m.then_some(i))
        {
            let _ = i2c.write(WM8960_I2C_ADDR, &self.regs[i].to_be_bytes());
        }
    }
}

struct RegisterView<'a> {
    regs: &'a mut [u16; 56],
    modified: [bool; 56],
}

impl<'a> RegisterView<'a> {
    const fn new(regs: &'a mut [u16; 56]) -> Self {
        Self {
            regs,
            modified: [false; 56],
        }
    }

    fn set<F: FieldAccess>(&mut self, val: F) {
        let idx: usize = F::ADDR.into();
        let old = self.regs[idx];
        let new = val.set(old);
        if new != old {
            self.regs[idx] = new;
            self.modified[idx] = true;
        }
    }
}

trait FieldAccess {
    const ADDR: RegAddr;
    const OFFSET: u8;
    const WIDTH: u8;
    type Value;
    fn set(&self, regval: u16) -> u16;
}

macro_rules! define_field {
    ($name:ident, $addr:expr, $offset:expr, $width:expr, $val:ty) => {
        struct $name($val);
        impl FieldAccess for $name {
            const ADDR: RegAddr = $addr;
            const OFFSET: u8 = $offset;
            const WIDTH: u8 = $width;
            type Value = $val;
            #[inline]
            fn set(&self, regval: u16) -> u16 {
                let val: u16 = self.0.into();
                let mask = ((1u16 << Self::WIDTH) - 1) << Self::OFFSET;
                (regval & !mask) | ((val << Self::OFFSET) & mask)
            }
        }
    };
}

// R0 (0x00) Left Input Volume
define_field!(InputPgaVolumeUpdate, 0x00, 8, 1, bool);
define_field!(LeftInputAnalogMute, 0x00, 7, 1, bool);
define_field!(LeftPgaVolume, 0x00, 0, 6, u8);

// R1 (0x01) Right Input Volume
define_field!(RightInputAnalogMute, 0x01, 7, 1, bool);

// R2 (0x02) LOUT1 volume
define_field!(HeadphoneOutVolumeUpdate, 0x02, 8, 1, bool);
define_field!(LeftHeadphoneVolume, 0x02, 0, 7, u8);

// R3 (0x03) ROUT1 volume
define_field!(HeadphoneOutVolumeUpdateRight, 0x03, 8, 1, bool);
define_field!(RightHeadphoneVolume, 0x03, 0, 7, u8);

// R4 (0x04) Clocking (1)
define_field!(Adc1Divider, 0x04, 6, 3, u8);
define_field!(DacDivider, 0x04, 3, 3, u8);
define_field!(SysClkDiv, 0x04, 1, 2, u8);
define_field!(ClockSelect, 0x04, 0, 1, bool);

// R5 (0x05) ADC & DAC Control (CTR1)
define_field!(DacSoftMuteEnable, 0x05, 3, 1, bool);
define_field!(AdcHighPassDisable, 0x05, 0, 1, bool);

// R6 (0x06) ADC & DAC Control (CTR2)
define_field!(DacSoftMuteMode, 0x06, 3, 1, bool);

// R7 (0x07) Audio Interface
define_field!(AudioInterfaceMasterMode, 0x07, 6, 1, bool);
define_field!(AudioWordLength, 0x07, 2, 2, u8);
define_field!(AudioFormat, 0x07, 0, 2, u8);

// R8 (0x08) Clocking (2)
define_field!(ClassDSysclkDivider, 0x08, 6, 3, u8);
define_field!(BclkFrequency, 0x08, 0, 4, u8);

// R10 (0x0A) Left DAC Volume
define_field!(DacVolumeUpdate, 0x0A, 8, 1, bool);
define_field!(LeftDacDigitalVolume, 0x0A, 0, 8, u8);

// R11 (0x0B) Right DAC Volume
define_field!(DacVolumeUpdateRight, 0x0B, 8, 1, bool);
define_field!(RightDacDigitalVolume, 0x0B, 0, 8, u8);

// R23 (0x17) Additional Control (1)
define_field!(DacMonoMix, 0x17, 4, 1, bool);

// R25 (0x19) Power Management (1)
define_field!(PowerMgmt1VmidSelect, 0x19, 7, 2, u8);
define_field!(PowerMgmt1VrefEnable, 0x19, 6, 1, bool);
define_field!(PowerMgmt1AinLeftEnable, 0x19, 5, 1, bool);
define_field!(PowerMgmt1EnableAdcLeft, 0x19, 3, 1, bool);
define_field!(MicrophoneBiasEnable, 0x19, 1, 1, bool);
define_field!(MasterClockDisable, 0x19, 0, 1, bool);

// R26 (0x1A) Power Management (2)
define_field!(LeftDacEnable, 0x1A, 8, 1, bool);
define_field!(RightDacEnable, 0x1A, 7, 1, bool);
define_field!(LeftOutput1Enable, 0x1A, 6, 1, bool);
define_field!(RightOutput1Enable, 0x1A, 5, 1, bool);
define_field!(PllEnable, 0x1A, 0, 1, bool);

// R27 (0x1B) Additional Control (3)
define_field!(AdcAlcSampleRateSelect, 0x1B, 0, 3, u8);

// R32 (0x20) ADCL signal path
define_field!(LeftInput1ToInverting, 0x20, 8, 1, bool);
define_field!(LeftInput3ToNonInverting, 0x20, 7, 1, bool);
define_field!(LeftInput2ToNonInverting, 0x20, 6, 1, bool);
define_field!(LeftBoostGain, 0x20, 4, 2, u8);
define_field!(LeftInputToBoost, 0x20, 3, 1, bool);

// R34 (0x22) Left Out Mix (1)
define_field!(LeftDacToOutputMixer, 0x22, 8, 1, bool);
define_field!(LeftInput3ToOutputMixer, 0x22, 7, 1, bool);
define_field!(LeftInput3ToOutputMixerVolume, 0x22, 4, 3, u8);

// R37 (0x25) Right Out Mix (2)
define_field!(RightDacToOutputMixer, 0x25, 8, 1, bool);
define_field!(RightInput3ToOutputMixer, 0x25, 7, 1, bool);
define_field!(RightInput3ToOutputMixerVolume, 0x25, 4, 3, u8);

// R40 (0x28) LOUT2 volume
define_field!(LeftSpeakerVolumeUpdate, 0x28, 8, 1, bool);
define_field!(LeftSpeakerVolume, 0x28, 0, 7, u8);

// R41 (0x29) ROUT2 volume
define_field!(RightSpeakerVolumeUpdate, 0x29, 8, 1, bool);
define_field!(RightSpeakerVolume, 0x29, 0, 7, u8);

// R43 (0x2B) Input Boost Mixer (1)
define_field!(Linput3Boost, 0x2B, 4, 3, u8);
define_field!(Linput2Boost, 0x2B, 1, 3, u8);

// R44 (0x2C) Input Boost Mixer (2)
define_field!(Rinput3Boost, 0x2C, 4, 3, u8);
define_field!(Rinput2Boost, 0x2C, 1, 3, u8);

// R47 (0x2F) Power Management (3)
define_field!(LeftMicEnable, 0x2F, 5, 1, bool);
define_field!(LeftOutputMixEnable, 0x2F, 3, 1, bool);
define_field!(RightOutputMixEnable, 0x2F, 2, 1, bool);

// R52 (0x34) PLL N
define_field!(PllN, 0x34, 0, 4, u8);

// R53, R54, R55 (0x35, 0x36, 0x37) PLL K
define_field!(PllKMsb, 0x35, 0, 8, u8);
define_field!(PllKMid, 0x36, 0, 8, u8);
define_field!(PllKLsb, 0x37, 0, 8, u8);

// =============================================================================
// Board Audio System
// =============================================================================

const I2S_BUF_SIZE: usize = FRAME_SIZE * 2;

/// Raw peripherals needed to construct I2S.
struct I2sPeripherals<'d> {
    spi: Peri<'d, peripherals::SPI3>,
    ws: Peri<'d, peripherals::PA15>,
    ck: Peri<'d, peripherals::PC10>,
    sd_tx: Peri<'d, peripherals::PB5>,
    sd_rx: Peri<'d, peripherals::PB4>,
    dma_tx: Peri<'d, peripherals::DMA1_CH7>,
    tx_buf: &'d mut [u16; I2S_BUF_SIZE],
    dma_rx: Peri<'d, peripherals::DMA1_CH0>,
    rx_buf: &'d mut [u16; I2S_BUF_SIZE],
}

/// Audio system state: either uninitialized (holding peripherals) or ready (holding I2S).
enum AudioState<'d> {
    Uninitialized(Option<I2sPeripherals<'d>>),
    Ready(I2S<'d, u16>),
}

/// Board-level audio system with deferred I2S construction.
///
/// The I2S peripheral is constructed lazily in `init()`, ensuring the
/// WM8960 codec is fully configured before I2S clocks start.
pub struct BoardAudioSystem<'d> {
    state: AudioState<'d>,
}

impl<'d> BoardAudioSystem<'d> {
    pub fn new(
        spi: Peri<'d, peripherals::SPI3>,
        ws: Peri<'d, peripherals::PA15>,
        ck: Peri<'d, peripherals::PC10>,
        sd_tx: Peri<'d, peripherals::PB5>,
        sd_rx: Peri<'d, peripherals::PB4>,
        dma_tx: Peri<'d, peripherals::DMA1_CH7>,
        tx_buf: &'d mut [u16; I2S_BUF_SIZE],
        dma_rx: Peri<'d, peripherals::DMA1_CH0>,
        rx_buf: &'d mut [u16; I2S_BUF_SIZE],
    ) -> Self {
        Self {
            state: AudioState::Uninitialized(Some(I2sPeripherals {
                spi,
                ws,
                ck,
                sd_tx,
                sd_rx,
                dma_tx,
                tx_buf,
                dma_rx,
                rx_buf,
            })),
        }
    }

    fn i2s(&mut self) -> &mut I2S<'d, u16> {
        match &mut self.state {
            AudioState::Ready(i2s) => i2s,
            AudioState::Uninitialized(_) => panic!("audio system not initialized"),
        }
    }
}

impl<'d> AudioSystem for BoardAudioSystem<'d> {
    fn init<I: I2cTrait, D: DelayNs>(&mut self, i2c: &mut I, delay: &mut D) {
        // Take peripherals out of Uninitialized state
        let p = match &mut self.state {
            AudioState::Uninitialized(opt) => opt.take().expect("init() called twice"),
            AudioState::Ready(_) => panic!("init() called on already initialized audio system"),
        };

        // 1. Configure WM8960 codec FIRST (while I2S doesn't exist yet)
        {
            let mut codec = Wm8960Codec::new(i2c);
            codec.init(delay);
            codec.enable_input(true);
            codec.enable_output(true);
        }

        // Allow codec to stabilize
        delay.delay_ms(20);

        // 2. NOW construct I2S (codec is ready, clocks are stable)
        let mut config = i2s::Config::default();
        config.mode = i2s::Mode::Slave;
        config.standard = i2s::Standard::Philips;
        config.format = i2s::Format::Data16Channel32;
        config.master_clock = false;
        config.frequency = Hertz(8_000);
        config.clock_polarity = i2s::ClockPolarity::IdleLow;

        let i2s = I2S::new_full_duplex(
            p.spi, p.ws, p.ck, p.sd_tx, p.sd_rx, p.dma_tx, p.tx_buf, p.dma_rx, p.rx_buf, config,
        );

        self.state = AudioState::Ready(i2s);
    }

    fn set_input_enabled<I: I2cTrait>(&mut self, i2c: &mut I, enable: bool) {
        Wm8960Codec::new(i2c).enable_input(enable);
    }

    fn set_output_enabled<I: I2cTrait>(&mut self, i2c: &mut I, enable: bool) {
        Wm8960Codec::new(i2c).enable_output(enable);
    }

    async fn start(&mut self) {
        self.i2s().start();
    }

    async fn stop(&mut self) {
        self.i2s().stop().await;
    }

    async fn read_write(&mut self, tx: &Frame, rx: &mut Frame) -> Result<(), AudioError> {
        match self.i2s().read_write(&tx.0, &mut rx.0).await {
            Ok(_) => Ok(()),
            Err(i2s::Error::Overrun) => Err(AudioError::Overrun),
            Err(i2s::Error::DmaUnsynced) => Err(AudioError::DmaUnsynced),
            Err(_) => Err(AudioError::Overrun),
        }
    }
}

// =============================================================================
// Main
// =============================================================================

const DMA_BUF_SIZE: usize = 64;

bind_interrupts!(struct Irqs {
    USART1 => usart::InterruptHandler<peripherals::USART1>;
    USART2 => usart::InterruptHandler<peripherals::USART2>;
});

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    // Clock configuration for I2S support
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

    // UART config for MGMT (even parity for STM32 bootloader compatibility)
    let mut mgmt_config = Config::default();
    mgmt_config.baudrate = 115200;
    mgmt_config.data_bits = DataBits::DataBits8;
    mgmt_config.stop_bits = StopBits::STOP1;
    mgmt_config.parity = Parity::ParityEven;

    // UART config for NET
    let mut net_config = Config::default();
    net_config.baudrate = 460800;
    net_config.data_bits = DataBits::DataBits8;
    net_config.stop_bits = StopBits::STOP2;
    net_config.parity = Parity::ParityNone;

    // DMA buffers for ring-buffered RX
    let mgmt_rx_buf = singleton!(: [u8; DMA_BUF_SIZE] = [0; DMA_BUF_SIZE]).unwrap();
    let net_rx_buf = singleton!(: [u8; DMA_BUF_SIZE] = [0; DMA_BUF_SIZE]).unwrap();

    // I2S DMA buffers
    let i2s_tx_buf = singleton!(: [u16; I2S_BUF_SIZE] = [0; I2S_BUF_SIZE]).unwrap();
    let i2s_rx_buf = singleton!(: [u16; I2S_BUF_SIZE] = [0; I2S_BUF_SIZE]).unwrap();

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

    // Shared I2C bus for EEPROM and audio codec (I2C1: PB6 SCL, PB7 SDA)
    let i2c = {
        use embassy_stm32::{gpio::Speed, i2c::Config, time::Hertz};

        let mut config = Config::default();
        config.frequency = Hertz(100_000);
        config.gpio_speed = Speed::VeryHigh;
        config.sda_pullup = false;
        config.scl_pullup = false;
        config.timeout = embassy_time::Duration::from_millis(1000);

        I2c::new_blocking(p.I2C1, p.PB6, p.PB7, config)
    };
    let delay = Delay;

    // Audio system with DEFERRED I2S construction
    // Peripherals are held until init() configures codec and then constructs I2S
    let audio_system = BoardAudioSystem::new(
        p.SPI3, p.PA15, p.PC10, p.PB5, p.PB4, p.DMA1_CH7, i2s_tx_buf, p.DMA1_CH0, i2s_rx_buf,
    );

    link::ui::App::new(
        to_mgmt,
        from_mgmt,
        to_net,
        from_net,
        led,
        button_a,
        button_b,
        button_mic,
        i2c,
        delay,
        audio_system,
    )
    .run()
    .await;
}

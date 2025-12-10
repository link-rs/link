//! Audio control for the WM8960 codec chip.
//!
//! This module provides control over the WM8960 audio codec attached to the I2C bus.

#![allow(dead_code)] // No need to use all of the fields on the device

use embedded_hal::i2c::I2c;

/// Size of an audio frame in 16-bit samples.
pub const FRAME_SIZE: usize = 320;

/// An audio frame containing PCM samples.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct Frame(pub [u16; FRAME_SIZE]);

impl Default for Frame {
    fn default() -> Self {
        Self([0; FRAME_SIZE])
    }
}

impl Frame {
    /// Convert frame samples to bytes (little-endian).
    pub fn as_bytes(&self) -> [u8; FRAME_SIZE * 2] {
        let mut bytes = [0u8; FRAME_SIZE * 2];
        for (i, sample) in self.0.iter().enumerate() {
            let le = sample.to_le_bytes();
            bytes[i * 2] = le[0];
            bytes[i * 2 + 1] = le[1];
        }
        bytes
    }
}

/// I2S audio error types.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum AudioError {
    /// Audio overrun - data was lost.
    Overrun,
    /// DMA became unsynchronized with the ring buffer.
    DmaUnsynced,
}

/// Trait for controlling the audio codec hardware.
///
/// This provides control over the WM8960 audio codec configuration,
/// including power management and enabling audio paths.
pub trait AudioCodec {
    /// Initialize and start the audio codec with default settings.
    fn start(&mut self);

    /// Enable or disable the audio input (microphone) path.
    fn enable_input(&mut self, enabled: bool);

    /// Enable or disable the audio output (headphone/speaker) path.
    fn enable_output(&mut self, enabled: bool);
}

/// Trait for I2S audio data streaming.
///
/// This trait provides async methods for streaming audio data through the I2S interface.
#[allow(async_fn_in_trait)]
pub trait AudioStream {
    /// Start the I2S audio stream.
    async fn start(&mut self);

    /// Stop the I2S audio stream.
    async fn stop(&mut self);

    /// Read an audio frame from the I2S input.
    async fn read(&mut self) -> Frame;

    /// Write an audio frame to the I2S output.
    async fn write(&mut self, frame: &Frame);

    /// Simultaneously read and write audio frames (full duplex).
    async fn read_write(&mut self, tx: &Frame, rx: &mut Frame) -> Result<(), AudioError>;

    /// Write samples from an iterator to the I2S output.
    async fn write_iter(&mut self, samples: impl Iterator<Item = u16>) {
        let mut send = Frame::default();

        let mut i_mod = 0;
        for (i, sample) in samples.enumerate() {
            i_mod = i % FRAME_SIZE;
            send.0[i_mod] = sample;

            if i_mod == FRAME_SIZE - 1 {
                self.write(&send).await;
            }
        }

        i_mod += 1;
        if i_mod < FRAME_SIZE {
            send.0[i_mod..].fill(0);
            self.write(&send).await;
        }
    }
}

const I2C_ADDR: u8 = 0x1a;

pub struct AudioControl<I> {
    i2c: I,
    regs: Registers,
}

impl<I: I2c> AudioControl<I> {
    const VALUE_MASK: u16 = 0x1ff;

    pub fn new(i2c: I) -> Self {
        Self {
            i2c,
            regs: Registers::default(),
        }
    }

    /// Initialize the audio codec with default settings.
    pub fn init(&mut self) {
        self.power_on();
        self.left_input_path(true);
        self.left_output_path(true);
        self.left_adc(true);
        self.left_dac(true);
        self.configure_dac(true, true, false);
        self.enable_i2s();
    }

    /// Reset the device and enable baseline devices
    fn power_on(&mut self) {
        // address = 0x0f, value = 0b0_0000_0000
        const RESET_SIGNAL: [u8; 2] = [0x1e, 0x00];
        let _ = self.i2c.write(I2C_ADDR, &RESET_SIGNAL);

        self.regs.modify(&mut self.i2c, |r| {
            r.set(PowerMgmt1VrefEnable(true));
            r.set(PowerMgmt1VmidSelect(0b01));
            r.set(MicrophoneBiasEnable(true));
        });
    }

    /// Enable/disable the left input path
    pub fn left_input_path(&mut self, enable: bool) {
        self.regs.modify(&mut self.i2c, |r| {
            // Power on/off input devices
            r.set(PowerMgmt1AinLeftEnable(enable));
            r.set(LeftMicEnable(enable));

            // Disable left input 3 (disconnected)
            r.set(Linput3Boost(0b000));
            r.set(LeftInput3ToOutputMixer(false));
            r.set(LeftInput3ToOutputMixerVolume(0b000));
            r.set(LeftInput3ToNonInverting(false));

            // Disable the right side inputs (disconnected)
            r.set(RightInputAnalogMute(true));
            r.set(Rinput2Boost(0b000));
            r.set(Rinput3Boost(0b000));
            r.set(RightInput3ToOutputMixer(false));
            r.set(RightInput3ToOutputMixerVolume(0b000));

            // Enable the left side
            r.set(LeftInput1ToInverting(true));
            r.set(LeftInput2ToNonInverting(true));
            r.set(LeftInputToBoost(true));
            r.set(LeftInputAnalogMute(false));

            // Set volumes
            r.set(Linput2Boost(0b000)); // mute
            r.set(LeftBoostGain(0b00)); // 0dB
            r.set(InputPgaVolumeUpdate(true));
            r.set(LeftPgaVolume(0b01_0111)); // 0dB
        });
    }

    /// Enable/disable the left output path
    pub fn left_output_path(&mut self, enable: bool) {
        self.regs.modify(&mut self.i2c, |r| {
            // Power on/off output devices
            r.set(LeftOutput1Enable(enable));
            r.set(LeftOutputMixEnable(enable));

            // Left input 3 bypass and speaker output are always disabled
            r.set(LeftInput3ToOutputMixer(false));
            r.set(LeftInput3ToOutputMixerVolume(0b000));
            r.set(LeftSpeakerVolumeUpdate(true));
            r.set(LeftSpeakerVolume(0b000_0000));

            // Set volumes
            r.set(HeadphoneOutVolumeUpdate(true));
            r.set(LeftHeadphoneVolume(0b111_1111)); // 6dB
        });
    }

    /// Enable/disable the right output path
    pub fn right_output_path(&mut self, enable: bool) {
        self.regs.modify(&mut self.i2c, |r| {
            // Power on/off output devices
            r.set(RightOutput1Enable(enable));
            r.set(RightOutputMixEnable(enable));

            // Right input 3 bypass and speaker output are always disabled
            r.set(RightInput3ToOutputMixer(false));
            r.set(RightInput3ToOutputMixerVolume(0b000));
            r.set(RightSpeakerVolumeUpdate(true));
            r.set(RightSpeakerVolume(0b000_0000));

            // Set volumes
            r.set(HeadphoneOutVolumeUpdateRight(true));
            r.set(RightHeadphoneVolume(0b111_1111)); // 6dB
        });
    }

    /// Enable/disable the left analog bypass
    pub fn left_analog_bypass(&mut self, _enable: bool) {
        self.regs.modify(&mut self.i2c, |r| {
            // Connect left input boost to left output mix
            r.set(LeftBoostToLeftOutputMix(true));

            // Set volumes
            r.set(LeftBoostToLeftOutputMixVolume(0b000)); // 0dB
        })
    }

    /// Enable/disable digital loopback
    pub fn digital_loopback(&mut self, enable: bool) {
        self.regs.modify(&mut self.i2c, |r| {
            // Set ADC to use the DACLRC clock
            r.set(AdcLrcPinSelect(enable));

            // Enable digital loopback
            r.set(DigitalLoopback(enable));
        })
    }

    /// Enable the I2S interface
    pub fn enable_i2s(&mut self) {
        self.regs.modify(&mut self.i2c, |r| {
            // Set master mode, I2S, 16-bit words
            r.set(AudioInterfaceMasterMode(true));
            r.set(AudioWordLength(0b00));
            r.set(AudioFormat(0b10));

            // Set clocks for 8khz
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

    /// Enable/disable the left DAC
    pub fn left_dac(&mut self, enable: bool) {
        self.regs.modify(&mut self.i2c, |r| {
            // Power on and connect
            r.set(LeftDacEnable(enable));
            r.set(LeftDacToOutputMixer(enable));

            // Set volume
            r.set(DacVolumeUpdate(true));
            r.set(LeftDacDigitalVolume(0b1111_1111)); // 0dB
        });
    }

    /// Enable/disable the right DAC
    pub fn right_dac(&mut self, enable: bool) {
        self.regs.modify(&mut self.i2c, |r| {
            // Power on and connect
            r.set(RightDacEnable(enable));
            r.set(RightDacToOutputMixer(enable));

            // Set volume
            r.set(DacVolumeUpdateRight(true));
            r.set(RightDacDigitalVolume(0b1111_1111)); // 0dB
        })
    }

    /// Enable/disable the left ADC
    pub fn left_adc(&mut self, enable: bool) {
        self.regs.modify(&mut self.i2c, |r| {
            // Power on
            r.set(PowerMgmt1EnableAdcLeft(enable));

            // Disable the high pass filter
            r.set(AdcHighPassDisable(true));
        });
    }

    /// Configure DAC behavior
    pub fn configure_dac(&mut self, mono_mix: bool, soft_mute_mode: bool, mute: bool) {
        self.regs.modify(&mut self.i2c, |r| {
            // Mix the left and right DAC outputs before sending them to the mixer
            r.set(DacMonoMix(mono_mix));

            // Make DAC mute softer
            r.set(DacSoftMuteMode(soft_mute_mode));

            // Mute DAC outputs
            r.set(DacSoftMuteEnable(mute));
        })
    }
}

impl<I: I2c> AudioCodec for AudioControl<I> {
    fn start(&mut self) {
        self.init();
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

pub type RegAddr = u8;

trait ToFromU16 {
    fn from_u16(x: u16) -> Self;
    fn into_u16(self) -> u16;
}

impl ToFromU16 for bool {
    fn from_u16(x: u16) -> Self {
        x != 0
    }

    fn into_u16(self) -> u16 {
        if self { 1 } else { 0 }
    }
}

impl ToFromU16 for u8 {
    fn from_u16(x: u16) -> Self {
        x as Self
    }

    fn into_u16(self) -> u16 {
        self.into()
    }
}

impl ToFromU16 for u16 {
    fn from_u16(x: u16) -> Self {
        x
    }

    fn into_u16(self) -> u16 {
        self
    }
}

#[derive(Clone)]
pub struct Registers {
    regs: [u16; 56],
}

impl Default for Registers {
    fn default() -> Self {
        let init: [(u8, u16); 56] = [
            (0x00, 0b0_1001_0111), // R0  Left Input volume
            (0x01, 0b0_1001_0111), // R1  Right Input volume
            (0x02, 0b0_0000_0000), // R2  LOUT1 volume
            (0x03, 0b0_0000_0000), // R3  ROUT1 volume
            (0x04, 0b0_0000_0000), // R4  Clocking (1)
            (0x05, 0b0_0000_1000), // R5  ADC & DAC Control (1)
            (0x06, 0b0_0000_0000), // R6  ADC & DAC Control (2)
            (0x07, 0b0_0000_1010), // R7  Audio Interface
            (0x08, 0b1_1100_0000), // R8  Clocking (2)
            (0x09, 0b0_0000_0000), // R9  Audio Interface
            (0x0A, 0b0_1111_1111), // R10 Left DAC volume
            (0x0B, 0b0_1111_1111), // R11 Right DAC volume
            (0x0C, 0b0_0000_0000), // R12 Reserved
            (0x0D, 0b0_0000_0000), // R13 Reserved
            (0x0E, 0b0_0000_0000), // R14 Reserved
            (0x0F, 0b0_0000_0000), // R15 Reset (not reset)
            (0x10, 0b0_0000_0000), // R16 3D control
            (0x11, 0b0_0111_1011), // R17 ALC1
            (0x12, 0b1_0000_0000), // R18 ALC2
            (0x13, 0b0_0011_0010), // R19 ALC3
            (0x14, 0b0_0000_0000), // R20 Noise Gate
            (0x15, 0b0_1100_0011), // R21 Left ADC volume
            (0x16, 0b0_1100_0011), // R22 Right ADC volume
            (0x17, 0b1_1100_0000), // R23 Additional control (1)
            (0x18, 0b0_0000_0000), // R24 Additional control (2)
            (0x19, 0b0_0000_0000), // R25 Power Mgmt (1)
            (0x1A, 0b0_0000_0000), // R26 Power Mgmt (2)
            (0x1B, 0b0_0000_0000), // R27 Additional Control (3)
            (0x1C, 0b0_0000_0000), // R28 Anti-pop 1
            (0x1D, 0b0_0000_0000), // R29 Anti-pop 2
            (0x1E, 0b0_0000_0000), // R30 Reserved
            (0x1F, 0b0_0000_0000), // R31 Reserved
            (0x20, 0b1_0000_0000), // R32 ADCL signal path
            (0x21, 0b1_0000_0000), // R33 ADCR signal path
            (0x22, 0b0_0101_0000), // R34 Left out Mix (1)
            (0x23, 0b0_0101_0000), // R35 Reserved
            (0x24, 0b0_0101_0000), // R36 Reserved
            (0x25, 0b0_0101_0000), // R37 Right out Mix (2)
            (0x26, 0b0_0000_0000), // R38 Mono out Mix (1)
            (0x27, 0b0_0000_0000), // R39 Mono out Mix (2)
            (0x28, 0b0_0000_0000), // R40 LOUT2 volume
            (0x29, 0b0_0000_0000), // R41 ROUT2 volume
            (0x2A, 0b0_0100_0000), // R42 MONOOUT volume
            (0x2B, 0b0_0000_0000), // R43 Input boost mixer (1)
            (0x2C, 0b0_0000_0000), // R44 Input boost mixer (2)
            (0x2D, 0b0_0101_0000), // R45 Bypass (1)
            (0x2E, 0b0_0101_0000), // R46 Bypass (2)
            (0x2F, 0b0_0000_0000), // R47 Power Mgmt (3)
            (0x30, 0b0_0000_0010), // R48 Additional Control (4)
            (0x31, 0b0_0011_0111), // R49 Class D Control (1)
            (0x32, 0b0_0100_1101), // R50 Reserved
            (0x33, 0b0_1000_0000), // R51 Class D Control (3)
            (0x34, 0b0_0000_1000), // R52 PLL N
            (0x35, 0b0_0011_0001), // R53 PLL K1
            (0x36, 0b0_0010_0110), // R54 PLL K2
            (0x37, 0b0_1110_1001), // R55 PLL K3
        ];

        let mut regs = [0u16; 56];
        for (i, (addr, val)) in init.iter().enumerate() {
            regs[i] = ((*addr as u16) << 9) | (val & 0x01FF);
        }

        Self { regs }
    }
}

impl Registers {
    fn modify<I, F>(&mut self, i2c: &mut I, f: F)
    where
        I: I2c,
        F: FnOnce(&mut RegisterView),
    {
        let mut r = RegisterView::new(&mut self.regs);
        f(&mut r);

        let modified = r
            .modified
            .iter()
            .enumerate()
            .filter_map(|(i, m)| m.then_some(i));
        for i in modified {
            let _ = i2c.write(I2C_ADDR, &self.regs[i].to_be_bytes());
        }
    }
}

pub struct RegisterView<'a> {
    regs: &'a mut [u16; 56],
    modified: [bool; 56],
}

impl<'a> RegisterView<'a> {
    pub const fn new(regs: &'a mut [u16; 56]) -> Self {
        Self {
            regs,
            modified: [false; 56],
        }
    }

    pub fn get<F: FieldAccess>(&self) -> F::Value {
        let reg = self.regs[F::ADDR as usize];
        F::get(reg)
    }

    pub fn set<F: FieldAccess>(&mut self, val: F) {
        let idx: usize = F::ADDR.into();
        let old = self.regs[idx];
        let new = val.set(old);
        if new != old {
            self.regs[idx] = new;
            self.modified[idx] = true;
        }
    }
}

pub trait FieldAccess {
    const ADDR: RegAddr;
    const OFFSET: u8;
    const WIDTH: u8;
    const MAX: u16 = (1 << Self::WIDTH) - 1;
    const MASK: u16 = Self::MAX << Self::OFFSET;
    type Value;

    fn new(val: Self::Value) -> Self;
    fn get(regval: u16) -> Self::Value;
    fn set(&self, regval: u16) -> u16;
}

macro_rules! define_field {
    ($name:ident, $addr:expr, $offset:expr, $width:expr, $val:ty) => {
        pub struct $name($val);

        impl FieldAccess for $name {
            const ADDR: RegAddr = $addr;
            const OFFSET: u8 = $offset;
            const WIDTH: u8 = $width;
            type Value = $val;

            #[inline]
            fn new(val: $val) -> Self {
                Self(val)
            }

            #[inline]
            fn get(regval: u16) -> $val {
                <$val>::from_u16((regval & Self::MASK) >> Self::OFFSET)
            }

            #[inline]
            fn set(&self, regval: u16) -> u16 {
                let val = self.0.into_u16();
                assert!(
                    val <= Self::MAX,
                    concat!(stringify!($name), ": value out of range"),
                );
                let mask = ((1u16 << Self::WIDTH) - 1) << Self::OFFSET;
                (regval & !mask) | ((val << Self::OFFSET) & mask)
            }
        }
    };
}

// R0 (0x00) Left Input Volume
define_field!(InputPgaVolumeUpdate, 0x00, 8, 1, bool);
define_field!(LeftInputAnalogMute, 0x00, 7, 1, bool);
define_field!(LeftPgaZeroCross, 0x00, 6, 1, bool);
define_field!(LeftPgaVolume, 0x00, 0, 6, u8);

// R1 (0x01) Right Input Volume
define_field!(InputPgaVolumeUpdateRight, 0x01, 8, 1, bool);
define_field!(RightInputAnalogMute, 0x01, 7, 1, bool);
define_field!(RightPgaZeroCross, 0x01, 6, 1, bool);
define_field!(RightPgaVolume, 0x01, 0, 6, u8);

// R2 (0x02) LOUT1 volume
define_field!(HeadphoneOutVolumeUpdate, 0x02, 8, 1, bool);
define_field!(LeftOutZeroCross, 0x02, 7, 1, bool);
define_field!(LeftHeadphoneVolume, 0x02, 0, 7, u8);

// R3 (0x03) ROUT1 volume
define_field!(HeadphoneOutVolumeUpdateRight, 0x03, 8, 1, bool);
define_field!(RightOutZeroCross, 0x03, 7, 1, bool);
define_field!(RightHeadphoneVolume, 0x03, 0, 7, u8);

// R4 (0x04) Clocking (1)
define_field!(Adc1Divider, 0x04, 6, 3, u8);
define_field!(DacDivider, 0x04, 3, 3, u8);
define_field!(SysClkDiv, 0x04, 1, 2, u8);
define_field!(ClockSelect, 0x04, 0, 1, bool);

// R5 (0x05) ADC & DAC Control (CTR1)
define_field!(Dac6dBAttenuateEnable, 0x05, 7, 1, bool);
define_field!(AdcPolarityControl, 0x05, 5, 2, u8);
define_field!(DacSoftMuteEnable, 0x05, 3, 1, bool);
define_field!(DeEmphasisControl, 0x05, 3, 2, u8);
define_field!(AdcHighPassDisable, 0x05, 0, 1, bool);

// R6 (0x06) ADC & DAC Control (CTR2)
define_field!(DacSlopeMode, 0x06, 1, 1, bool);
define_field!(DacSoftMuteRampSlow, 0x06, 2, 1, bool);
define_field!(DacSoftMuteMode, 0x06, 3, 1, bool);

// R7 (0x07) Audio Interface
define_field!(AdcLeftRightSwap, 0x07, 8, 1, bool);
define_field!(BclkInvert, 0x07, 7, 1, bool);
define_field!(AudioInterfaceMasterMode, 0x07, 6, 1, bool);
define_field!(DacLeftRightSwap, 0x07, 5, 1, bool);
define_field!(LrcPolarityOrDspMode, 0x07, 4, 1, bool);
define_field!(AudioWordLength, 0x07, 2, 2, u8);
define_field!(AudioFormat, 0x07, 0, 2, u8);

// R8 (0x08) Clocking (2)
define_field!(ClassDSysclkDivider, 0x08, 6, 3, u8);
define_field!(BclkFrequency, 0x08, 0, 4, u8);

// R9 (0x09) Audio Interface
define_field!(AdcLrcPinSelect, 0x09, 6, 1, bool);
define_field!(WordLength8, 0x09, 5, 1, bool);
define_field!(DacCompanding, 0x09, 3, 2, u8);
define_field!(AdcCompanding, 0x09, 1, 2, u8);
define_field!(DigitalLoopback, 0x09, 0, 1, bool);

// R10 (0x0A) Left DAC Volume
define_field!(DacVolumeUpdate, 0x0A, 8, 1, bool);
define_field!(LeftDacDigitalVolume, 0x0A, 0, 8, u8);

// R11 (0x0B) Right DAC Volume
define_field!(DacVolumeUpdateRight, 0x0B, 8, 1, bool);
define_field!(RightDacDigitalVolume, 0x0B, 0, 8, u8);

// R16 (0x10) 3D control
define_field!(ThreeDEnable, 0x10, 2, 1, bool);
define_field!(ThreeDLowerCutSelect, 0x10, 1, 1, bool);
define_field!(ThreeDUpperCutSelect, 0x10, 0, 1, bool);
define_field!(ThreeDControlRaw, 0x10, 0, 9, u16);

// R20 (0x14) Noise gate
define_field!(NoiseGateThreshold, 0x14, 3, 5, u8);
define_field!(NoiseGateEnable, 0x14, 0, 1, bool);

// R21 (0x15) Left ADC volume
define_field!(LeftAdcDigitalVolume, 0x15, 0, 8, u8);
define_field!(AdcVolumeUpdateLeft, 0x15, 8, 1, bool);

// R22 (0x16) Right ADC volume
define_field!(RightAdcDigitalVolume, 0x16, 0, 8, u8);
define_field!(AdcVolumeUpdateRight, 0x16, 8, 1, bool);

// R23 (0x17) Additional Control (1)
define_field!(ThermalShutDownEnable, 0x17, 8, 1, bool);
define_field!(AnalogBiasOptimisation, 0x17, 6, 2, u8);
define_field!(DacMonoMix, 0x17, 4, 1, bool);
define_field!(AdcDataOutputSelect, 0x17, 2, 2, u8);
define_field!(TimeoutClockSelect, 0x17, 1, 1, bool);
define_field!(TimeoutEnable, 0x17, 0, 1, bool);

// R24 (0x18) Additional Control (2)
define_field!(AdclrcDaclrcMode, 0x18, 2, 1, bool);
define_field!(Reg24Raw, 0x18, 0, 9, u16);

// R25 (0x19) Power Management (1)
define_field!(PowerMgmt1VmidSelect, 0x19, 7, 2, u8);
define_field!(PowerMgmt1VrefEnable, 0x19, 6, 1, bool);
define_field!(PowerMgmt1AinLeftEnable, 0x19, 5, 1, bool);
define_field!(PowerMgmt1AinRightEnable, 0x19, 4, 1, bool);
define_field!(PowerMgmt1EnableAdcLeft, 0x19, 3, 1, bool);
define_field!(PowerMgmt1EnableAdcRight, 0x19, 2, 1, bool);
define_field!(MicrophoneBiasEnable, 0x19, 1, 1, bool);
define_field!(MasterClockDisable, 0x19, 0, 1, bool);

// R26 (0x1A) Power Management (2)
define_field!(LeftDacEnable, 0x1A, 8, 1, bool);
define_field!(RightDacEnable, 0x1A, 7, 1, bool);
define_field!(LeftOutput1Enable, 0x1A, 6, 1, bool);
define_field!(RightOutput1Enable, 0x1A, 5, 1, bool);
define_field!(LeftSpeakerEnable, 0x1A, 4, 1, bool);
define_field!(RightSpeakerEnable, 0x1A, 3, 1, bool);
define_field!(Out3Enable, 0x1A, 1, 1, bool);
define_field!(PllEnable, 0x1A, 0, 1, bool);

// R27 (0x1B) Additional Control (3)
define_field!(VrefToAnalogueResistance, 0x1B, 6, 1, bool);
define_field!(CaplessHeadphoneSwitchEnable, 0x1B, 3, 1, bool);
define_field!(AdcAlcSampleRateSelect, 0x1B, 0, 3, u8);

// R32 (0x20) ADCL signal path
define_field!(LeftInput1ToInverting, 0x20, 8, 1, bool);
define_field!(LeftInput3ToNonInverting, 0x20, 7, 1, bool);
define_field!(LeftInput2ToNonInverting, 0x20, 6, 1, bool);
define_field!(LeftBoostGain, 0x20, 4, 2, u8);
define_field!(LeftInputToBoost, 0x20, 3, 1, bool);

// R33 (0x21) ADCR signal path
define_field!(RightMicBoost, 0x21, 4, 2, u8);
define_field!(AdcrSignalPathRaw, 0x21, 0, 9, u16);

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
define_field!(LeftSpeakerZeroCross, 0x28, 7, 1, bool);
define_field!(LeftSpeakerVolume, 0x28, 0, 7, u8);

// R41 (0x29) ROUT2 volume
define_field!(RightSpeakerVolumeUpdate, 0x29, 8, 1, bool);
define_field!(RightSpeakerZeroCross, 0x29, 7, 1, bool);
define_field!(RightSpeakerVolume, 0x29, 0, 7, u8);

// R42 (0x2A) MONOOUT volume
define_field!(MonoOutVolume, 0x2A, 6, 1, bool);

// R43 (0x2B) Input Boost Mixer (1)
define_field!(Linput3Boost, 0x2B, 4, 3, u8);
define_field!(Linput2Boost, 0x2B, 1, 3, u8);

// R44 (0x2C) Input Boost Mixer (2)
define_field!(Rinput3Boost, 0x2C, 4, 3, u8);
define_field!(Rinput2Boost, 0x2C, 1, 3, u8);

// R45 (0x2D) Bypass (1)
define_field!(LeftBoostToLeftOutputMix, 0x2D, 7, 1, bool);
define_field!(LeftBoostToLeftOutputMixVolume, 0x2D, 4, 3, u8);

// R46 (0x2E) Bypass (2)
define_field!(RightBoostToRightOutputMix, 0x2E, 7, 1, bool);
define_field!(RightBoostToRightOutputMixVolume, 0x2E, 4, 3, u8);

// R47 (0x2F) Power Management (3)
define_field!(LeftMicEnable, 0x2F, 5, 1, bool);
define_field!(RightMicEnable, 0x2F, 4, 1, bool);
define_field!(LeftOutputMixEnable, 0x2F, 3, 1, bool);
define_field!(RightOutputMixEnable, 0x2F, 2, 1, bool);

// R48 (0x30) Additional Control (4)

// R49 (0x31) Class D Control (1)
define_field!(ClassDSpeakerOutputEnable, 0x31, 6, 2, u8);

// R51 (0x33) Class D Control (3)
define_field!(SpeakerDcGain, 0x33, 3, 3, u8);
define_field!(SpeakerAcGain, 0x33, 0, 3, u8);

// R52 (0x34) PLL N
define_field!(OpClockDivider, 0x34, 6, 3, u8);
define_field!(IntegerModeEnable, 0x34, 5, 1, bool);
define_field!(PllRescale, 0x34, 4, 1, bool);
define_field!(PllN, 0x34, 0, 4, u8);

// R53, R54, R55 (0x35, 0x36, 0x37) PLL K
define_field!(PllKMsb, 0x35, 0, 8, u8);
define_field!(PllKMid, 0x36, 0, 8, u8);
define_field!(PllKLsb, 0x37, 0, 8, u8);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mocks::{I2cDevice, MockI2c};

    /// Mock audio control chip (WM8960) for testing
    pub struct MockAudioControl {
        regs: [u16; 56],
    }

    impl MockAudioControl {
        pub const I2C_ADDR: u8 = 0x1a;

        pub fn new() -> Self {
            Self { regs: [0; 56] }
        }

        pub fn get_reg(&self, addr: u8) -> u16 {
            self.regs[addr as usize]
        }
    }

    impl I2cDevice for MockAudioControl {
        fn transaction(
            &mut self,
            operations: &mut [embedded_hal::i2c::Operation<'_>],
        ) -> Result<(), core::convert::Infallible> {
            for op in operations {
                match op {
                    embedded_hal::i2c::Operation::Write(data) => {
                        // WM8960 uses 7-bit address in high byte, 9-bit data
                        // Format: [addr_high | data_high, data_low]
                        if data.len() == 2 {
                            let word = u16::from_be_bytes([data[0], data[1]]);
                            let addr = (word >> 9) as u8;
                            let value = word & 0x1ff;
                            if (addr as usize) < self.regs.len() {
                                self.regs[addr as usize] = value;
                            }
                        }
                    }
                    embedded_hal::i2c::Operation::Read(_) => {
                        // WM8960 is write-only for configuration
                    }
                }
            }
            Ok(())
        }
    }

    use std::cell::RefCell;
    use std::rc::Rc;

    fn mock_i2c_with_audio() -> (MockI2c, Rc<RefCell<MockAudioControl>>) {
        let mut i2c = MockI2c::new();
        let mock = Rc::new(RefCell::new(MockAudioControl::new()));
        i2c.attach_shared(MockAudioControl::I2C_ADDR, mock.clone());
        (i2c, mock)
    }

    #[test]
    fn audio_control_new() {
        let (i2c, _mock) = mock_i2c_with_audio();
        let _audio = AudioControl::new(i2c);
    }

    #[test]
    fn audio_control_power_on() {
        let (i2c, mock) = mock_i2c_with_audio();
        let mut audio = AudioControl::new(i2c);
        audio.init();

        let mock = mock.borrow();
        // R25 (0x19) Power Management (1): VREF enabled, VMID=01, mic bias enabled
        let r25 = mock.get_reg(0x19);
        assert!(r25 & (1 << 6) != 0, "VREF should be enabled");
        assert!((r25 >> 7) & 0b11 == 0b01, "VMID should be 01");
        assert!(r25 & (1 << 1) != 0, "Mic bias should be enabled");
    }

    #[test]
    fn audio_control_enable_i2s() {
        let (i2c, mock) = mock_i2c_with_audio();
        let mut audio = AudioControl::new(i2c);
        audio.enable_i2s();

        let mock = mock.borrow();
        // R7 (0x07) Audio Interface: master mode, I2S format (0b10), 16-bit (0b00)
        let r7 = mock.get_reg(0x07);
        assert!(r7 & (1 << 6) != 0, "Master mode should be enabled");
        assert!(r7 & 0b11 == 0b10, "Audio format should be I2S (0b10)");

        // R26 (0x1A) Power Management (2): PLL enabled
        let r26 = mock.get_reg(0x1A);
        assert!(r26 & 1 != 0, "PLL should be enabled");
    }

    #[test]
    fn audio_control_digital_loopback() {
        let (i2c, mock) = mock_i2c_with_audio();
        let mut audio = AudioControl::new(i2c);

        audio.digital_loopback(true);
        {
            let mock = mock.borrow();
            // R9 (0x09): digital loopback bit 0
            let r9 = mock.get_reg(0x09);
            assert!(r9 & 1 != 0, "Digital loopback should be enabled");
        }

        audio.digital_loopback(false);
        {
            let mock = mock.borrow();
            let r9 = mock.get_reg(0x09);
            assert!(r9 & 1 == 0, "Digital loopback should be disabled");
        }
    }

    #[test]
    fn audio_control_left_dac() {
        let (i2c, mock) = mock_i2c_with_audio();
        let mut audio = AudioControl::new(i2c);
        audio.left_dac(true);

        let mock = mock.borrow();
        // R26 (0x1A) Power Management (2): left DAC enable bit 8
        let r26 = mock.get_reg(0x1A);
        assert!(r26 & (1 << 8) != 0, "Left DAC should be enabled");

        // R34 (0x22) Left Out Mix: left DAC to output mixer bit 8
        let r34 = mock.get_reg(0x22);
        assert!(r34 & (1 << 8) != 0, "Left DAC to output mixer should be enabled");

        // R10 (0x0A) Left DAC volume: should be 0xFF (0dB)
        let r10 = mock.get_reg(0x0A);
        assert!(r10 & 0xFF == 0xFF, "Left DAC volume should be 0xFF");
    }

    #[test]
    fn audio_control_right_dac() {
        let (i2c, mock) = mock_i2c_with_audio();
        let mut audio = AudioControl::new(i2c);
        audio.right_dac(true);

        let mock = mock.borrow();
        // R26 (0x1A) Power Management (2): right DAC enable bit 7
        let r26 = mock.get_reg(0x1A);
        assert!(r26 & (1 << 7) != 0, "Right DAC should be enabled");

        // R37 (0x25) Right Out Mix: right DAC to output mixer bit 8
        let r37 = mock.get_reg(0x25);
        assert!(r37 & (1 << 8) != 0, "Right DAC to output mixer should be enabled");
    }

    #[test]
    fn audio_control_left_output_path() {
        let (i2c, mock) = mock_i2c_with_audio();
        let mut audio = AudioControl::new(i2c);
        audio.left_output_path(true);

        let mock = mock.borrow();
        // R26 (0x1A) Power Management (2): LOUT1 enable bit 6
        let r26 = mock.get_reg(0x1A);
        assert!(r26 & (1 << 6) != 0, "LOUT1 should be enabled");

        // R47 (0x2F) Power Management (3): left output mix enable bit 3
        let r47 = mock.get_reg(0x2F);
        assert!(r47 & (1 << 3) != 0, "Left output mix should be enabled");

        // R2 (0x02) LOUT1 volume: should be 0x7F (6dB)
        let r2 = mock.get_reg(0x02);
        assert!(r2 & 0x7F == 0x7F, "Left headphone volume should be 0x7F");
    }

    #[test]
    fn audio_control_right_output_path() {
        let (i2c, mock) = mock_i2c_with_audio();
        let mut audio = AudioControl::new(i2c);
        audio.right_output_path(true);

        let mock = mock.borrow();
        // R26 (0x1A) Power Management (2): ROUT1 enable bit 5
        let r26 = mock.get_reg(0x1A);
        assert!(r26 & (1 << 5) != 0, "ROUT1 should be enabled");

        // R47 (0x2F) Power Management (3): right output mix enable bit 2
        let r47 = mock.get_reg(0x2F);
        assert!(r47 & (1 << 2) != 0, "Right output mix should be enabled");

        // R3 (0x03) ROUT1 volume: should be 0x7F (6dB)
        let r3 = mock.get_reg(0x03);
        assert!(r3 & 0x7F == 0x7F, "Right headphone volume should be 0x7F");
    }

    #[test]
    fn audio_control_configure_dac() {
        let (i2c, mock) = mock_i2c_with_audio();
        let mut audio = AudioControl::new(i2c);
        audio.configure_dac(true, true, false);

        let mock = mock.borrow();
        // R23 (0x17) Additional Control (1): DAC mono mix bit 4
        let r23 = mock.get_reg(0x17);
        assert!(r23 & (1 << 4) != 0, "DAC mono mix should be enabled");

        // R6 (0x06) ADC & DAC Control (CTR2): soft mute mode bit 3
        let r6 = mock.get_reg(0x06);
        assert!(r6 & (1 << 3) != 0, "Soft mute mode should be enabled");

        // R5 (0x05) ADC & DAC Control (CTR1): soft mute enable bit 3 should be off
        let r5 = mock.get_reg(0x05);
        assert!(r5 & (1 << 3) == 0, "Soft mute should be disabled");
    }
}

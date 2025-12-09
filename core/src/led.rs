use embedded_hal::digital::{ErrorType, OutputPin, StatefulOutputPin};

/// Wrapper that inverts pin logic (for pins wired with opposite polarity)
pub struct InvertedPin<P>(pub P);

impl<P: ErrorType> ErrorType for InvertedPin<P> {
    type Error = P::Error;
}

impl<P: OutputPin> OutputPin for InvertedPin<P> {
    fn set_low(&mut self) -> Result<(), Self::Error> {
        self.0.set_high()
    }

    fn set_high(&mut self) -> Result<(), Self::Error> {
        self.0.set_low()
    }
}

impl<P: StatefulOutputPin> StatefulOutputPin for InvertedPin<P> {
    fn is_set_high(&mut self) -> Result<bool, Self::Error> {
        self.0.is_set_low()
    }

    fn is_set_low(&mut self) -> Result<bool, Self::Error> {
        self.0.is_set_high()
    }
}

/// All 8 possible RGB colors (3-bit color space)
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Color {
    Black,   // 000
    Red,     // 100
    Green,   // 010
    Blue,    // 001
    Yellow,  // 110 (R+G)
    Cyan,    // 011 (G+B)
    Magenta, // 101 (R+B)
    White,   // 111
}

impl Color {
    /// Returns (red, green, blue) as booleans (true = on)
    fn rgb(self) -> (bool, bool, bool) {
        match self {
            Color::Black => (false, false, false),
            Color::Red => (true, false, false),
            Color::Green => (false, true, false),
            Color::Blue => (false, false, true),
            Color::Yellow => (true, true, false),
            Color::Cyan => (false, true, true),
            Color::Magenta => (true, false, true),
            Color::White => (true, true, true),
        }
    }
}

/// RGB LED abstraction over three individual output pins
pub struct Led<R, G, B> {
    red: R,
    green: G,
    blue: B,
}

impl<R, G, B> Led<R, G, B>
where
    R: StatefulOutputPin,
    G: StatefulOutputPin,
    B: StatefulOutputPin,
{
    pub fn new(red: R, green: G, blue: B) -> Self {
        Self { red, green, blue }
    }

    /// Set the LED to the specified color (active low / common anode)
    pub fn set(&mut self, color: Color) {
        let (r, g, b) = color.rgb();

        // Active low: set_low() turns LED on, set_high() turns LED off
        if r {
            let _ = self.red.set_low();
        } else {
            let _ = self.red.set_high();
        }

        if g {
            let _ = self.green.set_low();
        } else {
            let _ = self.green.set_high();
        }

        if b {
            let _ = self.blue.set_low();
        } else {
            let _ = self.blue.set_high();
        }
    }
}

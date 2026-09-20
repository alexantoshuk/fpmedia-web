//! Common SMPTE / NTSC frame rates with drop-frame flags.

use core::fmt;

/// Rational playback rate (`num / den` frames per second).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rational {
    pub num: u32,
    pub den: u32,
}

impl Rational {
    pub const fn new(num: u32, den: u32) -> Self {
        Self { num, den }
    }

    pub fn to_f64(self) -> f64 {
        self.num as f64 / self.den as f64
    }
}

/// Frame rate used for SMPTE counting.
///
/// NTSC rates use **nominal** integer fps for HH:MM:SS:FF (24 for 23.976, 30 for
/// 29.97, 60 for 59.94). Drop-frame variants skip frames at minute boundaries
/// (Heidelberger / SMPTE ST 12-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FrameRate {
    Fps23_976,
    Fps24,
    Fps25,
    Fps29_97Df,
    Fps29_97Ndf,
    Fps30,
    Fps50,
    Fps59_94Df,
    Fps59_94Ndf,
    Fps60,
}

impl FrameRate {
    /// Exact playback rate as a rational.
    pub const fn rational(self) -> Rational {
        match self {
            Self::Fps23_976 => Rational::new(24000, 1001),
            Self::Fps24 => Rational::new(24, 1),
            Self::Fps25 => Rational::new(25, 1),
            Self::Fps29_97Df | Self::Fps29_97Ndf => Rational::new(30000, 1001),
            Self::Fps30 => Rational::new(30, 1),
            Self::Fps50 => Rational::new(50, 1),
            Self::Fps59_94Df | Self::Fps59_94Ndf => Rational::new(60000, 1001),
            Self::Fps60 => Rational::new(60, 1),
        }
    }

    /// Nominal integer fps used in the timecode frame field.
    pub const fn nominal(self) -> u32 {
        match self {
            Self::Fps23_976 | Self::Fps24 => 24,
            Self::Fps25 => 25,
            Self::Fps29_97Df | Self::Fps29_97Ndf | Self::Fps30 => 30,
            Self::Fps50 => 50,
            Self::Fps59_94Df | Self::Fps59_94Ndf | Self::Fps60 => 60,
        }
    }

    pub const fn is_drop_frame(self) -> bool {
        matches!(self, Self::Fps29_97Df | Self::Fps59_94Df)
    }

    /// Frames skipped at each non-10th minute (2 @ 29.97 DF, 4 @ 59.94 DF).
    pub const fn drop_count(self) -> u32 {
        match self {
            Self::Fps29_97Df => 2,
            Self::Fps59_94Df => 4,
            _ => 0,
        }
    }

    pub fn to_f64(self) -> f64 {
        self.rational().to_f64()
    }

    /// Map a float fps + drop flag to a known rate (tolerant to 2 decimals).
    pub fn from_float(fps: f64, drop_frame: bool) -> Option<Self> {
        let rounded = (fps * 100.0).round() as i32;
        match (rounded, drop_frame) {
            (2397 | 2398, _) => Some(Self::Fps23_976),
            (2400, _) => Some(Self::Fps24),
            (2500, _) => Some(Self::Fps25),
            (2997, true) => Some(Self::Fps29_97Df),
            (2997, false) => Some(Self::Fps29_97Ndf),
            (3000, _) => Some(Self::Fps30),
            (5000, _) => Some(Self::Fps50),
            (5994, true) => Some(Self::Fps59_94Df),
            (5994, false) => Some(Self::Fps59_94Ndf),
            (6000, _) => Some(Self::Fps60),
            _ => None,
        }
    }
}

impl fmt::Display for FrameRate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fps23_976 => write!(f, "23.976"),
            Self::Fps24 => write!(f, "24"),
            Self::Fps25 => write!(f, "25"),
            Self::Fps29_97Df => write!(f, "29.97df"),
            Self::Fps29_97Ndf => write!(f, "29.97"),
            Self::Fps30 => write!(f, "30"),
            Self::Fps50 => write!(f, "50"),
            Self::Fps59_94Df => write!(f, "59.94df"),
            Self::Fps59_94Ndf => write!(f, "59.94"),
            Self::Fps60 => write!(f, "60"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_float_common() {
        assert_eq!(
            FrameRate::from_float(23.976, false),
            Some(FrameRate::Fps23_976)
        );
        assert_eq!(
            FrameRate::from_float(29.97, true),
            Some(FrameRate::Fps29_97Df)
        );
        assert_eq!(
            FrameRate::from_float(29.97, false),
            Some(FrameRate::Fps29_97Ndf)
        );
    }
}

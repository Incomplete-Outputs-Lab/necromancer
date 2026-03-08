//! ME transition type (next transition style).
//!
//! Corresponds to `_BMDSwitcherTransitionStyle` in BMDSwitcherAPI.
//! Used by `CTTp` (ChangeTransitionNext).

use binrw::binrw;
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use std::convert::TryFrom;

/// Type of the next M/E transition (Mix, Dip, Wipe, DVE, Stinger).
#[binrw]
#[brw(repr = u8, big)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[repr(u8)]
pub enum TransitionType {
    #[default]
    Mix = 0,
    Dip = 1,
    Wipe = 2,
    DVE = 3,
    Sting = 4,
}

impl TryFrom<u8> for TransitionType {
    type Error = ();
    fn try_from(v: u8) -> Result<Self, Self::Error> {
        match v {
            0 => Ok(TransitionType::Mix),
            1 => Ok(TransitionType::Dip),
            2 => Ok(TransitionType::Wipe),
            3 => Ok(TransitionType::DVE),
            4 => Ok(TransitionType::Sting),
            _ => Err(()),
        }
    }
}

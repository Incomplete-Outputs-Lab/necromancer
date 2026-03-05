//! # Aux; 2/2 atoms
//!
//! Aux (auxiliary) video outputs.
//!
//! ## Implemented atoms (2)
//!
//! FourCC | Atom name | Length
//! ------ | --------- | ------
//! `AuxS` | `AuxSource` | 0xc
//! `CAuS` | `ChangeAuxSource` | 0xc
//!

use crate::structs::VideoSource;
use binrw::binrw;

/// `AuxS`: current aux source (`AuxSource`)
///
/// Sent by the switcher to report the current source on an AUX bus.
///
/// ## Packet format
///
/// * `u8`: aux bus ID
/// * 1 byte padding
/// * `u16`: video source
#[binrw]
#[brw(big)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuxSource {
    #[brw(pad_after = 1)]
    pub aux_bus: u8,
    pub video_source: VideoSource,
}

/// `CAuS`: change aux source (`ChangeAuxSource`)
///
/// Sent by a client to change the source on an AUX bus.
///
/// sofie-atem-connection serialises this as:
///
/// * `u8`: setting mask (always `0x01`)
/// * `u8`: aux bus ID
/// * `u16`: video source
#[binrw]
#[brw(big)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeAuxSource {
    #[br(temp)]
    #[bw(calc = 0x01)]
    _setting_mask: u8,
    pub aux_bus: u8,
    pub video_source: VideoSource,
}

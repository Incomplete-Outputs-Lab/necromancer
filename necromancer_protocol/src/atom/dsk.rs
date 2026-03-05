//! # Dsk; 6/11 atoms
//!
//! This module implements a minimal subset of downstream keyer (DSK) atoms
//! needed to control on-air state, sources, rate and auto–transition.
//! Remaining atoms are documented below for future implementation.
//!
//! ## Unimplemented atoms (5)
//!
//! FourCC | Atom name | Length
//! ------ | --------- | ------
//! `CDsG` | `ChangeDskShapedClipGain` | 0x14
//! `CDsM` | `ChangeDskMask` | 0x14
//! `DskB` | `DskInputSelection` | 0x10
//! `DskP` | `DskConfigParameters` | 0x1c
//! `DskS` | `DskCurrentState` | 0x10

use crate::structs::VideoSource;
use binrw::binrw;

/// `CDsC`: change DSK cut source (`ChangeDskCut`)
///
/// ## Packet format
///
/// * `u8`: DSK ID
/// * 1 byte padding
/// * `u16`: cut source ID ([VideoSource])
#[binrw]
#[brw(big)]
#[derive(Default, Debug, PartialEq, Eq, Clone, Copy)]
pub struct ChangeDskCut {
    #[brw(pad_after = 1)]
    pub key: u8,
    pub cut_source: VideoSource,
}

/// `CDsF`: change DSK fill source (`ChangeDskFill`)
///
/// ## Packet format
///
/// * `u8`: DSK ID
/// * 1 byte padding
/// * `u16`: fill source ID ([VideoSource])
#[binrw]
#[brw(big)]
#[derive(Default, Debug, PartialEq, Eq, Clone, Copy)]
pub struct ChangeDskFill {
    #[brw(pad_after = 1)]
    pub key: u8,
    pub fill_source: VideoSource,
}

/// `CDsL`: change DSK on-air state (`ChangeDskLive`)
///
/// ## Packet format
///
/// * `u8`: DSK ID
/// * `bool`: on-air
/// * 2 bytes padding
#[binrw]
#[brw(big)]
#[derive(Default, Debug, PartialEq, Eq, Clone, Copy)]
pub struct ChangeDskLive {
    pub key: u8,
    #[brw(pad_after = 2)]
    #[br(map = |v: u8| v != 0)]
    #[bw(map = |v: &bool| Into::<u8>::into(*v))]
    pub on_air: bool,
}

/// `CDsR`: change DSK auto-transition rate (`ChangeDskRate`)
///
/// ## Packet format
///
/// * `u8`: DSK ID
/// * `u8`: rate (frames)
/// * 2 bytes padding
#[binrw]
#[brw(big)]
#[derive(Default, Debug, PartialEq, Eq, Clone, Copy)]
pub struct ChangeDskRate {
    pub key: u8,
    #[brw(pad_after = 1)]
    pub rate: u8,
}

/// `CDsT`: change DSK tie state (`ChangeDskTie`)
///
/// ## Packet format
///
/// * `u8`: DSK ID
/// * `bool`: tie
/// * 2 bytes padding
#[binrw]
#[brw(big)]
#[derive(Default, Debug, PartialEq, Eq, Clone, Copy)]
pub struct ChangeDskTie {
    pub key: u8,
    #[brw(pad_after = 2)]
    #[br(map = |v: u8| v != 0)]
    #[bw(map = |v: &bool| Into::<u8>::into(*v))]
    pub tie: bool,
}

/// `DDsA`: downstream keyer auto–transition (`DoDskAuto`)
///
/// ## Packet format
///
/// * `u8`: DSK ID
/// * 3 bytes padding
#[binrw]
#[brw(big)]
#[derive(Default, Debug, PartialEq, Eq, Clone, Copy)]
pub struct DoDskAuto {
    #[brw(pad_size_to = 4)]
    pub key: u8,
}

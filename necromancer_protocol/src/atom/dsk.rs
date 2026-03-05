//! # Dsk; 6/11 atoms
//!
//! This module implements a minimal subset of downstream keyer (DSK) atoms
//! needed to control on-air state, sources, rate and auto–transition.
//! Remaining atoms are documented below for future implementation.
//!
//! ## Unimplemented atoms (2)
//!
//! FourCC | Atom name | Length
//! ------ | --------- | ------
//! `CDsG` | `ChangeDskShapedClipGain` | 0x14
//! `CDsM` | `ChangeDskMask` | 0x14
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

/// `DskB`: downstream keyer input selection (`DskInputSelection`)
///
/// ## Packet format
///
/// * `u8`: DSK ID
/// * 1 byte padding
/// * `u16`: fill source ID ([VideoSource])
/// * `u16`: cut source ID ([VideoSource])
/// * 2 bytes padding
#[binrw]
#[brw(big)]
#[derive(Default, Debug, PartialEq, Eq, Clone, Copy)]
pub struct DskInputSelection {
    #[brw(pad_after = 1)]
    pub key: u8,
    pub fill_source: VideoSource,
    #[brw(pad_after = 2)]
    pub cut_source: VideoSource,
}

/// `DskP`: downstream keyer configuration parameters (`DskConfigParameters`)
///
/// Mirrors Sofie `DownstreamKeyPropertiesCommand` layout.
///
/// ## Packet format
///
/// * `u8`: DSK ID
/// * `bool`: tie
/// * `u8`: rate
/// * `bool`: pre-multiply
/// * `i16`: clip
/// * `i16`: gain
/// * `bool`: invert
/// * `bool`: mask enabled
/// * `i16`: mask top
/// * `i16`: mask bottom
/// * `i16`: mask left
/// * `i16`: mask right
/// * 2 bytes padding
#[binrw]
#[brw(big)]
#[derive(Default, Debug, PartialEq, Eq, Clone, Copy)]
pub struct DskConfigParameters {
    pub key: u8,
    #[br(map = |v: u8| v != 0)]
    #[bw(map = |v: &bool| Into::<u8>::into(*v))]
    pub tie: bool,
    pub rate: u8,
    #[br(map = |v: u8| v != 0)]
    #[bw(map = |v: &bool| Into::<u8>::into(*v))]
    pub pre_multiply: bool,
    pub clip: i16,
    pub gain: i16,
    #[br(map = |v: u8| v != 0)]
    #[bw(map = |v: &bool| Into::<u8>::into(*v))]
    pub invert: bool,
    #[br(map = |v: u8| v != 0)]
    #[bw(map = |v: &bool| Into::<u8>::into(*v))]
    pub mask_enabled: bool,
    pub mask_top: i16,
    pub mask_bottom: i16,
    pub mask_left: i16,
    #[brw(pad_after = 2)]
    pub mask_right: i16,
}

/// `DskS`: downstream keyer current state (`DskCurrentState`)
///
/// ## Packet format (protocol v8.0.1 and later)
///
/// * `u8`: DSK ID
/// * `bool`: on-air
/// * `bool`: in transition
/// * `bool`: auto transition active
/// * `bool`: transition direction towards on-air
/// * `u8`: remaining frames
#[binrw]
#[brw(big)]
#[derive(Default, Debug, PartialEq, Eq, Clone, Copy)]
pub struct DskCurrentState {
    pub key: u8,
    #[br(map = |v: u8| v != 0)]
    #[bw(map = |v: &bool| Into::<u8>::into(*v))]
    pub on_air: bool,
    #[br(map = |v: u8| v != 0)]
    #[bw(map = |v: &bool| Into::<u8>::into(*v))]
    pub in_transition: bool,
    #[br(map = |v: u8| v != 0)]
    #[bw(map = |v: &bool| Into::<u8>::into(*v))]
    pub is_auto: bool,
    #[br(map = |v: u8| v != 0)]
    #[bw(map = |v: &bool| Into::<u8>::into(*v))]
    pub is_towards_on_air: bool,
    pub remaining_frames: u8,
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::atom::{Atom, Payload};
    use crate::Result;
    use binrw::{BinRead, BinWrite};
    use std::io::Cursor;

    #[test]
    fn roundtrip_dsk_input_selection() -> Result<()> {
        let expected = DskInputSelection {
            key: 0,
            fill_source: VideoSource::Input1,
            cut_source: VideoSource::Input2,
        };
        let atom = Atom::new(expected);
        let mut out = Cursor::new(Vec::new());
        atom.write(&mut out)?;

        let parsed = Atom::read(&mut Cursor::new(out.into_inner()))?;
        let Payload::DskInputSelection(cmd) = parsed.payload else {
            panic!("wrong payload type");
        };
        assert_eq!(expected, cmd);
        Ok(())
    }
}

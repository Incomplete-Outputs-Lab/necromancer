//! # Transitions and digital video effects; 4/21 atoms
//!
//! ## Unimplemented atoms (17)
//!
//! FourCC | Atom name | Length
//! ------ | --------- | ------
//! `CTDp` | `ChangeTransitionDipProperties` | 0x10
//! `CTDv` | `ChangeTransitionDVEProperties` | 0x1c
//! `CTMx` | `ChangeTransitionMixProperties` | 0xc
//! `CTPr` | `ChangeTransitionPreviewTrans` | 0xc
//! `CTPs` | `ChangeTransitionPosition` | 0xc
//! `CTSt` | `ChangeTransitionStingerProperties` | 0x1c
//! `CTTp` | `ChangeTransitionNext` | 0xc  ← implemented
//! `CTWp` | `ChangeTransitionWipeProperties` | 0x1c
//! `DAu2` | `DoTransitionAuto_2` | 0xc
//! `STWV` | `SetTransitionWipeVelocity` | 0x18
//! `TDpP` | `TransitionDipProperties` | 0xc
//! `TDvP` | `TransitionDVEProperties` | 0xc
//! `TMxP` | `TransitionMixProperties` | 0xc
//! `TrPr` | `TransitionPreviewTrans` | 0xc
//! `TrSS` | `TransitionSelectionState` | 0x10
//! `TStP` | `TransitionStingerProperties` | 0x1c
//! `TWpP` | `TransitionWipeProperties` | 0x1c

use crate::structs::{DVETransitionStyle, TransitionType};
use binrw::binrw;

/// `_DVE`: Digital video effects capabilities (`CapabilitiesDVE`)
///
/// ## Packet format
///
/// * `bool`: can rotate
/// * `bool`: can scale up
/// * `u16`: number of supported transition styles
/// * `u8[style_count]`: transition style ID
#[binrw]
#[brw(big)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DVECapabilities {
    #[br(map = |v: u8| v != 0)]
    #[bw(map = |v: &bool| Into::<u8>::into(*v))]
    pub can_rotate: bool,

    #[br(map = |v: u8| v != 0)]
    #[bw(map = |v: &bool| Into::<u8>::into(*v))]
    pub can_scale_up: bool,

    #[br(temp)]
    #[bw(try_calc(u16::try_from(supported_dve_transition_styles.len())))]
    length: u16,

    #[br(count = length)]
    pub supported_dve_transition_styles: Vec<DVETransitionStyle>,
}

/// `DCut`: cut (`DoTransitionCut`)
///
/// Swap program and preview inputs immediately.
///
/// ## Packet format
///
/// * `u8`: ME to cut
/// * 3 bytes padding
#[binrw]
#[brw(big)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cut {
    #[brw(pad_size_to = 4)]
    pub me: u8,
}

/// `DAut`: auto (`DoTransitionAuto`)
///
/// Swap program and preview inputs with a transition.
///
/// ## Packet format
///
/// * `u8`: ME to auto
/// * 3 bytes padding
#[binrw]
#[brw(big)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Auto {
    #[brw(pad_size_to = 4)]
    pub me: u8,
}

/// `CTTp`: change next transition (`ChangeTransitionNext`)
///
/// Sets the type of transition used for the next auto transition on an M/E.
///
/// ## Packet format
///
/// * `u8`: ME index
/// * `u8`: next transition style (TransitionType: Mix=0, Dip=1, Wipe=2, DVE=3, Sting=4)
/// * 2 bytes padding
#[binrw]
#[brw(big)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeTransitionNext {
    pub me: u8,
    #[br(map = |v: u8| TransitionType::try_from(v).unwrap_or(TransitionType::Mix))]
    #[bw(map = |v: &TransitionType| *v as u8)]
    #[brw(pad_after = 2)]
    pub next_transition: TransitionType,
}

/// `TrPs`: transition position (`TransitionCurrentPosition`)
///
/// ## Packet format
///
/// * `u8`: ME
/// * `bool`: transition in progress
/// * `u8`: number of frames of transition remaining
/// * 1 byte padding
/// * `u16`: transition position
/// * 2 bytes padding
#[binrw]
#[brw(big)]
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct TransitionPosition {
    pub me: u8,
    #[br(map = |v: u8| v != 0)]
    #[bw(map = |v: &bool| Into::<u8>::into(*v))]
    pub in_progress: bool,
    #[brw(pad_after = 1)]
    pub frames_remaining: u8,
    /// Transition position (0..=10000)
    #[brw(pad_after = 2)]
    pub position: u16,
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{
        atom::{Atom, Payload},
        packet::{AtemPacket, AtemPacketFlags},
        Result,
    };
    use binrw::{BinRead, BinWrite};
    use std::io::Cursor;

    #[test]
    fn capabilities() -> Result {
        // ATEM Mini
        let cmd = hex::decode("002000005f44564500010011101112131415161718191a1b1c1d1e1f22000000")?;
        let cmd = Atom::read(&mut Cursor::new(&cmd))?;

        let expected = Atom::new(DVECapabilities {
            can_rotate: false,
            can_scale_up: true,
            supported_dve_transition_styles: vec![
                DVETransitionStyle::SqueezeTopLeft,
                DVETransitionStyle::SqueezeTop,
                DVETransitionStyle::SqueezeTopRight,
                DVETransitionStyle::SqueezeLeft,
                DVETransitionStyle::SqueezeRight,
                DVETransitionStyle::SqueezeBottomLeft,
                DVETransitionStyle::SqueezeBottom,
                DVETransitionStyle::SqueezeBottomRight,
                DVETransitionStyle::PushTopLeft,
                DVETransitionStyle::PushTop,
                DVETransitionStyle::PushTopRight,
                DVETransitionStyle::PushLeft,
                DVETransitionStyle::PushRight,
                DVETransitionStyle::PushBottomLeft,
                DVETransitionStyle::PushBottom,
                DVETransitionStyle::PushBottomRight,
                DVETransitionStyle::GraphicLogoWipe,
            ],
        });
        assert_eq!(expected, cmd);

        Ok(())
    }

    #[test]
    fn cut() -> Result {
        let cmd = hex::decode("08188001000000000001000f000c00004443757400000000")?;
        let pkt = AtemPacket::read(&mut Cursor::new(&cmd))?;
        let payload = pkt.atoms().expect("wrong payload type");

        assert_eq!(1, payload.len());
        let Payload::Cut(cut) = &payload[0].payload else {
            panic!("wrong command type");
        };

        assert_eq!(0, cut.me);

        let o = AtemPacket::new_atoms(
            AtemPacketFlags::new().with_ack(true),
            0x8001,
            0,
            0x1,
            0xf,
            vec![Atom::new(Cut { me: 0 })],
        );
        let mut out = Cursor::new(Vec::with_capacity(cmd.len()));
        o.write(&mut out)?;

        assert_eq!(cmd, out.into_inner());
        Ok(())
    }
}

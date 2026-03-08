//! # ATEM commands / atoms
//!
//! Structures here map to `BEPAtom*` and `BEPStruct*` classes in `BMDSwitcherAPI`.
//!
//! Atoms are grouped into modules by their functional area, and re-exported here.
//!
//! Blackmagic's protocol is based on `repr(C)` of various structs (ie: _not packed_), and the
//! alignment is baked into the protocol in strange ways. Both the ATEM hardware and software
//! (`BMDSwitcherAPI.bundle`) leak uninitialised memory into "padding" fields, making round-trip
//! testing _annoying_. These structures are written in machine-native byte order before being
//! converted to big-endian.
//!
//! A lot of the publicly-documented reverse engineering efforts use slightly different terminology,
//! and are incomplete. This implementation is no different in that regard. ;)
//!
//! `necromancer` will _generally_ favour Blackmagic's terminology (unless it's clunky or wrong),
//! and mention third-party names for things where relevant and accurate.
//!
//! ## Progress
//!
//! In BMDSwitcherAPI 9.8.3
//!
//! * total atoms: 344
//! * implemented atoms: 69 (20%)
//! * identified atoms: 344 (100%)
//!
//! ## Uncategorised unimplemented atoms (8)
//!
//! <div class="warning">
//!
//! **LIST IS NOT YET COMPLETE**
//!
//! </div>
//!
//! FourCC | Atom name | Length
//! ------ | --------- | ------
//! `C3sl` | `ChangeSDI3GOutputLevel` | 0xc
//! `ClrM` | `ColorimetryMode` | 0xc
//! `Powr` | `PowerStatus` | 0xc
//! `RInL` | `ResetInputLabels` | 0xc
//! `SPtM` | `SerialPortFunction` | 0xc
//! `V3sl` | `CurrentSDI3GOutputLevel` | 0xc
//! `Warn` | `WarningMessage` | 0x34
//! `Whol` | `IdentityInformation` | 0xb8

mod audio;
#[path = "aux_.rs"]
mod aux;
mod camera;
mod colour;
mod down_convert;
mod dsk;
mod fairlight;
mod ftb;
mod hyperdeck;
mod initialisation;
mod inpr;
mod key;
mod macros;
mod media_player;
mod mfg_test;
mod mix_effect;
mod multiview;
mod network;
mod recording;
mod remote_source;
mod rtmp;
mod settings;
mod storage;
mod super_source;
mod talkback;
mod tally;
mod time;
mod topology;
mod transitions;
mod ver;
mod video_mode;
mod visca;

use crate::{packet::AtemPacket, util::OffsetCounter, Result};
use binrw::{binrw, helpers::until_eof, io::TakeSeekExt};
use std::{fmt::Debug, io::SeekFrom};

pub use self::{
    aux::{AuxSource, ChangeAuxSource},
    camera::{CameraCommand, CameraControl},
    colour::{ColourGeneratorParams, SetColourGeneratorParams},
    fairlight::{
        CapabilitiesFairlightAudioMixer, CapabilitiesFairlightAudioMixerHeadphoneOut,
        FairlightAudioMixerInputSourceProperties,
        FairlightAudioMixerMasterOutEqualiserBandProperties, FairlightAudioMixerTally,
        FairlightEqualiserBandRangeCapabilities, HeadphoneOutputCapabilities,
    },
    dsk::{
        ChangeDskCut, ChangeDskFill, ChangeDskLive, ChangeDskRate, ChangeDskTie, DoDskAuto,
        DskConfigParameters, DskCurrentState, DskInputSelection,
    },
    ftb::{CutToBlack, FadeToBlackAuto, FadeToBlackParams, FadeToBlackStatus, SetFadeToBlackParams},
    initialisation::InitialisationComplete,
    inpr::InputProperties,
    macros::MacroCapabilities,
    media_player::{
        CaptureStill, MediaPlayerCapabilities, MediaPlayerFrameDescription, MediaPlayerSource,
        MediaPlayerSourceID, SetMediaPlayerSource, CAPTURE_STILL,
    },
    mfg_test::{MfgTest, MfgTestResult},
    mix_effect::{
        MixEffectBlockCapabilities, PreviewInput, ProgramInput, SetPreviewInput, SetProgramInput,
    },
    recording::{
        RecordToMedia, RecordToMediaDurationRequest, RecordToMediaRecordingTimecode,
        RecordToMediaStatus, RecordToMediaSwitchDisk, RECORD_TO_MEDIA_DURATION_REQUEST,
        RECORD_TO_MEDIA_SWITCH_DISK,
    },
    remote_source::{RemoteSourceForceInternetProbe, REMOTE_SOURCE_FORCE_INTERNET_PROBE},
    rtmp::{RtmpDurationRequest, RTMP_DURATION_REQUEST},
    settings::{
        ClearSettings, RestoreSettings, SaveSettings, CLEAR_STARTUP_SETTINGS,
        RESTORE_STARTUP_SETTINGS, SAVE_STARTUP_SETTINGS,
    },
    storage::{
        ClearMediaPool, FileTransferChunkParams, FileTransferError, FileType, FinishFileDownload,
        LockObtained, MediaPoolLock, MediaPoolLockStatus, SetupFileDownload, SetupFileUpload,
        TransferAck, TransferChunk, TransferCompleted, CLEAR_MEDIA_POOL,
    },
    tally::TalliedSources,
    time::{
        SetTimeOfDay, SetTimecodeConfig, Time, TimeMode, TimecodeConfig, TimecodeRequest,
        TIMECODE_REQUEST,
    },
    topology::Topology,
    transitions::{Auto, ChangeTransitionNext, Cut, DVECapabilities, TransitionPosition},
    ver::{ProductName, Version},
    video_mode::{CoreVideoMode, SetVideoMode, SupportedVideoModes},
    visca::{Visca422AutoAllocateAddresses, VISCA_422_AUTO_ALLOCATE_ADDRESSES},
};

/// Structure for BEP atoms.
///
/// This includes commands sent by the client to the switcher, and events from the switcher sent to
/// the client.
///
/// ## Format
///
/// * `u16`: atom length, minimum 8
/// * 2 bytes padding
/// * 4 bytes: atom type identifier
/// * `(length - 8)` bytes: payload
///
/// The atom type identifier is parsed as `magic` in [`Payload`][].
#[binrw]
#[derive(Clone, PartialEq, Eq)]
#[brw(big)]
#[bw(stream = r, map_stream = OffsetCounter::new)]
pub struct Atom {
    // Length for the read path
    #[br(temp, pad_after = 2)]
    #[br(assert((Self::HEADERS_LENGTH..=Self::MAX_ATOM_LENGTH).contains(&length)))]
    #[bw(ignore)]
    length: u16,

    /// Atom payload.
    // On read, length includes 4 extra bytes (length field and padding).
    #[br(map_stream = |reader| { reader.take_seek(u64::from(length) - 4) }, pad_size_to = length - 4)]
    // On write, we haven't written the `length` field yet, and we'll come back to it.
    #[bw(pad_before = 4)]
    pub payload: Payload,

    // Length field for the write path
    #[br(ignore)]
    // On write, r.total() includes all headers
    #[bw(assert(r.total() <= (Self::MAX_ATOM_LENGTH as u64)))]
    #[bw(try_calc(u16::try_from(r.total())))]
    #[bw(seek_before = SeekFrom::Current(-(r.total() as i64)), restore_position)]
    length: u16,
}

impl std::fmt::Debug for Atom {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Atom")
            .field("payload", &self.payload)
            .finish()
    }
}

macro_rules! atom_payloads {
    (
        $($magic:expr => $variant:ident,)*
    ) => {
        /// [`Atom`][] payload variants.
        ///
        /// When read or written with `binrw` traits, the first 4 bytes contain the atom's type
        /// identifier (`magic`), and all following bytes are the payload variant's parameters
        /// (which may be empty, if the [`Atom`'s][Atom] length is 8).
        #[binrw]
        #[brw(big)]
        #[derive(Clone, PartialEq, Eq)]
        pub enum Payload {
            $(
                #[doc = concat!("`", stringify!($magic), "`: [`", stringify!($variant), "`][]")]
                #[brw(magic = $magic)]
                $variant($variant),
            )*

            /// Unknown payload type.
            ///
            /// The first parameter is the atom type, the second is the payload.
            Unknown([u8; 4], #[br(parse_with = until_eof)] Vec<u8>),
        }

        $(
            impl From<$variant> for Payload {
                fn from(p: $variant) -> Payload {
                    Payload::$variant(p)
                }
            }
        )*

        impl Debug for Payload {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                match self {
                    $(
                        Self::$variant(v) => f
                            .debug_tuple(stringify!($variant))
                            .field(v)
                            .finish(),
                    )*
                    Self::Unknown(cmd, payload) => f
                        .debug_tuple("Unknown")
                        .field(&cmd.escape_ascii().to_string())
                        .field(&hex::encode(payload))
                        .finish(),
                }
            }
        }
    }
}

atom_payloads!(
    b"_DVE" => DVECapabilities,
    b"_FAC" => CapabilitiesFairlightAudioMixer,
    b"_FEC" => FairlightEqualiserBandRangeCapabilities,
    b"_FMH" => CapabilitiesFairlightAudioMixerHeadphoneOut,
    b"_MAC" => MacroCapabilities,
    b"_MeC" => MixEffectBlockCapabilities,
    b"_mpl" => MediaPlayerCapabilities,
    b"_pin" => ProductName,
    b"_top" => Topology,
    b"_ver" => Version,
    b"_VMC" => SupportedVideoModes,
    b"AMBP" => FairlightAudioMixerMasterOutEqualiserBandProperties,
    b"AuxS" => AuxSource,
    b"Capt" => CaptureStill,
    b"CCdP" => CameraControl,
    b"CClV" => SetColourGeneratorParams,
    b"CCmd" => CameraCommand,
    b"CLMP" => ClearMediaPool,
    b"ColV" => ColourGeneratorParams,
    b"CPgI" => SetProgramInput,
    b"CPvI" => SetPreviewInput,
    b"CAuS" => ChangeAuxSource,
    b"CTCC" => SetTimecodeConfig,
    b"CTTp" => ChangeTransitionNext,
    b"CVdM" => SetVideoMode,
    b"CDsC" => ChangeDskCut,
    b"CDsF" => ChangeDskFill,
    b"CDsL" => ChangeDskLive,
    b"CDsR" => ChangeDskRate,
    b"CDsT" => ChangeDskTie,
    b"DAut" => Auto,
    b"DCut" => Cut,
    b"FASP" => FairlightAudioMixerInputSourceProperties,
    b"DDsA" => DoDskAuto,
    b"DskB" => DskInputSelection,
    b"DskP" => DskConfigParameters,
    b"DskS" => DskCurrentState,
    b"FCut" => CutToBlack,
    b"FMTl" => FairlightAudioMixerTally,
    b"FtbA" => FadeToBlackAuto,
    b"FtbC" => SetFadeToBlackParams,
    b"FtbP" => FadeToBlackParams,
    b"FtbS" => FadeToBlackStatus,
    b"FTCD" => FileTransferChunkParams,
    b"FTDa" => TransferChunk,
    b"FTDC" => TransferCompleted,
    b"FTDE" => FileTransferError,
    b"FTFD" => FinishFileDownload,
    b"FTSD" => SetupFileDownload,
    b"FTSU" => SetupFileUpload,
    b"FTUA" => TransferAck,
    b"InCm" => InitialisationComplete,
    b"InPr" => InputProperties,
    b"LKOB" => LockObtained,
    b"LKST" => MediaPoolLockStatus,
    b"LOCK" => MediaPoolLock,
    b"MfgR" => MfgTestResult,
    b"MfgT" => MfgTest,
    b"MPCE" => MediaPlayerSource,
    b"MPfe" => MediaPlayerFrameDescription,
    b"MPSS" => SetMediaPlayerSource,
    b"PrgI" => ProgramInput,
    b"PrvI" => PreviewInput,
    b"PZSA" => Visca422AutoAllocateAddresses,
    b"RcTM" => RecordToMedia,
    b"RMDR" => RecordToMediaDurationRequest,
    b"RMSp" => RecordToMediaSwitchDisk,
    b"RSip" => RemoteSourceForceInternetProbe,
    b"RTMR" => RecordToMediaRecordingTimecode,
    b"RTMS" => RecordToMediaStatus,
    b"SRcl" => ClearSettings,
    b"SRDR" => RtmpDurationRequest,
    b"SRrs" => RestoreSettings,
    b"SRsv" => SaveSettings,
    b"SToD" => SetTimeOfDay,
    b"TCCc" => TimecodeConfig,
    b"Time" => Time,
    b"TiRq" => TimecodeRequest,
    b"TlSr" => TalliedSources,
    b"TrPs" => TransitionPosition,
    b"VidM" => CoreVideoMode,
);

impl Atom {
    /// Minimum size of an [Atom], including all headers (length + padding + magic).
    const HEADERS_LENGTH: u16 = 8;

    /// Maximum size of an [Atom], including all headers (length + padding + magic).
    const MAX_ATOM_LENGTH: u16 = AtemPacket::MAX_PAYLOAD_LENGTH;

    /// Maximum command payload size (minus [Atom] headers).
    const MAX_PAYLOAD_LENGTH: u16 = Self::MAX_ATOM_LENGTH - Self::HEADERS_LENGTH;

    pub fn new(payload: impl Into<Payload>) -> Self {
        Self {
            payload: payload.into(),
        }
    }
}

/// Parses a byte slice as an _optionally_-null-terminated, UTF-8-encoded
/// string, ignoring all bytes after the first null.
///
/// Returns [`Error::Utf8`][1] on UTF-8 encoding errors.
///
/// This is similar to [`CStr::from_bytes_until_nul`][0], but does not require a
/// null terminator.
///
/// [0]: std::ffi::CStr::from_bytes_until_nul
/// [1]: crate::Error::Utf8
fn str_from_utf8_null(p: &[u8]) -> Result<&str> {
    let p = p.split(|c| *c == 0).next().unwrap_or(p);
    Ok(std::str::from_utf8(p)?)
}

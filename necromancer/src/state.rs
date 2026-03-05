use crate::{
    protocol::{
        atom::{
            Atom, ColourGeneratorParams, DskConfigParameters, DskCurrentState, DskInputSelection,
            FadeToBlackStatus, FairlightAudioMixerInputSourceProperties, InputProperties,
            MediaPlayerCapabilities, MediaPlayerFrameDescription, MediaPlayerSourceID,
            MixEffectBlockCapabilities, Payload, ProductName, Topology, TransitionPosition, Version,
        },
        structs::{DVETransitionStyle, EqualiserRange, TallyFlags, VideoMode, VideoSource},
    },
    Result,
};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    ops::RangeInclusive,
};

/// Configuration properties for a downstream keyer.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DskProperties {
    pub tie: bool,
    pub rate: u8,
    pub pre_multiply: bool,
    pub clip: i16,
    pub gain: i16,
    pub invert: bool,
    pub mask_enabled: bool,
    pub mask_top: i16,
    pub mask_bottom: i16,
    pub mask_left: i16,
    pub mask_right: i16,
}

/// Runtime state for a downstream keyer.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DskRuntimeState {
    pub on_air: bool,
    pub in_transition: bool,
    pub is_auto: bool,
    pub is_towards_on_air: bool,
    pub remaining_frames: u8,
}

bitflags! {
    #[derive(Debug, Default, PartialEq, Eq, Clone, Copy)]
    pub struct StateUpdate: u32 {
        /// The switcher reports that initialisation has completed, and state
        /// is complete.
        const INITIALISATION_COMPLETE        = 1;
        const PRODUCT_NAME                   = 1 << 1;
        const VERSION                        = 1 << 2;
        const TOPOLOGY                       = 1 << 3;
        const PROGRAM_SOURCE                 = 1 << 4;
        const PREVIEW_SOURCE                 = 1 << 5;
        const TRANSITION_POSITION            = 1 << 6;
        const TALLY_BY_SOURCE                = 1 << 7;
        const SUPPORTED_VIDEO_MODES          = 1 << 8;
        const VIDEO_MODE                     = 1 << 9;
        const INPUT_PROPERTIES               = 1 << 10;
        const FADE_TO_BLACK_STATUS           = 1 << 11;
        const FADE_TO_BLACK_RATE             = 1 << 12;
        const MEDIA_PLAYER_CAPABILITIES      = 1 << 13;
        const MEDIA_PLAYER_FRAME_DESCRIPTION = 1 << 14;
        const COLOUR_GENERATOR_PARAMS        = 1 << 15;
        const MIX_EFFECT_BLOCK_CAPABILITIES  = 1 << 16;
        const MEDIA_PLAYER_SOURCE            = 1 << 17;
        const FAIRLIGHT_TALLY                = 1 << 18;
        const FAIRLIGHT_INPUT_SOURCE_PROPS   = 1 << 19;
        const FAIRLIGHT_FREQUENCY_RANGES     = 1 << 20;
        const DVE_CAPABILITIES               = 1 << 21;
        const DSK_SOURCES                    = 1 << 22;
        const DSK_PROPERTIES                 = 1 << 23;
        const DSK_STATE                      = 1 << 24;

        const PREVIEW_OR_PROGRAM_SOURCE = Self::PREVIEW_SOURCE.bits() | Self::PROGRAM_SOURCE.bits();
        const UNSUPPORTED_COMMAND            = 1 << 31;
    }
}

/// Maximum number of supported MEs.
const MAX_MES: usize = 8;

/// Maximum number of supported colour generators.
const MAX_COLOUR_GENERATORS: u8 = 8;

/// Maximum number of supported media players.
const MAX_MEDIA_PLAYERS: u8 = 8;

/// [AtemState] stores all state from [AtemController] events.
///
/// [AtemController]: crate::controller::AtemController
#[derive(Default, Clone)]
pub struct AtemState {
    // TODO: replace Hashmaps in this structure with Vec or simple arrays.
    /// Switcher intialisation completed.
    pub initialisation_complete: bool,
    /// The switcher's product name.
    pub product_name: ProductName,
    /// The switcher's firmware / protocol version.
    pub version: Version,
    /// The topology of the switcher.
    pub topology: Topology,

    me_capabilities: [MixEffectBlockCapabilities; MAX_MES],
    program_source: [VideoSource; MAX_MES],
    preview_source: [VideoSource; MAX_MES],
    /// Transition position for each ME.
    pub transition_position: HashMap<u8, TransitionPosition>,
    /// Current tally state for each source.
    pub tally_by_source: HashMap<VideoSource, TallyFlags>,
    /// List of all video modes supported by the switcher.
    pub supported_video_modes: Vec<VideoMode>,
    /// Current video mode.
    pub video_mode: VideoMode,
    /// Input properties.
    pub input_properties: HashMap<VideoSource, InputProperties>,
    fade_to_black_status: [FadeToBlackStatus; MAX_MES],
    fade_to_black_rates: [u8; MAX_MES],
    pub media_player_capabilities: MediaPlayerCapabilities,

    /// List of media player sources.
    ///
    /// Entries are set to `None` when there is no available information,
    /// after a Topology message but before a MediaPlayerSource message.
    media_player_sources: [Option<MediaPlayerSourceID>; MAX_MEDIA_PLAYERS as usize],

    /// Media player frame descriptions. Slots are 0-indexed.
    ///
    /// Indexes here are `u8` as [MediaPlayerCapabilities] indicates a maximum
    /// clip count as `u8`.
    ///
    /// [MediaPlayerFrameDescription] represents it as `u16`, [SetupFileUpload]
    /// and [SetupFileDownload] represent it as `u32`.
    ///
    /// [SetupFileUpload]: crate::protocol::atom::SetupFileUpload
    /// [SetupFileDownload]: crate::protocol::atom::SetupFileDownload
    pub media_player_frame_descriptions: HashMap<u8, MediaPlayerFrameDescription>,

    /// Colour generator configurations.
    ///
    /// Entries are 0-indexed (ie: `colour_generator[0]` == [VideoSource::Colour1]).
    colour_generator_params: [ColourGeneratorParams; MAX_COLOUR_GENERATORS as usize],
    /// Number of colour generators.
    pub colour_generators: u8,

    /// Previously-observed unsupported commands that we shouldn't warn about
    /// again.
    pub unsupported_commands: HashSet<[u8; 4]>,

    /// Total number of observed unsupported commands.
    pub unsupported_command_count: usize,

    /// Tally state of each Fairlight audio mixer input.
    pub fairlight_audio_mixer_tally: BTreeMap<u16, bool>,

    /// Properties for each Fairlight audio mixer input.
    pub fairlight_audio_mixer_input_props: BTreeMap<u16, FairlightAudioMixerInputSourceProperties>,

    /// Supported Fairlight audio equaliser frequency ranges.
    pub fairlight_audio_frequency_ranges: BTreeMap<EqualiserRange, RangeInclusive<u32>>,

    pub dve_can_rotate: bool,
    pub dve_can_scale_up: bool,
    pub dve_supported_transition_styles: HashSet<DVETransitionStyle>,

    /// Downstream keyer sources (fill, cut) by key index.
    pub dsk_sources: HashMap<u8, (VideoSource, VideoSource)>,

    /// Downstream keyer configuration properties.
    pub dsk_properties: HashMap<u8, DskProperties>,

    /// Downstream keyer runtime state (on-air, transition status, etc).
    pub dsk_state: HashMap<u8, DskRuntimeState>,
}

impl AtemState {
    /// Parses a stream of [Atom] and updates our internal state.
    ///
    /// Returns `true` if internal state was updated.
    pub fn update_state(&mut self, cmds: &[Atom]) -> Result<StateUpdate> {
        let mut updated_fields = StateUpdate::empty();
        for cmd in cmds.iter() {
            let pl = &cmd.payload;
            trace!("<<< {pl:?}");
            match pl {
                Payload::Unknown(command_name, _) => {
                    self.unsupported_command_count += 1;
                    if self.unsupported_commands.insert(*command_name) {
                        warn!(
                            "first sighting of unsupported event: b\"{}\"",
                            command_name.escape_ascii()
                        );
                    }
                    updated_fields |= StateUpdate::UNSUPPORTED_COMMAND;
                    continue;
                }

                Payload::InitialisationComplete(_) => {
                    self.initialisation_complete = true;
                    updated_fields |= StateUpdate::INITIALISATION_COMPLETE;
                }

                Payload::ProductName(name) => {
                    self.product_name = name.clone();
                    debug!(?self.product_name, "updated");
                    updated_fields |= StateUpdate::PRODUCT_NAME;
                }

                Payload::Version(ver) => {
                    ver.check_firmware_version()?;
                    self.version = *ver;
                    debug!(?self.version, "updated");
                    updated_fields |= StateUpdate::VERSION;
                }

                Payload::Topology(top) => {
                    self.topology = top.clone();
                    debug!(?self.topology, "updated");
                    if self.topology.mes as usize > MAX_MES {
                        warn!(
                            "device reports {} MEs, but this library only supports {MAX_MES}",
                            self.topology.mes
                        );
                    }
                    if self.tally_by_source.is_empty() {
                        self.tally_by_source.reserve(self.topology.sources as usize);
                    }
                    if self.input_properties.is_empty() {
                        self.input_properties
                            .reserve(self.topology.sources as usize);
                    }
                    if self.topology.media_players > MAX_MEDIA_PLAYERS {
                        warn!(
                            "device reports {} media players, but this library only supports {MAX_MEDIA_PLAYERS}",
                            self.topology.media_players,
                        );
                    }
                    updated_fields |= StateUpdate::TOPOLOGY;
                }

                Payload::MixEffectBlockCapabilities(mec) => {
                    let me = mec.me as usize;
                    debug!(?mec, "updated ME capabilities");
                    if me >= self.me_capabilities.len() {
                        continue;
                    }
                    self.me_capabilities[me] = *mec;
                    updated_fields |= StateUpdate::MIX_EFFECT_BLOCK_CAPABILITIES;
                }

                Payload::ProgramInput(pi) => {
                    let me = pi.me as usize;
                    debug!(?pi, "updated program source");
                    if me >= self.program_source.len() {
                        continue;
                    }
                    self.program_source[me] = pi.video_source;
                    updated_fields |= StateUpdate::PROGRAM_SOURCE;
                }

                Payload::PreviewInput(pi) => {
                    let me = pi.me as usize;
                    debug!(?pi, "updated preview source");
                    if me >= self.preview_source.len() {
                        continue;
                    }
                    self.preview_source[me] = pi.video_source;
                    updated_fields |= StateUpdate::PREVIEW_SOURCE;
                }

                Payload::TransitionPosition(pos) => {
                    self.transition_position.insert(pos.me, pos.clone());
                    debug!(?pos, "updated transition position");
                    updated_fields |= StateUpdate::TRANSITION_POSITION;
                }

                Payload::TalliedSources(tally) => {
                    self.tally_by_source = tally.clone().into();
                    debug!(?self.tally_by_source, "updated");
                    updated_fields |= StateUpdate::TALLY_BY_SOURCE;
                }

                Payload::SupportedVideoModes(vmc) => {
                    self.supported_video_modes = vmc.clone().into();
                    debug!(?self.supported_video_modes, "updated");
                    updated_fields |= StateUpdate::SUPPORTED_VIDEO_MODES;
                }

                Payload::CoreVideoMode(vm) => {
                    self.video_mode = vm.clone().into();
                    debug!(?self.video_mode, "updated");
                    updated_fields |= StateUpdate::VIDEO_MODE;
                }

                Payload::InputProperties(inpr) => {
                    debug!(?inpr, "updated input property");
                    if let Some(colour_generator_id) = inpr.colour_generator_id() {
                        if colour_generator_id < MAX_COLOUR_GENERATORS
                            && colour_generator_id >= self.colour_generators
                        {
                            self.colour_generators = colour_generator_id + 1;
                        }
                    }

                    self.input_properties
                        .insert(inpr.video_source, inpr.clone());
                    updated_fields |= StateUpdate::INPUT_PROPERTIES;
                }

                Payload::FadeToBlackStatus(ftbs) => {
                    let me = ftbs.me as usize;
                    debug!(?ftbs, "updated fade to black status");
                    if me >= self.fade_to_black_status.len() {
                        continue;
                    }
                    self.fade_to_black_status[me] = *ftbs;
                    updated_fields |= StateUpdate::FADE_TO_BLACK_STATUS;
                }

                Payload::FadeToBlackParams(ftbp) => {
                    let me = ftbp.me as usize;
                    debug!(?ftbp, "updated fade to black rate");
                    if me >= self.fade_to_black_rates.len() {
                        continue;
                    }
                    self.fade_to_black_rates[me] = ftbp.rate;
                    updated_fields |= StateUpdate::FADE_TO_BLACK_RATE;
                }

                Payload::MediaPlayerCapabilities(mpl) => {
                    self.media_player_capabilities = *mpl;
                    debug!(?self.media_player_capabilities, "updated");
                    updated_fields |= StateUpdate::MEDIA_PLAYER_CAPABILITIES;
                }

                Payload::MediaPlayerFrameDescription(mpfe) => {
                    debug!(?mpfe, "updated media player frame description");
                    if mpfe.store_id != 0 || mpfe.index > 0xff {
                        continue;
                    }
                    self.media_player_frame_descriptions
                        .insert(mpfe.index as u8, mpfe.clone());
                    updated_fields |= StateUpdate::MEDIA_PLAYER_FRAME_DESCRIPTION;
                }

                Payload::MediaPlayerSource(mpce) => {
                    debug!(?mpce, "updated media player source");
                    if mpce.id >= MAX_MEDIA_PLAYERS || mpce.id >= self.topology.media_players {
                        continue;
                    }
                    self.media_player_sources[usize::from(mpce.id)] = Some(mpce.source);
                    updated_fields |= StateUpdate::MEDIA_PLAYER_SOURCE;
                }

                Payload::ColourGeneratorParams(colv) => {
                    debug!(?colv, "updated colour generator params");
                    if colv.id >= MAX_COLOUR_GENERATORS {
                        continue;
                    }
                    if colv.id >= self.colour_generators {
                        self.colour_generators = colv.id + 1;
                    }

                    self.colour_generator_params[usize::from(colv.id)] = *colv;
                    updated_fields |= StateUpdate::COLOUR_GENERATOR_PARAMS;
                }

                Payload::FairlightAudioMixerTally(fmtl) => {
                    debug!(?fmtl, "updated fairlight audio mixer tally");
                    for e in fmtl.entries.iter() {
                        self.fairlight_audio_mixer_tally
                            .insert(e.source_id, e.active);
                    }
                    updated_fields |= StateUpdate::FAIRLIGHT_TALLY;
                }

                Payload::FairlightAudioMixerInputSourceProperties(fasp) => {
                    debug!(
                        ?fasp,
                        "updated fairlight audio mixer input source properties"
                    );
                    self.fairlight_audio_mixer_input_props
                        .insert(fasp.source_id, fasp.clone());
                    updated_fields |= StateUpdate::FAIRLIGHT_INPUT_SOURCE_PROPS;
                }

                Payload::FairlightEqualiserBandRangeCapabilities(fec) => {
                    debug!(?fec, "updated fairlight audio mixer equaliser band ranges");

                    for limit in fec.limits.clone() {
                        self.fairlight_audio_frequency_ranges
                            .insert(limit.range, limit.into());
                    }
                    updated_fields |= StateUpdate::FAIRLIGHT_FREQUENCY_RANGES;
                }

                Payload::DVECapabilities(dve) => {
                    debug!(?dve, "updated DVE capabilities");
                    self.dve_can_rotate = dve.can_rotate;
                    self.dve_can_scale_up = dve.can_scale_up;
                    self.dve_supported_transition_styles =
                        HashSet::from_iter(dve.supported_dve_transition_styles.iter().copied());

                    updated_fields |= StateUpdate::DVE_CAPABILITIES;
                }

                Payload::DskInputSelection(sel) => {
                    debug!(?sel, "updated DSK input selection");
                    self.dsk_sources
                        .insert(sel.key, (sel.fill_source, sel.cut_source));
                    updated_fields |= StateUpdate::DSK_SOURCES;
                }

                Payload::DskConfigParameters(cfg) => {
                    debug!(?cfg, "updated DSK config parameters");
                    let props = DskProperties {
                        tie: cfg.tie,
                        rate: cfg.rate,
                        pre_multiply: cfg.pre_multiply,
                        clip: cfg.clip,
                        gain: cfg.gain,
                        invert: cfg.invert,
                        mask_enabled: cfg.mask_enabled,
                        mask_top: cfg.mask_top,
                        mask_bottom: cfg.mask_bottom,
                        mask_left: cfg.mask_left,
                        mask_right: cfg.mask_right,
                    };
                    self.dsk_properties.insert(cfg.key, props);
                    updated_fields |= StateUpdate::DSK_PROPERTIES;
                }

                Payload::DskCurrentState(st) => {
                    debug!(?st, "updated DSK state");
                    let runtime = DskRuntimeState {
                        on_air: st.on_air,
                        in_transition: st.in_transition,
                        is_auto: st.is_auto,
                        is_towards_on_air: st.is_towards_on_air,
                        remaining_frames: st.remaining_frames,
                    };
                    self.dsk_state.insert(st.key, runtime);
                    updated_fields |= StateUpdate::DSK_STATE;
                }

                _ => (),
            }
        }

        Ok(updated_fields)
    }

    /// Get the capabilities of a given ME.
    pub const fn get_me_capabilities(&self, me: u8) -> Option<MixEffectBlockCapabilities> {
        if me >= self.topology.mes {
            return None;
        }
        let me = me as usize;
        if me >= MAX_MES {
            return None;
        }
        Some(self.me_capabilities[me])
    }

    /// Get the current program source for the given ME.
    ///
    /// Returns `None` if the `me` is invalid for this switcher's topology.
    pub const fn get_program_source(&self, me: u8) -> Option<VideoSource> {
        if me >= self.topology.mes {
            return None;
        }
        let me = me as usize;
        if me >= MAX_MES {
            return None;
        }
        Some(self.program_source[me])
    }

    /// Get the program sources for all MEs.
    pub fn get_program_sources(&self) -> &[VideoSource] {
        &self.program_source[0..self.topology.mes as usize]
    }

    /// Get the current preview source for the given ME.
    ///
    /// Returns `None` if the `me` is invalid for this switcher's topology.
    pub const fn get_preview_source(&self, me: u8) -> Option<VideoSource> {
        if me >= self.topology.mes {
            return None;
        }
        let me = me as usize;
        if me >= MAX_MES {
            return None;
        }
        Some(self.preview_source[me])
    }

    /// Get the preview sources for all MEs.
    pub fn get_preview_sources(&self) -> &[VideoSource] {
        &self.preview_source[0..self.topology.mes as usize]
    }

    pub const fn get_fade_to_black_status(&self, me: u8) -> Option<FadeToBlackStatus> {
        if me >= self.topology.mes {
            return None;
        }
        let me = me as usize;
        if me >= MAX_MES {
            return None;
        }
        Some(self.fade_to_black_status[me])
    }

    /// Get the fade-to-black statuses for all MEs.
    pub fn get_fade_to_black_statuses(&self) -> &[FadeToBlackStatus] {
        &self.fade_to_black_status[0..self.topology.mes as usize]
    }

    pub const fn get_fade_to_black_rate(&self, me: u8) -> Option<u8> {
        if me >= self.topology.mes {
            return None;
        }
        let me = me as usize;
        if me >= MAX_MES {
            return None;
        }
        Some(self.fade_to_black_rates[me])
    }

    /// Get the fade-to-black rates for all MEs.
    pub fn get_fade_to_black_rates(&self) -> &[u8] {
        &self.fade_to_black_rates[0..self.topology.mes as usize]
    }

    pub fn get_colour_generator(&self, id: u8) -> Option<ColourGeneratorParams> {
        if id >= self.colour_generators || id >= MAX_COLOUR_GENERATORS {
            return None;
        }
        Some(self.colour_generator_params[usize::from(id)])
    }

    /// Get all colour generator parameters.
    pub fn get_colour_generators(&self) -> &[ColourGeneratorParams] {
        &self.colour_generator_params[0..self.colour_generators as usize]
    }

    /// Gets the current source for a given media player `id`.
    pub fn get_media_player_source(&self, id: u8) -> Option<MediaPlayerSourceID> {
        if id >= self.topology.media_players || id >= MAX_MEDIA_PLAYERS {
            return None;
        }
        self.media_player_sources[usize::from(id)]
    }

    /// Get all media player sources.
    pub fn get_media_player_sources(&self) -> &[Option<MediaPlayerSourceID>] {
        &self.media_player_sources[0..self.topology.media_players as usize]
    }
}

impl std::fmt::Debug for AtemState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AtemState")
            .field("initialisation_complete", &self.initialisation_complete)
            .field("product_name", &self.product_name)
            .field("version", &self.version)
            .field("topology", &self.topology)
            .field(
                "me_capabilities",
                &&self.me_capabilities[..MAX_MES.min(self.topology.mes as usize)],
            )
            .field(
                "program_source",
                &&self.program_source[..MAX_MES.min(self.topology.mes as usize)],
            )
            .field(
                "preview_source",
                &&self.preview_source[..MAX_MES.min(self.topology.mes as usize)],
            )
            .field("transition_position", &self.transition_position)
            .field("tally_by_source", &self.tally_by_source)
            .field("supported_video_modes", &self.supported_video_modes)
            .field("input_properties", &self.input_properties)
            .field("video_mode", &self.video_mode)
            .field(
                "fade_to_black_status",
                &&self.fade_to_black_status[..MAX_MES.min(self.topology.mes as usize)],
            )
            .field(
                "fade_to_black_rates",
                &&self.fade_to_black_rates[..MAX_MES.min(self.topology.mes as usize)],
            )
            .field("media_player_capabilities", &self.media_player_capabilities)
            .field(
                "media_player_frame_descriptions",
                &self.media_player_frame_descriptions,
            )
            .field(
                "media_player_sources",
                &&self.media_player_sources
                    [..(MAX_MEDIA_PLAYERS.min(self.topology.media_players) as usize)],
            )
            .field(
                "colour_generators",
                &&self.colour_generator_params
                    [..(MAX_COLOUR_GENERATORS.min(self.colour_generators) as usize)],
            )
            .field("unsupported_command_count", &self.unsupported_command_count)
            .field(
                "fairlight_audio_mixer_tally",
                &self.fairlight_audio_mixer_tally,
            )
            .field(
                "fairlight_audio_mixer_input_props",
                &self.fairlight_audio_mixer_input_props,
            )
            .finish()
    }
}

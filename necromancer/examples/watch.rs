use clap::Parser;
use necromancer::{AtemController, Result, StateUpdate};
use std::net::SocketAddrV4;
use tracing::{info, level_filters::LevelFilter};
use tracing_subscriber::EnvFilter;

/// ATEM event watcher.
#[derive(Debug, Parser)]
#[clap(verbatim_doc_comment)]
struct CliParser {
    /// IP address of the ATEM switcher.
    #[clap(short, long)]
    pub ip: String,

    /// Automatically reconnect on connection loss.
    #[clap(short, long)]
    pub reconnect: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::builder()
                .with_default_directive(LevelFilter::INFO.into())
                .from_env_lossy(),
        )
        .compact()
        .init();
    let opts = CliParser::parse();
    let atem = AtemController::connect_udp(
        SocketAddrV4::new(opts.ip.parse().unwrap(), 9910),
        opts.reconnect,
    )
    .await?;
    let state = atem.get_state().await;
    info!(
        "Connected ATEM switch: {:?}, FW v{}",
        state.product_name, state.version
    );
    info!("Video mode: {}", state.video_mode);
    for me in 0..state.topology.mes {
        let pgm = state.get_program_source(me).unwrap_or_default();
        let pre = state.get_preview_source(me).unwrap_or_default();

        info!("ME {me}: Program {pgm:?}, Preview {pre:?}");
    }
    if state.topology.auxs > 0 {
        for aux in 0..state.topology.auxs {
            let src = state.get_aux_source(aux).unwrap_or_default();
            info!("AUX {aux}: {src:?}");
        }
    }
    info!(
        "Supported DVE transition styles (scale={:?}, rotate={:?}): {:?}",
        state.dve_can_scale_up, state.dve_can_rotate, state.dve_supported_transition_styles,
    );
    info!(
        "Fairlight EQ frequencies: {:?}",
        state.fairlight_audio_frequency_ranges
    );

    loop {
        let Ok((state, update)) = atem.state_update_events().recv().await else {
            panic!("oops");
        };

        if update.contains(StateUpdate::PROGRAM_SOURCE) {
            info!("Program sources: {:?}", state.get_program_sources());
        }

        if update.contains(StateUpdate::PREVIEW_SOURCE) {
            info!("Preview sources: {:?}", state.get_preview_sources());
        }

        if update.contains(StateUpdate::AUX_SOURCE) {
            info!("AUX sources: {:?}", state.get_aux_sources());
        }

        if update.contains(StateUpdate::FADE_TO_BLACK_RATE) {
            info!("Fade to black rates: {:?}", state.get_fade_to_black_rates());
        }

        if update.contains(StateUpdate::FADE_TO_BLACK_STATUS) {
            info!(
                "Fade to black status: {:?}",
                state.get_fade_to_black_statuses()
            );
        }

        if update.contains(StateUpdate::FAIRLIGHT_TALLY) {
            info!("Fairlight tally: {:?}", state.fairlight_audio_mixer_tally);
        }

        if update.contains(StateUpdate::FAIRLIGHT_INPUT_SOURCE_PROPS) {
            info!(
                "Fairlight input source properties: {:?}",
                state.fairlight_audio_mixer_input_props
            );
        }
    }
}

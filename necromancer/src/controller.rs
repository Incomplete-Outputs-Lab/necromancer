use crate::{
    error::Error,
    protocol::{
        atom::{
            Atom, Auto, ChangeAuxSource, ChangeDskCut, ChangeDskFill, ChangeDskLive, ChangeDskRate,
            ChangeDskTie, Cut, CutToBlack, DoDskAuto, FadeToBlackAuto, FileTransferChunkParams,
            FileType, FinishFileDownload, MediaPlayerSourceID, MediaPoolLock, Payload,
            SetColourGeneratorParams, SetMediaPlayerSource, SetPreviewInput, SetProgramInput,
            SetupFileDownload, SetupFileUpload, TimecodeRequest, TransferChunk, CAPTURE_STILL,
            CLEAR_MEDIA_POOL, CLEAR_STARTUP_SETTINGS, RESTORE_STARTUP_SETTINGS,
            RTMP_DURATION_REQUEST, SAVE_STARTUP_SETTINGS,
        },
        rle::RLE_MARKER,
        structs::VideoSource,
        AtemControl, AtemPacket, AtemPacketFlags,
    },
    rle::rle_md5_size,
    state::{AtemState, StateUpdate},
    udp::AtemUdpChannel,
};
use binrw::BinWrite;
use concread::cowcell::asynch::{CowCell, CowCellReadTxn};
use futures::{pin_mut, StreamExt};
use rand::Rng;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    io::Cursor,
    net::SocketAddrV4,
    sync::{
        atomic::{AtomicBool, AtomicU16, Ordering},
        Arc, Weak,
    },
    time::{Duration, Instant},
};
use tokio::{
    select,
    sync::{
        broadcast::{self, Receiver},
        mpsc::{self, Sender},
        oneshot, Notify, Semaphore,
    },
    task::JoinHandle,
    time::interval,
};
use tokio_stream::wrappers::IntervalStream;

/// Internal type for asynchronous message passing.
enum AsyncCommand {
    /// Send [Atom(s)][Atom] to the switcher, and opptionally wait for a response.
    Commands {
        /// The [Atom(s)][Atom] to send.
        cmds: Vec<Atom>,

        /// If set, notify the caller when the atom(s) were acknowledged by the receiver, or an
        /// error was returned.
        responder: Option<oneshot::Sender<Result<(), Error>>>,
    },

    /// File download request.
    FileDownload(AsyncFileDownloadRequest),

    // File upload request.
    FileUpload(AsyncFileUploadRequest),

    /// Storage lock request
    // TODO: make locking use a state machine
    StorageLock {
        store_id: u16,
        responder: oneshot::Sender<Result<Arc<StorageLock>, Error>>,
    },
}

/// Asynchronous file download request state.
struct AsyncFileDownloadRequest {
    store_id: u16,
    index: u8,
    typ: FileType,
    tx: Arc<mpsc::Sender<Result<Vec<u8>, Error>>>,
    bytes_received: usize,
    bytes_since_last_ack: usize,
    /// Storage lock.
    ///
    /// This needs to be kept alive while the download is still in progress.
    storage_lock: Arc<StorageLock>,
}

/// Asynchronous file upload request state.
struct AsyncFileUploadRequest {
    store_id: u16,
    index: u8,
    buffer: VecDeque<u64>,
    size: u32,
    typ: FileType,
    is_rle: bool,
    name: String,
    description: String,
    md5: [u8; 16],
    chunk_size: u16,
    chunks_remaining: u16,
    responder: Option<oneshot::Sender<Result<(), Error>>>,
    semaphore: Arc<Semaphore>,
    /// Storage lock.
    ///
    /// This needs to be kept alive while the upload is in progress.
    storage_lock: Arc<StorageLock>,
}

#[allow(rustdoc::private_intra_doc_links)]
/// [AtemController] establishes a session with an ATEM switcher, and keeps
/// state.
///
/// ## General design
///
/// Each session is coordinated through a couple of tasks:
///
/// * `recv_task`: [AtemReceiver] handles incoming packets from the device,
///   accepting commands to send to the device, and handling
///   retransmits.
///
/// * `state_task`: ([AtemState]) subscribes to incoming events, and records
///   them in its own state object. [AtemController] proxies requests for state
///   to it.
///
/// ## Other notes
///
/// While this attempts retries, it seems like the controller drops *all*
/// sessions on losing Ethernet link.
pub struct AtemController {
    cmd_tx: Sender<AsyncCommand>,

    /// State associated with the connection.
    state: Arc<CowCell<AtemState>>,
    state_rx: Receiver<(CowCellReadTxn<AtemState>, StateUpdate)>,
}

impl AtemController {
    /// Connects to an ATEM controller over UDP.
    ///
    /// ## Args
    ///
    /// * `addr`: The UDP socket address to connect to
    /// * `reconnect`: If `true`, reconnect after failures.
    pub async fn connect_udp(addr: SocketAddrV4, reconnect: bool) -> Result<Self, Error> {
        info!("Initialising connection to switcher...");
        let (mut receiver, cmd_tx) = AtemReceiver::new(addr, reconnect);
        let initialised_rx = receiver.initialise().await?;
        let state = receiver.state.clone();
        let state_rx = receiver.state_rx.resubscribe();

        debug!("Spawning receiver task...");
        let recv_task = tokio::task::spawn(async move { receiver.run().await });

        info!("Waiting for initialisation completion event...");
        if initialised_rx.await.is_ok() {
            info!("connect udp done");

            let c = Self {
                cmd_tx,
                state,
                state_rx,
            };
            return Ok(c);
        }

        // Initialisation has failed - state_task has dropped initialised_tx
        // without using it.
        match recv_task.await.map_err(|e| {
            error!("joinError: {e:?}");
            Error::Internal
        })? {
            Err(e) => {
                error!("recv_task reported error: {e:?}");
                Err(e)
            }
            Ok(_) => Err(Error::Internal),
        }
    }

    /// Sends [Atom]s to the controller, and waits for a response.
    async fn send(&self, cmds: Vec<Atom>) -> Result<(), Error> {
        // https://tokio.rs/tokio/tutorial/channels#receive-responses
        let (responder, resp_rx) = oneshot::channel();
        self.send_ex(AsyncCommand::Commands {
            cmds,
            responder: Some(responder),
        })
        .await?;
        resp_rx.await.map_err(|_| Error::Timeout)?
    }

    /// Sends [Atom]s to the controller with extended options, and waits
    /// for a response.
    async fn send_ex(&self, async_cmd: AsyncCommand) -> Result<(), Error> {
        self.cmd_tx
            .send(async_cmd)
            .await
            .map_err(|_| Error::ChannelUnavailable)
    }

    /// Start an image frame download.
    ///
    /// **WARNING:** this is unreliable when high logging levels are
    /// enabled, and may return corrupted data.
    ///
    /// ## Errors
    ///
    /// * [`Error::ParameterOutOfRange`] when `index` is not a valid frame ID
    /// * [`Error::NotFound`] when `index` is a valid slot, but does not contain
    ///   any data
    pub async fn start_file_download(
        &self,
        store_id: u16,
        index: u8,
    ) -> Result<mpsc::Receiver<Result<Vec<u8>, Error>>, Error> {
        warn!("File transfers are not reliable - DATA MAY BE CORRUPTED, especially when 'trace' logging is enabled!");
        if store_id != 0 {
            error!("unsupported store ID: {store_id:#04x}");
            return Err(Error::UnknownParameter);
        }

        {
            let state = self.get_state().await;
            if index >= state.media_player_capabilities.still_count {
                error!("unknown frame ID: {index:#04x}");
                return Err(Error::ParameterOutOfRange);
            }

            let Some(frame_info) = state.media_player_frame_descriptions.get(&index) else {
                error!("unknown frame ID: {index:#04x}");
                return Err(Error::ParameterOutOfRange);
            };

            if !frame_info.is_valid {
                error!("frame ID {index:#04x} does not contain valid data");
                return Err(Error::NotFound);
            }

            // TODO: get MD5 and check it
        }

        let (responder, resp_rx) = oneshot::channel();
        self.send_ex(AsyncCommand::StorageLock {
            store_id,
            responder,
        })
        .await?;
        debug!("waiting for storage lock availability");
        let storage_lock = resp_rx.await.map_err(|_| Error::ChannelUnavailable)??;
        storage_lock.await_availability().await?;
        debug!("have storage lock");

        let (tx, rx) = mpsc::channel(128);
        let tx = Arc::new(tx);
        let req = AsyncFileDownloadRequest {
            store_id,
            index,
            tx,
            typ: FileType::StillFrame,
            bytes_received: 0,
            bytes_since_last_ack: 0,
            storage_lock,
        };
        self.send_ex(AsyncCommand::FileDownload(req)).await?;

        Ok(rx)
    }

    /// Upload [an RLE-compressed image][crate::protocol::rle] to the switcher.
    pub async fn upload_still_image(
        &self,
        index: u8,
        name: String,
        description: String,
        buffer: VecDeque<u64>,
    ) -> Result<(), Error> {
        warn!("File transfers are unreliable when 'trace' logging is enabled!");

        let frame_size = {
            let state = self.get_state().await;
            if index >= state.media_player_capabilities.still_count {
                error!("unknown frame ID: {index:#04x}");
                return Err(Error::ParameterOutOfRange);
            }
            debug!(
                "current video mode {} is {}x{}",
                state.video_mode,
                state.video_mode.width(),
                state.video_mode.lines()
            );
            state.video_mode.pixels() * 4
        };

        let (md5, size) = rle_md5_size(buffer.iter().copied());
        let size = size.try_into().map_err(|_| Error::InvalidLength)?;
        if size != frame_size {
            error!("incorrect frame size: expected {frame_size}, got {size}");
            return Err(Error::InvalidLength);
        }

        let (responder, resp_rx) = oneshot::channel();
        self.send_ex(AsyncCommand::StorageLock {
            store_id: 0,
            responder,
        })
        .await?;
        debug!("waiting for storage lock availability");
        let storage_lock = resp_rx.await.map_err(|_| Error::ChannelUnavailable)??;
        storage_lock.await_availability().await?;
        debug!("have storage lock");

        let (responder, resp_rx) = oneshot::channel();
        let req = AsyncFileUploadRequest {
            store_id: 0,
            index,
            buffer,
            typ: FileType::StillFrame,
            is_rle: true,
            size,
            name,
            description,
            md5,
            chunk_size: 0,
            chunks_remaining: 0,
            responder: Some(responder),
            semaphore: Arc::new(Semaphore::new(1)),
            storage_lock,
        };
        self.send_ex(AsyncCommand::FileUpload(req)).await?;
        resp_rx.await.map_err(|_| Error::Timeout)?
    }

    /// Sets the current program input for a given media encoder.
    pub async fn set_program_input(&self, me: u8, video_source: VideoSource) -> Result<(), Error> {
        let cmd = Atom::new(SetProgramInput { me, video_source });
        self.send(vec![cmd]).await
    }

    /// Sets the current program input for a given media encoder.
    pub async fn set_preview_input(&self, me: u8, video_source: VideoSource) -> Result<(), Error> {
        let cmd = Atom::new(SetPreviewInput { me, video_source });
        self.send(vec![cmd]).await
    }

    /// Swaps the current preview and program inputs for a given media encoder
    /// immediately with no transition.
    pub async fn cut(&self, me: u8) -> Result<(), Error> {
        let cmd = Atom::new(Cut { me });
        self.send(vec![cmd]).await
    }

    /// Swaps the current preview and program inputs for a given media encoder
    /// with the currently-selected transition.
    pub async fn auto(&self, me: u8) -> Result<(), Error> {
        let cmd = Atom::new(Auto { me });
        self.send(vec![cmd]).await
    }

    /// Sets the current source for a given AUX bus.
    pub async fn set_aux_source(&self, aux_bus: u8, video_source: VideoSource) -> Result<(), Error> {
        let state = self.get_state().await;
        if aux_bus >= state.topology.auxs {
            error!(
                "aux bus #{aux_bus} does not exist, switcher has {} aux bus(es)",
                state.topology.auxs
            );
            return Err(Error::ParameterOutOfRange);
        }
        drop(state);

        let cmd = Atom::new(ChangeAuxSource { aux_bus, video_source });
        self.send(vec![cmd]).await
    }

    pub async fn cut_black(&self, me: u8, black: bool) -> Result<(), Error> {
        let cmd = Atom::new(CutToBlack { me, black });
        self.send(vec![cmd]).await
    }

    pub async fn toggle_auto_black(&self, me: u8) -> Result<(), Error> {
        let cmd = Atom::new(FadeToBlackAuto { me });
        self.send(vec![cmd]).await
    }

    /// Triggers an auto transition on a downstream keyer.
    pub async fn dsk_auto(&self, key: u8) -> Result<(), Error> {
        let cmd = Atom::new(DoDskAuto { key });
        self.send(vec![cmd]).await
    }

    /// Sets the on-air state for a downstream keyer.
    pub async fn set_dsk_on_air(&self, key: u8, on_air: bool) -> Result<(), Error> {
        let cmd = Atom::new(ChangeDskLive { key, on_air });
        self.send(vec![cmd]).await
    }

    /// Sets the tie state for a downstream keyer.
    pub async fn set_dsk_tie(&self, key: u8, tie: bool) -> Result<(), Error> {
        let cmd = Atom::new(ChangeDskTie { key, tie });
        self.send(vec![cmd]).await
    }

    /// Sets the cut source for a downstream keyer.
    pub async fn set_dsk_cut_source(
        &self,
        key: u8,
        source: VideoSource,
    ) -> Result<(), Error> {
        let cmd = Atom::new(ChangeDskCut {
            key,
            cut_source: source,
        });
        self.send(vec![cmd]).await
    }

    /// Sets the fill source for a downstream keyer.
    pub async fn set_dsk_fill_source(
        &self,
        key: u8,
        source: VideoSource,
    ) -> Result<(), Error> {
        let cmd = Atom::new(ChangeDskFill {
            key,
            fill_source: source,
        });
        self.send(vec![cmd]).await
    }

    /// Sets the auto transition rate for a downstream keyer.
    pub async fn set_dsk_rate(&self, key: u8, rate: u8) -> Result<(), Error> {
        let cmd = Atom::new(ChangeDskRate { key, rate });
        self.send(vec![cmd]).await
    }

    /// Captures the primary program output as a still image.
    pub async fn capture(&self) -> Result<(), Error> {
        let state = self.get_state().await;
        if !state.media_player_capabilities.supports_still_capture {
            error!("switcher does not support still image capture");
            return Err(Error::FeatureUnavailable);
        }
        let cmd = Atom::new(CAPTURE_STILL);
        self.send(vec![cmd]).await
    }

    /// Change a media player's source.
    pub async fn set_media_player_source(
        &self,
        media_player: u8,
        source: MediaPlayerSourceID,
    ) -> Result<(), Error> {
        let state = self.get_state().await;
        if media_player >= state.topology.media_players {
            error!(
                "media player #{media_player} does not exist, switcher has {} media player(s)",
                state.topology.media_players
            );
            return Err(Error::ParameterOutOfRange);
        }

        match source {
            MediaPlayerSourceID::Still(index) => {
                if state.media_player_capabilities.still_count == 0 {
                    error!("media player does not support still images");
                    return Err(Error::FeatureUnavailable);
                }

                if index >= state.media_player_capabilities.still_count {
                    error!(
                        "still #{index} does not exist, switcher supports {} still(s)",
                        state.media_player_capabilities.still_count
                    );
                    return Err(Error::ParameterOutOfRange);
                }

                if !state
                    .media_player_frame_descriptions
                    .get(&index)
                    .is_some_and(|mpfd| mpfd.is_valid)
                {
                    error!("still #{index} does not contain a valid frame");
                    return Err(Error::NotFound);
                }
            }

            MediaPlayerSourceID::VideoClip(index) => {
                if state.media_player_capabilities.clip_count == 0 {
                    error!("media player does not support video clips");
                    return Err(Error::FeatureUnavailable);
                }

                if index >= state.media_player_capabilities.clip_count {
                    error!(
                        "clip #{index} does not exist, switcher supports {} clip(s)",
                        state.media_player_capabilities.clip_count
                    );
                    return Err(Error::ParameterOutOfRange);
                }
            }
        }
        drop(state);

        let cmd = Atom::new(SetMediaPlayerSource {
            enable: true,
            id: media_player,
            source,
        });
        self.send(vec![cmd]).await
    }

    /// Delete all items in the media pool.
    pub async fn clear_media_pool(&self) -> Result<(), Error> {
        let cmd = Atom::new(CLEAR_MEDIA_POOL);
        self.send(vec![cmd]).await
    }

    pub async fn rtmp_stream_duration(&self) -> Result<(), Error> {
        let cmd = Atom::new(RTMP_DURATION_REQUEST);
        self.send(vec![cmd]).await
    }

    /// Saves the current settings to the start-up configuration.
    pub async fn save_startup_settings(&self) -> Result<(), Error> {
        let cmd = Atom::new(SAVE_STARTUP_SETTINGS);
        self.send(vec![cmd]).await
    }

    /// Clears the start-up configuration.
    pub async fn clear_startup_settings(&self) -> Result<(), Error> {
        let cmd = Atom::new(CLEAR_STARTUP_SETTINGS);
        self.send(vec![cmd]).await
    }

    /// Restores settings from the start-up configuration
    ///
    /// **Warning:** this method is never used by the SDK
    pub async fn restore_startup_settings(&self) -> Result<(), Error> {
        let cmd = Atom::new(RESTORE_STARTUP_SETTINGS);
        self.send(vec![cmd]).await
    }

    /// Sets colour generator parameters.
    pub async fn set_colour_generator_params(
        &self,
        params: SetColourGeneratorParams,
    ) -> Result<(), Error> {
        let cmd = Atom::new(params);
        self.send(vec![cmd]).await
    }

    pub async fn get_state(&self) -> impl std::ops::Deref<Target = AtemState> {
        self.state.read().await
    }

    pub fn state_update_events(&self) -> Receiver<(CowCellReadTxn<AtemState>, StateUpdate)> {
        self.state_rx.resubscribe()
    }
}

/// A packet to retry sending if there is no response from the switcher.
struct PacketWaitingForResponse {
    /// The packet which will be re-sent after a timeout.
    pkt: AtemPacket,
    /// Channel which was awaited for a response.
    responder: Option<oneshot::Sender<Result<(), Error>>>,
    /// Number of remaining retries for the packet.
    retry_limit: u8,
    /// Time when a retry was last attempted. The packet will be retried after
    /// [AtemReceiver::RETRANSMIT_TIME] has passed.
    last_attempt: Instant,
}

/// Coordinates the connection with the device.
///
/// This runs its own event loop ([`AtemReceiver::run()`]).
struct AtemReceiver {
    addr: SocketAddrV4,
    channel: AtemUdpChannel,
    cmd_rx: mpsc::Receiver<AsyncCommand>,
    cmd_tx_weak: mpsc::WeakSender<AsyncCommand>,
    /// Packets which have been received from the switcher. Events will be
    /// sorted by [`AtemPacket::sender_packet_id`].
    tx: Option<mpsc::Sender<AtemPacket>>,
    /// The current session ID for the connection.
    session_id: u16,
    /// Packets which have not yet been forwarded to [Self::tx]. This must
    /// remain sorted by [`AtemPacket::sender_packet_id`].
    rx_queue: VecDeque<AtemPacket>,
    /// The last time we tried to send to [Self::tx]
    last_rx_time: Instant,
    /// The next packet ID we expect to forward to [Self::tx]
    next_pkt_forward: u16,
    /// Atomic counter to track which [`AtemPacket::sender_packet_id`] should be
    /// used next.
    sender_packet_id: AtomicU16,
    /// Packets that have been sent which are waiting for a response from the
    /// switcher. This must remain sorted by [`AtemPacket::sender_packet_id`].
    ///
    /// This is limited to [`Self::MAX_ACK_QUEUE_LENGTH`].
    ack_queue: VecDeque<PacketWaitingForResponse>,
    /// [Notify] used to track when a request for the device's clock times out.
    clock_notifier: Arc<Notify>,
    /// Currently locked storage domains by any client.
    storage_locks: HashSet<u16>,
    owned_storage_locks: HashMap<u16, Weak<StorageLock>>,
    /// File downloads which are still in progress.
    downloads: HashMap<u16, AsyncFileDownloadRequest>,
    /// File uploads which are still in progress.
    uploads: HashMap<u16, AsyncFileUploadRequest>,
    upload_chunk_params_rx: mpsc::Receiver<FileTransferChunkParams>,
    upload_chunk_params_tx: mpsc::Sender<FileTransferChunkParams>,
    /// File uploads which are completed, and awaiting confirmation from the
    /// switcher ([TransferCompleted][crate::protocol::atom::TransferCompleted]).
    finished_uploads: HashMap<u16, (Option<oneshot::Sender<Result<(), Error>>>, Arc<StorageLock>)>,
    /// [Notify] used to track when we need to stop our main event loop.
    stop_main_loop: Arc<Notify>,
    /// We've already issued a disconnect command, or the switcher is
    /// disconnecting us.
    disconnected: AtomicBool,
    state: Arc<CowCell<AtemState>>,
    state_rx: broadcast::Receiver<(CowCellReadTxn<AtemState>, StateUpdate)>,
    state_tx: broadcast::Sender<(CowCellReadTxn<AtemState>, StateUpdate)>,
    state_task: Option<JoinHandle<Result<(), Error>>>,
    reconnect: bool,
    reconnection_signal: Option<oneshot::Receiver<()>>,
    initialisation_complete: bool,
}

impl AtemReceiver {
    const INIT_TIMEOUT: Duration = Duration::from_secs(1);
    const DISCONNECT_TIMEOUT: Duration = Duration::from_secs(1);
    const RECONNECT_DELAY: Duration = Duration::from_secs(1);

    /// Depth of the upload chunk params receive buffer.
    const UPLOAD_CHUNK_PARAMS_SIZE: usize = 16;

    /// Depth of the command transmit buffer.
    ///
    /// This affects how many [`AsyncCommand`]s can sit in
    /// [`cmd_rx`][Self::cmd_rx].
    ///
    /// Once a packet has been sent once, it enters
    /// [`ack_queue`][Self::ack_queue], which is affected by
    /// [`MAX_ACK_QUEUE_LENGTH`][Self::MAX_ACK_QUEUE_LENGTH].
    const COMMAND_CHANNEL_SIZE: usize = 16;

    /// During a frame upload, the number of chunks that will be sent as a
    /// "burst" before waiting for the switcher to acknowledge them all.
    ///
    /// ## Background
    ///
    /// The switcher indicates its transfer capacity with
    /// [`FileTransferChunkParams`], but this doesn't seem to be the *only*
    /// bottleneck in the switcher's file transfer pipeline.
    ///
    /// The switcher can batch-acknowledge up to about 32 packets (with the
    /// ATEM Mini), and will wait about 20ms after the last packet before
    /// sending *any* acknowledgement. We also need to manage size of
    /// [`cmd_rx`][Self::cmd_rx] and [`ack_queue`][Self::ack_queue] – we don't
    /// want to gum those up with too many file transfer chunks or require
    /// retransmits.
    ///
    /// ATEM's SDK/tools seem to just take the [`FileTransferChunkParams`] at
    /// face value, and DoS the switcher. This also means it ends up
    /// retransmitting nearly every chunk of a frame at least once, and ends up
    /// wasting a bunch of bandwidth *and* being slower.
    ///
    /// ## Benchmarks
    ///
    /// This benchmark is sending a mostly-uncompressed (7,204,696 bytes, 5178
    /// [chunks][chunk]) 1080p image to the ATEM Mini, using a wired gigabit
    /// ethernet network, on the same switch.
    ///
    /// Waiting for ack after... | Duration  | Packets sent
    /// ------------------------ | --------- | ------------
    /// each [chunk][]           | 86.38 sec | 5,178 (1x)
    /// every 10 [chunks][chunk] | 8.77 sec  | 5,178 (1x)
    /// every 24 [chunks][chunk] | 3.77 sec  | 5,178 (1x)
    /// vs. ATEM's SDK/tools     | 6.66 sec  | 15,672 (3.03x)
    ///
    /// "Duration" is measured from [`SetupFileUpload`] (`FTSD`) to the switcher
    /// sending [`TransferCompleted`][crate::protocol::atom::TransferCompleted] (`FTDC`).
    /// _Lower is better._
    ///
    /// "Packets sent" is the number of packets containing a [chunk][]. A number
    /// higher than 5,178 (1x) indicates that the client retransmitted some
    /// chunks because the switcher couldn't keep up. _Lower is better_.
    ///
    /// [chunk]: TransferChunk
    const UPLOAD_BURST_SIZE: usize = 24;

    /// Maximum length which [`ack_queue`][Self::ack_queue] may grow to.
    const MAX_ACK_QUEUE_LENGTH: usize = 512;
    const OVERFLOW_MARGIN: u16 = AtemPacket::MAX_PACKET_ID - (Self::MAX_ACK_QUEUE_LENGTH as u16);
    /// Number of retries to send.
    const RETRANSMIT_LIMIT: u8 = 3;
    /// Delay before next retry.
    const RETRANSMIT_TIME: Duration = Duration::from_millis(500);

    /// Create a new `recv_task`.
    ///
    /// ## Returns
    ///
    /// * `channel`: UDP connection to work with
    /// * `tx`: [broadcast::Sender] where incoming packets from the device go to
    fn new(addr: SocketAddrV4, reconnect: bool) -> (Self, mpsc::Sender<AsyncCommand>) {
        let (cmd_tx, cmd_rx) = mpsc::channel(Self::COMMAND_CHANNEL_SIZE);
        let cmd_tx_weak = cmd_tx.downgrade();
        let (state_tx, state_rx) = broadcast::channel(16);
        let (upload_chunk_params_tx, upload_chunk_params_rx) =
            mpsc::channel(Self::UPLOAD_CHUNK_PARAMS_SIZE);
        (
            Self {
                addr,
                channel: AtemUdpChannel::new(),
                cmd_rx,
                cmd_tx_weak,
                tx: None,
                session_id: 0,
                rx_queue: VecDeque::new(),
                last_rx_time: Instant::now(),
                next_pkt_forward: 1,
                sender_packet_id: AtomicU16::new(1),
                ack_queue: VecDeque::new(),
                clock_notifier: Arc::new(Notify::new()),
                storage_locks: HashSet::new(),
                owned_storage_locks: HashMap::new(),
                downloads: HashMap::new(),
                uploads: HashMap::new(),
                finished_uploads: HashMap::new(),
                upload_chunk_params_rx,
                upload_chunk_params_tx,
                stop_main_loop: Arc::new(Notify::new()),
                disconnected: AtomicBool::new(false),
                state: Arc::new(CowCell::new(AtemState::default())),
                state_tx,
                state_rx,
                state_task: None,
                reconnect,
                reconnection_signal: None,
                initialisation_complete: false,
            },
            cmd_tx,
        )
    }

    async fn spawn_state_task(
        &mut self,
        mut rx: mpsc::Receiver<AtemPacket>,
    ) -> Result<oneshot::Receiver<()>, Error> {
        let (initialised_tx, initialised_rx) = oneshot::channel();
        debug!("Spawning state_task");
        let state_state = self.state.clone();
        let state_tx = self.state_tx.clone();

        self.state_task = Some(tokio::task::spawn(async move {
            let mut initialised_tx = Some(initialised_tx);
            while let Some(pkt) = rx.recv().await {
                if let Some(cmds) = pkt.atoms() {
                    let mut w = state_state.write().await;
                    let updated_fields = w.update_state(cmds)?;
                    if !updated_fields.is_empty() {
                        w.commit().await;

                        // It doesn't matter whether this actually succeeds
                        let _ = state_tx.send((state_state.read().await, updated_fields));

                        // Signal initialisation completion after updating our
                        // state.
                        if updated_fields.intersects(StateUpdate::INITIALISATION_COMPLETE) {
                            if let Some(t) = initialised_tx.take() {
                                if t.send(()).is_err() {
                                    error!("Could not signal initialisation completion");
                                    break;
                                }
                            }
                        }
                    }
                }
            }

            debug!("state_task done");
            Ok::<(), Error>(())
        }));

        Ok(initialised_rx)
    }

    /// Initialises the connection to the switcher, and requests the current state.
    async fn initialise(&mut self) -> Result<oneshot::Receiver<()>, Error> {
        // Explicitly clear internal states, in case some other task has a copy
        // of this.
        self.sender_packet_id.store(1, Ordering::SeqCst);
        self.session_id = 0;
        self.rx_queue.clear();
        self.next_pkt_forward = 1;
        self.last_rx_time = Instant::now();
        self.disconnected.store(false, Ordering::SeqCst);
        self.storage_locks.clear();
        self.owned_storage_locks.clear();
        self.downloads.clear();
        self.uploads.clear();
        self.finished_uploads.clear();
        self.initialisation_complete = false;
        self.clock_notifier = Arc::new(Notify::new());
        self.stop_main_loop = Arc::new(Notify::new());
        (self.upload_chunk_params_tx, self.upload_chunk_params_rx) =
            mpsc::channel(Self::UPLOAD_CHUNK_PARAMS_SIZE);
        self.ack_queue.clear();
        {
            let mut s = self.state.write().await;
            AtemState::default().clone_into(&mut s);
            s.commit().await;
        }

        self.channel.connect(self.addr).await?;
        let (tx, rx) = mpsc::channel::<AtemPacket>(16);
        self.tx = Some(tx);
        let initialised_rx = self.spawn_state_task(rx).await?;

        // Our initial session ID should be random, and not set the highest bit.
        let initial_session_id = rand::rng().random_range(1..=0x7fff);

        self.channel
            .send(&AtemPacket::new_control(
                AtemPacketFlags::new().with_control(true),
                // AtemPacketFlags::CONTROL,
                initial_session_id,
                0,
                0xb1, /* TODO */
                0,
                AtemControl::Connect,
            ))
            .await?;

        debug!("Waiting for init packet for session {initial_session_id:#x}...");
        let (switcher_packet_id, session_id) = tokio::time::timeout(Self::INIT_TIMEOUT, async {
            loop {
                let resp = self.channel.recv().await?;
                if resp.session_id != initial_session_id {
                    // wrong session ID
                    continue;
                }

                let Some(control) = resp.control() else {
                    // wrong payload type
                    continue;
                };

                match control {
                    AtemControl::ConnectAck { session_id } => {
                        return Ok((resp.sender_packet_id, session_id | 0x8000));
                    }

                    AtemControl::ConnectNack => {
                        error!("switcher rejected connection attempt for session {initial_session_id:#x}");
                        return Err(Error::MixerOverloaded);
                    }

                    _ => {
                        error!("Unexpected control code {control:?} on session {initial_session_id:#x}");
                        return Err(Error::UnexpectedState);
                    }
                }
            }
        })
        .await
        .map_err(|_| {
            error!("timeout waiting for INIT response from switcher on session {initial_session_id:#x}");
            Error::Timeout
        })??;

        // The proper session ID to use for later packets
        self.session_id = session_id;

        // Acknowledge the INIT response using `initial_session_id`, which
        // triggers a request for current state. The response will be on
        // `session_id`.
        debug!("Established session {session_id:#x} from {initial_session_id:#x}, requesting current switcher state...");
        self.channel
            .send(&AtemPacket::new(
                AtemPacketFlags::new().with_response(true),
                initial_session_id,
                switcher_packet_id,
                0xd4, /* TODO */
                0,
            ))
            .await?;

        debug!("Yielding further processing to main loop...");
        Ok(initialised_rx)
    }

    /// Disconnects from the switcher.
    fn disconnect(&mut self) -> Result<(), Error> {
        info!("AtemReceiver disconnecting...");
        // Need to stop the main loop
        self.stop_main_loop.notify_waiters();
        if self.disconnected.swap(true, Ordering::SeqCst) {
            debug!("switcher already disconnected us!");
            return Ok(());
        }

        let pkt = AtemPacket::new_control(
            AtemPacketFlags::new().with_control(true),
            self.session_id,
            0,
            0,
            0,
            AtemControl::Disconnect,
        );
        let mut out = Cursor::new(Vec::new());
        pkt.write(&mut out)?;

        // Take ownership of the connection, because this isn't run in an async
        // context, and the tokio runtime may be shutting down.
        let sock = self.channel.take_std_socket()?;
        sock.set_nonblocking(false)?;
        sock.set_read_timeout(Some(Self::DISCONNECT_TIMEOUT))?;
        sock.set_write_timeout(Some(Self::DISCONNECT_TIMEOUT))?;

        sock.send(&out.into_inner())?;

        debug!("sent disconnect");

        // Don't worry about waiting for a response.
        Ok(())
    }

    /// Starts the main event loop.  There are four sources of events:
    ///
    /// * `cmd_rx.recv`: [handles queued commands to send to the switcher][Self::handle_queued_command]
    ///
    ///   Commands are logged in a buffer if retransmission is required.
    ///
    /// * `channel.recv`: [collects new packets from the switcher to process][Self::handle_incoming_packet],
    ///   and pushes them into the receiver queue (in order).
    ///
    /// * `retransmit_wakeup`: periodically runs a few tasks:
    ///
    ///   * [request the current clock state][Self::request_clock].
    ///
    ///     This acts as a keep-alive / health check, if it fails,
    ///     `clock_notifier` is triggered.
    ///
    ///   * [limit the receiver queue][Self::limit_rx_queue]; if it has grown
    ///     [too large][Self::MAX_RX_QUEUE_LENGTH], or it has been
    ///     [too long][Self::MAX_RX_QUEUE_TIME] since it was
    ///     [last forwarded][Self::forward_rx_queue], it flushes the entire
    ///     queue immediately.
    ///
    ///   * [retransmit any unacknowledged commands][Self::do_retransmits], and
    ///     notify receivers of any unackowledged commands.
    ///
    /// * `clock_notifier`: aborts the event loop if there was no
    ///   acknowledgement of a clock state request.
    async fn main_loop(&mut self) -> Result<(), Error> {
        let retransmit_wakeup = IntervalStream::new(interval(Self::RETRANSMIT_TIME));
        pin_mut!(retransmit_wakeup);

        trace!(
            "starting loop, reconnection_signal.is_some() = {}",
            self.reconnection_signal.is_some()
        );
        let reconnection_signal = self.reconnection_signal.take();
        let reconnection_notify = Arc::new(Notify::new());
        let reconnection_notifier = reconnection_notify.clone();
        tokio::task::spawn(async move {
            if let Some(reconnection_signal) = reconnection_signal {
                let _ = reconnection_signal.await;
                reconnection_notifier.notify_waiters();
            }
        });

        loop {
            select! {
                () = self.stop_main_loop.notified() => {
                    // This might not execute!
                    info!("stopping main_loop");
                    return Ok(());
                }

                () = self.clock_notifier.notified() => {
                    error!("clock request timed out - loss of connectivity?");
                    return Err(Error::Timeout);
                }

                // Look for something to send
                Some(async_cmd) = self.cmd_rx.recv() => {
                    self.handle_queued_command(async_cmd).await?;
                }

                // Get packets from the switcher
                Ok(resp) = self.channel.recv() => {
                    self.handle_incoming_packet(resp).await?;
                }

                Some(_) = retransmit_wakeup.next() => {
                    if self.initialisation_complete {
                        self.request_clock().await?;
                    }
                    self.limit_rx_queue().await?;
                    self.do_retransmits().await?;
                }

                Some(params) = self.upload_chunk_params_rx.recv() => {
                    self.handle_file_transfer_chunk_params(params).await?;
                }

                () = reconnection_notify.notified() => {
                    debug!("reconnection_signal fired");
                }
            }
        }
    }

    async fn run(mut self) -> Result<(), Error> {
        loop {
            debug!("starting main_loop");
            let r = self.main_loop().await;

            // Abort the state_task, and check if it returned an error
            if let Some(state_task) = self.state_task.take() {
                state_task.abort();
                match state_task.await {
                    Err(join_error) => {
                        if join_error.is_panic() {
                            error!("state_task paniced: {}", join_error.to_string());
                            return Err(Error::Internal);
                        }

                        // Cancellation is expected here...
                    }

                    Ok(Err(e)) => {
                        error!("state_task reported error: {e:?}");
                        return Err(e);
                    }

                    Ok(Ok(())) => (),
                }
            }

            if !self.reconnect {
                // Don't automatically reconnect.
                return r;
            }

            loop {
                info!("disconnected; waiting for reconnection");
                tokio::time::sleep(Self::RECONNECT_DELAY).await;

                info!("reconnecting...");
                match self.initialise().await {
                    Err(e) => {
                        error!("error initialising reconnection: {e:?}");
                        continue;
                    }

                    Ok(o) => {
                        // The initialisation can't happen until the main_loop
                        // is running again; so we can't stop here and wait.
                        self.reconnection_signal = Some(o);
                    }
                }

                info!("reconnected, waiting for new initialisation");
                break;
            }
        }
    }

    /// Sends some commands to the switcher, returning the `sender_packet_id`
    /// used for that packet.
    async fn handle_queued_command(&mut self, async_cmd: AsyncCommand) -> Result<(), Error> {
        let (cmds, responder) = match async_cmd {
            AsyncCommand::Commands { cmds, responder } => (cmds, responder),
            AsyncCommand::FileDownload(req) => {
                let id = rand::random();
                let cmd = Atom::new(SetupFileUpload {
                    id,
                    store_id: req.store_id,
                    index: req.index.into(),
                    typ: req.typ,
                });

                // Now also register a handler
                self.downloads.insert(id, req);

                // TODO: handle leakage on errors
                (vec![cmd], None)
            }
            AsyncCommand::FileUpload(req) => {
                let id = rand::random();
                let cmd = Atom::new(SetupFileDownload {
                    id,
                    store_id: req.store_id,
                    index: req.index.into(),
                    size: req.size,
                    typ: req.typ,
                    is_rle: req.is_rle,
                });
                self.uploads.insert(id, req);
                // TODO: handle leakage on errors
                (vec![cmd], None)
            }
            AsyncCommand::StorageLock {
                store_id,
                responder,
            } => {
                if store_id >= 64 {
                    let _ = responder.send(Err(Error::ParameterOutOfRange));
                    return Ok(());
                }

                // TODO: replace with try_insert: https://github.com/rust-lang/rust/issues/82766
                let owned_storage_lock = match self.owned_storage_locks.get_mut(&store_id) {
                    Some(l) => l,
                    None => {
                        let w = Weak::new();
                        assert!(self.owned_storage_locks.insert(store_id, w).is_none());
                        self.owned_storage_locks
                            .get_mut(&store_id)
                            .expect("expected storage_lock to exist now")
                    }
                };

                match owned_storage_lock.upgrade() {
                    None => {
                        // The weak pointer was dropped, need to make another storage lock.

                        // Check if the lock is owned by another client
                        if self.storage_locks.contains(&store_id) {
                            warn!("storage {} is locked by another client", store_id);
                            // let _ = responder.send(Err(Error::StorageLocked));
                            // return Ok(());
                        }

                        let Some(cmd_tx) = self.cmd_tx_weak.upgrade() else {
                            error!("could not upgrade cmd_tx");
                            let _ = responder.send(Err(Error::Internal));
                            return Err(Error::Internal);
                        };

                        // Create a lock entity to fill later
                        let storage_lock = Arc::new(StorageLock::new(store_id, cmd_tx));
                        *owned_storage_lock = Arc::downgrade(&storage_lock);

                        // Now try to lock it
                        let cmd = Atom::new(MediaPoolLock::lock(store_id));

                        let _ = responder.send(Ok(storage_lock));
                        (vec![cmd], None)
                    }

                    Some(storage_lock) => {
                        // We already have a storage lock, so we can just pass that
                        // back to the client.
                        let _ = responder.send(Ok(storage_lock.clone()));

                        return Ok(());
                    }
                }
            }
        };

        let r = async {
            let sender_packet_id = self.sender_packet_id.fetch_add(1, Ordering::SeqCst);
            trace!(">>> 0x{sender_packet_id:04X}: {:?}", cmds);
            let pkt = AtemPacket::new_atoms(
                AtemPacketFlags::new().with_ack(true),
                self.session_id,
                0,
                0,
                sender_packet_id,
                cmds,
            );

            self.channel.send(&pkt).await?;

            Ok(pkt)
        }
        .await;

        let mut pkt = match r {
            Err(e) => {
                if let Some(responder) = responder {
                    if let Err(e) = responder.send(Err(e)) {
                        error!("error sending error response to responder: {e:?}");
                    }
                }

                return Err(Error::Internal);
            }

            Ok(p) => p,
        };

        // Add an entry to the ack_queue for this packet. Mark it as a
        // retransmission for future sends.
        pkt.flags.set_retransmission(true);
        let waiting = PacketWaitingForResponse {
            pkt,
            responder,
            retry_limit: Self::RETRANSMIT_LIMIT,
            last_attempt: Instant::now(),
        };

        if self.ack_queue.len() >= Self::MAX_ACK_QUEUE_LENGTH {
            warn!(
                "ack_queue is long ({} entries), has the switcher stalled?",
                self.ack_queue.len()
            );
            self.ack_queue.clear();
        }

        let sender_packet_id = waiting.pkt.sender_packet_id;
        let idx = self
            .ack_queue
            .partition_point(|p| p.pkt.sender_packet_id < waiting.pkt.sender_packet_id);
        self.ack_queue.insert(idx, waiting);
        trace!(
            ack_queue_len = self.ack_queue.len(),
            "sent command: 0x{sender_packet_id:04X}",
        );

        Ok(())
    }

    const RX_QUEUE_OVERFLOW_MARGIN: u16 =
        AtemPacket::MAX_PACKET_ID - (Self::MAX_RX_QUEUE_LENGTH as u16);
    const MAX_RX_QUEUE_LENGTH: usize = 64;
    const MAX_RX_QUEUE_TIME: Duration = Duration::from_secs(2);

    /// Handles an incoming packet from the device.
    ///
    /// If the packet is an acknowledgement of a previously-sent command, this
    /// will [notify any waiting tasks][Self::handle_ack]. This handles when
    /// the switcher batch-acknowledges packets.
    ///
    /// If the packet has a `sender_packet_id`, then it contains an event. This
    /// is pushed (in order) into the receiver queue, and when we have the next
    /// expected `sender_packet_id`, any outstanding packets are
    /// [forwarded to listeners][Self::forward_rx_queue].
    ///
    /// This also [checks the receiver queue][Self::limit_rx_queue].
    async fn handle_incoming_packet(&mut self, resp: AtemPacket) -> Result<(), Error> {
        // Check that the incoming packet is for our session ID
        assert_ne!(
            0,
            self.session_id & 0x8000,
            "handling an incoming packet before establishing a session ID!"
        );
        if self.session_id != resp.session_id {
            warn!(
                "unexpected session ID: {:#X} != {:#X}",
                resp.session_id, self.session_id,
            );
            return Ok(());
        }

        self.limit_rx_queue().await?;
        trace!("<<< 0x{:04X}: {resp:X?}", resp.sender_packet_id);

        if let Some(ctrl) = resp.control() {
            match ctrl {
                AtemControl::Disconnect => {
                    error!("switcher disconnected us!");
                    self.disconnected.store(true, Ordering::SeqCst);

                    // Acknowledge the disconnect immediately, without queueing.
                    let pkt = AtemPacket::new_control(
                        AtemPacketFlags::new().with_control(true),
                        self.session_id,
                        0,
                        0,
                        0,
                        AtemControl::DisconnectAck,
                    );
                    self.channel.send(&pkt).await?;
                    return Err(Error::Disconnected);
                }

                ctrl => {
                    error!("unexpected control packet: {ctrl:?}");
                    return Err(Error::UnexpectedState);
                }
            }
        }

        if let Some(atoms) = resp.atoms() {
            if atoms
                .iter()
                .any(|c| matches!(c.payload, Payload::InitialisationComplete(_)))
            {
                self.initialisation_complete = true;
            }
        }

        // Check if this is a response to something we sent earlier.
        if resp.acked_packet_id != 0 && resp.flags.response() {
            self.handle_ack(resp.acked_packet_id);
        }

        if resp.sender_packet_id == 0 && !resp.has_atoms() {
            // Message is likely an acknowledgement of something we sent
            // earlier; but it was something already in process (eg: pressing
            // "Auto" when a transition is already in progress).
            //
            // Don't do anything with the rx_queue.
            //
            // On overflow, the switcher will send real messages with this
            // packet ID. BMDSwitcherAPI seems to batch-ack this packet when
            // sender_packet_id = 1 arrives; but this could mean we lost some
            // data.
            // debug!("got zero sender_packet_id: {resp:#?}");
            return Ok(());
        }

        if resp.sender_packet_id < self.next_pkt_forward {
            // Switcher is sending us an old packet again, but we're past that
            // point. This tends to happen a lot on WiFi.
            trace!(
                "switcher sent us old packet {:#04X}, we're up to {:#04X}",
                resp.sender_packet_id,
                self.next_pkt_forward
            );

            // forward_rx_queue should have acked this packet already, but maybe
            // it was lost? But shouldn't matter, as when everything catches up
            // again, we'll effectively "batch-ack" the messages.
            // if let Some(mut ack) = resp.make_ack() {
            //     trace!("re-acking packet: 0x{:04X}", ack.acked_packet_id);
            //     if ack.has_atoms() && ack.sender_packet_id == 0 {
            //         // Contains transfer acks, but no sender_packet_id has been
            //         // allocated.
            //         ack.sender_packet_id = self.sender_packet_id.fetch_add(1, Ordering::SeqCst);
            //         trace!("includes transfer acks, sending with sender_packet_id={:#04X}", ack.sender_packet_id);
            //     }
            //     self.channel.send(&ack).await?;
            // }
            return Ok(());
        }

        // Put the received packet into the queue
        let idx = self
            .rx_queue
            .partition_point(|p| p.sender_packet_id < resp.sender_packet_id);

        if self
            .rx_queue
            .get(idx)
            .is_some_and(|p| p.sender_packet_id == resp.sender_packet_id)
        {
            // Retransmission of a packet we already received
            // TODO: maybe should ack this again
            debug!(
                "repeat incoming existing packet in queue: 0x{:04X}, next_pkt_forward=0x{:04x}",
                resp.sender_packet_id, self.next_pkt_forward
            );
        } else {
            if idx > 0 {
                // We _always_ queue packets, but this means we only log
                // when we're behind (due to packet loss or out-of-order
                // delivery).
                trace!(
                    "queuing packet, next_pkt_forward=0x{:04X}",
                    self.next_pkt_forward
                );
            }
            self.rx_queue.insert(idx, resp);
        }

        // Forward any packets in the queue which are near the next expected value.
        //
        // FIXME: this should actually push _everything_ in the queue which is
        // sequentially after a next packet to forward. At the moment it waits
        // for `limit_rx_queue()`, which could end up sending some out-of-order
        // packets.
        let s = if self.next_pkt_forward >= Self::RX_QUEUE_OVERFLOW_MARGIN {
            // We're close to the overflow point, so do a lower bounds check to
            // ensure only grab what's "high"
            self.rx_queue
                .partition_point(|p| Self::RX_QUEUE_OVERFLOW_MARGIN < p.sender_packet_id)
        } else {
            0
        };

        let idx = self
            .rx_queue
            .partition_point(|p| p.sender_packet_id <= self.next_pkt_forward);
        if idx > 0 {
            self.forward_rx_queue(s..idx).await?;
        }

        Ok(())
    }

    /// Forwards the contents of the reciever queue to subscribers, and
    /// acknowledges the packet(s).
    ///
    /// Unlike the BM SDK, this acknowledges *every* packet explicitly, and
    /// doesn't batch acks.
    async fn forward_rx_queue(
        &mut self,
        range: impl std::ops::RangeBounds<usize>,
    ) -> Result<(), Error> {
        let packets: Vec<AtemPacket> = self.rx_queue.drain(range).collect();
        for mut p in packets {
            // There are two cases to handle in this function:
            // - we have a total drain of all entries (timeout or size limit)
            //   (not anymore, we'll just disconnect)
            // - we have a partial drain of "what should happen next" (normal)
            //
            // rx_queue is always sorted by ID. On overflow, a normal partial
            // drain will skip over the "low" values, and come back for them
            // later.
            //
            // TODO: On total drains then there can be a big gap and everything
            // is whacky.
            if p.sender_packet_id == 0x7fff {
                // Next packet after 0x7fff is 0x0
                self.next_pkt_forward = 0;
            } else if self.next_pkt_forward == p.sender_packet_id {
                // This can go backwards if the switcher retransmits an
                // older packet after sending a newer packet; particularly
                // an issue over WiFi.
                //
                // This forces us to process every packet in order.
                self.next_pkt_forward = p.sender_packet_id + 1;
            }

            let ack: Option<AtemPacket> = p.make_ack();

            if let Some(cmds) = p.atoms_mut() {
                // Special-case some commands.
                // TODO: migrate to extract_if when stable
                let mut i = 0;
                while i < cmds.len() {
                    match cmds[i].payload {
                        Payload::FileTransferChunkParams(_) => {
                            // Uploads
                            let ftcp = cmds.remove(i);
                            self.enqueue_file_transfer_chunk_params(ftcp).await?;
                        }

                        Payload::TransferCompleted(_) => {
                            // Used by uploads and downloads
                            let ftdc = cmds.remove(i);
                            self.handle_transfer_completed(ftdc).await?;
                        }

                        Payload::FileTransferError(_) => {
                            // Used by uploads and downloads
                            let ftde = cmds.remove(i);
                            self.handle_transfer_error(ftde).await?;
                        }

                        Payload::TransferChunk(_) => {
                            // Downloads
                            let chunk = cmds.remove(i);
                            self.handle_download_chunk(chunk).await?;
                        }

                        Payload::LockObtained(_) => {
                            let lkob = cmds.remove(i);
                            self.handle_lock_obtained(lkob).await?;
                        }

                        Payload::MediaPoolLockStatus(_) => {
                            let lkst = cmds.remove(i);
                            self.handle_lock_state_changed(lkst).await?;
                        }

                        // Not a file transfer command, ignore it.
                        _ => i += 1,
                    }
                }
            }

            let Some(tx) = self.tx.as_ref() else {
                error!("state channel missing!");
                return Err(Error::Internal);
            };

            if tx.send(p).await.is_err() {
                error!("state channel gone!");
                return Err(Error::Internal);
            }

            if let Some(mut ack) = ack {
                trace!("acking packet: 0x{:04X}", ack.acked_packet_id);
                if ack.has_atoms() && ack.sender_packet_id == 0 {
                    // Contains transfer acks, but no sender_packet_id has been
                    // allocated.
                    ack.sender_packet_id = self.sender_packet_id.fetch_add(1, Ordering::SeqCst);
                    trace!(
                        "includes transfer acks, sending with sender_packet_id={:#04X}",
                        ack.sender_packet_id
                    );
                }
                self.channel.send(&ack).await?;
            }
            self.last_rx_time = Instant::now();
        }

        Ok(())
    }

    /// Limit the `rx_queue` to a maximum age and size
    async fn limit_rx_queue(&mut self) -> Result<(), Error> {
        let rx_duration: Duration = self.last_rx_time.elapsed();
        if rx_duration >= Self::MAX_RX_QUEUE_TIME
            || self.rx_queue.len() >= Self::MAX_RX_QUEUE_LENGTH
        {
            // Too long since the last message was forwarded; dump the
            // buffer and move ahead.
            warn!(
                "packet buffer stalled for too long ({} ms) or too large ({}), disconnecting",
                rx_duration.as_millis(),
                self.rx_queue.len()
            );
            // self.forward_rx_queue(..).await?;
            return Err(Error::Timeout);
        }

        Ok(())
    }

    /// Handle acknowledgements of previously sent packets.
    fn handle_ack(&mut self, acked_packet_id: u16) {
        trace!("got ack for command: 0x{acked_packet_id:04X}");
        if self.ack_queue.is_empty() {
            // Nothing outstanding to ack
            return;
        }

        // ack_queue is always sorted by sender_packet_id, but this is only a
        // u16, so it could wrap around.
        let min_pos = if acked_packet_id >= Self::OVERFLOW_MARGIN {
            // The acked packet is close to the overflow point, so set a minimum
            // sender_packet_id that we want to check
            let min_acked_packet_id =
                acked_packet_id.saturating_sub(Self::MAX_ACK_QUEUE_LENGTH as u16);
            self.ack_queue
                .partition_point(|p| p.pkt.sender_packet_id < min_acked_packet_id)
        } else {
            0
        };

        let pos = self
            .ack_queue
            .partition_point(|p| p.pkt.sender_packet_id <= acked_packet_id);
        if pos == 0 {
            // All packets waiting for ack are newer than this
            return;
        }

        for pending in self.ack_queue.drain(min_pos..pos) {
            let Some(responder) = pending.responder else {
                continue;
            };

            if responder.send(Ok(())).is_err() {
                error!(
                    "responder remote side gone ({})",
                    pending.pkt.sender_packet_id
                );
                continue;
            }
        }
    }

    /// Work through the [Self::ack_queue] and retransmit any outstanding
    /// packets beyond the deadline.
    async fn do_retransmits(&mut self) -> Result<(), Error> {
        let mut errors = Vec::new();

        for (i, pending) in self.ack_queue.iter_mut().enumerate() {
            if pending.last_attempt.elapsed() < Self::RETRANSMIT_TIME {
                // too soon to retransmit
                continue;
            }

            if pending.retry_limit == 0 {
                // No retransmits available, send timeout
                error!("packet timeout: 0x{:04X}", pending.pkt.sender_packet_id);
                errors.insert(0, (i, Error::Timeout));
                continue;
            }

            pending.retry_limit -= 1;
            pending.last_attempt = Instant::now();
            trace!("retransmitting packet: {:?}", pending.pkt);
            if let Err(e) = self.channel.send(&pending.pkt).await {
                errors.insert(0, (i, e));
                continue;
            }
        }

        // Collect any errored states
        for (i, e) in errors {
            let Some(pending) = self.ack_queue.remove(i) else {
                error!(
                    "tried to pop entry #{i} from {} entry ack_queue",
                    self.ack_queue.len()
                );
                return Err(Error::Internal);
            };

            if let Some(responder) = pending.responder {
                if responder.send(Err(e)).is_err() {
                    error!(
                        "responder remote side gone ({})",
                        pending.pkt.sender_packet_id
                    );
                    continue;
                }
            }
        }

        Ok(())
    }

    /// Queue a request for the device's clock.
    async fn request_clock(&mut self) -> Result<(), Error> {
        let (responder, resp_rx) = oneshot::channel();
        let async_cmd = AsyncCommand::Commands {
            cmds: vec![TIME_REQUEST_COMMAND.clone()],
            responder: Some(responder),
        };

        self.handle_queued_command(async_cmd).await?;

        let clock_notifier = self.clock_notifier.clone();
        tokio::task::spawn(async move {
            // Wait for a result from the resp_rx; this has a RecvError layer
            // then our Error type.
            if !resp_rx.await.is_ok_and(|r| r.is_ok()) {
                clock_notifier.notify_waiters();
            }
        });
        Ok(())
    }

    async fn handle_download_chunk(&mut self, cmd: Atom) -> Result<(), Error> {
        let Payload::TransferChunk(chunk) = cmd.payload else {
            return Err(Error::Internal);
        };

        let Some(download) = self.downloads.get_mut(&chunk.id) else {
            return Ok(());
        };

        let chunk_len = chunk.payload.len();
        download.bytes_received += chunk_len;
        download.bytes_since_last_ack += chunk_len;

        if download.tx.send(Ok(chunk.payload)).await.is_err() {
            error!("download channel disconnected!");
            self.downloads.remove(&chunk.id);
        }

        Ok(())
    }

    async fn handle_transfer_completed(&mut self, cmd: Atom) -> Result<(), Error> {
        let Payload::TransferCompleted(completed) = cmd.payload else {
            return Err(Error::Internal);
        };

        if let Some(download) = self.downloads.remove(&completed.id) {
            debug!("finished download: {:#04x}", completed.id);
            drop(download.storage_lock);
        } else if let Some((responder, storage_lock)) = self.finished_uploads.remove(&completed.id)
        {
            debug!("finished upload: {:#04x}", completed.id);
            if let Some(responder) = responder {
                if responder.send(Ok(())).is_err() {
                    error!("error notifying upload responder");
                }
            }
            drop(storage_lock);
        }

        Ok(())
    }

    /// Handle an incoming [FileTransferError][crate::protocol::atom::FileTransferError].
    async fn handle_transfer_error(&mut self, cmd: Atom) -> Result<(), Error> {
        let Payload::FileTransferError(error) = cmd.payload else {
            return Err(Error::Internal);
        };

        if let Some(download) = self.downloads.remove(&error.id) {
            error!(
                "error during download {:#04x}: {:#02x}",
                error.id, error.code
            );
            let tx = download.tx;
            tokio::task::spawn(async move {
                if tx
                    .send(Err(Error::SwitcherTransferError(error.code)))
                    .await
                    .is_err()
                {
                    error!("error notifying download channel");
                }
            });
            drop(download.storage_lock);
        } else if let Some(upload) = self.uploads.remove(&error.id) {
            error!("error during upload {:#04x}: {:#02x}", error.id, error.code);
            if let Some(responder) = upload.responder {
                if responder
                    .send(Err(Error::SwitcherTransferError(error.code)))
                    .is_err()
                {
                    error!("error notifying upload responder");
                }
            }
            drop(upload.storage_lock);
        } else if let Some((responder, storage_lock)) = self.finished_uploads.remove(&error.id) {
            error!(
                "error finishing upload {:#04x}: {:#02x}",
                error.id, error.code
            );
            if let Some(responder) = responder {
                if responder
                    .send(Err(Error::SwitcherTransferError(error.code)))
                    .is_err()
                {
                    error!("error notifying upload responder");
                }
            }
            drop(storage_lock);
        }

        Ok(())
    }

    /// Enqueues a [FileTransferChunkParams] for later processing in
    /// [`upload_chunk_params_rx`][Self::upload_chunk_params_rx].
    ///
    /// This ensures we ack the [FileTransferChunkParams] before we start
    /// sending a bunch of [TransferChunk]s.
    async fn enqueue_file_transfer_chunk_params(&mut self, cmd: Atom) -> Result<(), Error> {
        let Payload::FileTransferChunkParams(params) = cmd.payload else {
            return Err(Error::Internal);
        };

        self.upload_chunk_params_tx
            .send(params)
            .await
            .map_err(|_| Error::ChannelUnavailable)
    }

    /// Handles a [FileTransferChunkParams] message from
    /// [`upload_chunk_params_rx`][Self::upload_chunk_params_rx], and pushes
    async fn handle_file_transfer_chunk_params(
        &mut self,
        params: FileTransferChunkParams,
    ) -> Result<(), Error> {
        let Some(cmd_tx) = self.cmd_tx_weak.upgrade() else {
            error!("unable to upgrade cmd_tx_weak; AtemController dropped it?");
            return Err(Error::UnexpectedState);
        };

        let Some(upload) = self.uploads.get_mut(&params.id) else {
            // The switcher could have given us a FTCD when we've already queued
            // up the last chunk. So just ignore this.
            return Ok(());
        };

        upload.chunk_size = params.chunk_size;
        upload.chunks_remaining = params.chunk_count;
        let mtu = (TransferChunk::MAX_PAYLOAD_LENGTH.min(upload.chunk_size) & !0x7) as usize;
        if mtu <= 24 {
            error!(
                "swicher's MTU was {}, protocol MTU is {}, need at least 24 bytes for RLE",
                upload.chunk_size,
                TransferChunk::MAX_PAYLOAD_LENGTH,
            );
            return Err(Error::ParameterOutOfRange);
        }

        // The switcher has given us permission to send
        // `chunk_count` chunks of data. Now we need to fiddle our
        // data queue and figure out what we can send.
        let mut chunks = Vec::with_capacity(upload.chunks_remaining.into());
        while upload.chunks_remaining > 0 {
            let mut chunk = TransferChunk::new(params.id, mtu);

            while let Some(b) = upload.buffer.pop_front() {
                if b == RLE_MARKER && (chunk.payload.len() + 24) > mtu {
                    // We need to be able to push the entire RLE
                    // sequence (u64 * 3) in the same command, but
                    // there's not enough space.
                    // Put it back and save it for later.
                    upload.buffer.push_front(b);
                    break;
                }

                chunk.payload.extend_from_slice(&b.to_be_bytes());
                assert!(chunk.payload.len() <= mtu);
                if chunk.payload.len() >= mtu {
                    // Chunk is now full
                    break;
                }
            }

            if chunk.payload.is_empty() {
                // We've run out of data to send.
                break;
            }

            // We've got a full (enough) packet, now pop it on our
            // outbound packet queue
            // TODO: this stuff could propagate the error back to the uploader
            // properly
            chunks.push(Atom::new(chunk));
            upload.chunks_remaining -= 1;
        }

        let permit = upload.semaphore.clone().acquire_owned();
        if upload.buffer.is_empty() {
            // There's nothing more to upload, finish it.
            let Some(upload) = self.uploads.remove(&params.id) else {
                error!("upload ID {:#04x} disappeared?", params.id);
                return Err(Error::UnexpectedState);
            };

            chunks.push(Atom::new(FinishFileDownload {
                id: params.id,
                name: upload.name,
                description: upload.description,
                md5: upload.md5,
            }));
            if self
                .finished_uploads
                .insert(params.id, (upload.responder, upload.storage_lock))
                .is_some()
            {
                error!("upload {:#04x} already in the finished list?", params.id);
                return Err(Error::UnexpectedState);
            } else {
                info!("last chunk for upload {:#04x} in queue", params.id);
            };
        }

        // Make a worker which will do the actual sending
        // TODO: should signal errors back...
        tokio::task::spawn(async move {
            let Ok(permit) = permit.await else {
                error!("permit error?");
                return;
            };
            let mut burst = Vec::with_capacity(Self::UPLOAD_BURST_SIZE);
            for chunk in chunks {
                let (responder, resp_rx) = oneshot::channel();
                if cmd_tx
                    .send(AsyncCommand::Commands {
                        cmds: vec![chunk],
                        responder: Some(responder),
                    })
                    .await
                    .is_err()
                {
                    error!("failed to transmit chunk to worker");
                    return;
                }

                burst.push(resp_rx);
                if burst.len() >= Self::UPLOAD_BURST_SIZE {
                    // We've hit the limit for the number of packets we can send
                    // in a burst. Wait for the acks to come back before
                    // continuing.
                    if wait_for_acks(&mut burst).await.is_err() {
                        break;
                    };
                }
            }
            // Wait for any outstanding acks.
            let _ = wait_for_acks(&mut burst).await;
            drop(permit);
        });

        Ok(())
    }

    async fn handle_lock_obtained(&mut self, cmd: Atom) -> Result<(), Error> {
        let Payload::LockObtained(lkob) = cmd.payload else {
            return Err(Error::Internal);
        };

        let store_id = lkob.store_id;
        if store_id >= 64 {
            error!("unable to lock storage ID {store_id}, too large");
            return Err(Error::ParameterOutOfRange);
        }

        let Some(owned_storage_lock) = self.owned_storage_locks.get(&store_id) else {
            error!("No matching storage lock requests for storage ID {store_id}");
            return Err(Error::UnexpectedState);
        };

        let Some(storage_lock) = owned_storage_lock.upgrade() else {
            // TODO: we should probably unlock things if we didn't already?
            error!("unable to upgrade storage lock {store_id}, no notifier?");
            return Err(Error::Internal);
        };

        storage_lock.make_available();
        Ok(())
    }

    async fn handle_lock_state_changed(&mut self, cmd: Atom) -> Result<(), Error> {
        let Payload::MediaPoolLockStatus(lkst) = cmd.payload else {
            return Err(Error::Internal);
        };

        if lkst.locked {
            self.storage_locks.insert(lkst.store_id);
        } else {
            self.storage_locks.remove(&lkst.store_id);
            if let Some(storage_lock) = self
                .owned_storage_locks
                .remove(&lkst.store_id)
                .and_then(|l| l.upgrade())
            {
                storage_lock.make_unavailable();
            }
        }

        Ok(())
    }
}

/// Wait for acks to a collection of sent packets, draining the `Vec`.
///
/// On success, returns `Ok(())`, and `burst` will be empty.
///
/// On failure, returns the first [Error]. `burst` will still contain subsequent
/// `oneshot::Receiver`s after the [Error].
async fn wait_for_acks(burst: &mut Vec<oneshot::Receiver<Result<(), Error>>>) -> Result<(), Error> {
    for recv_rx in burst.drain(..) {
        match recv_rx.await.map_err(|_| Error::ChannelUnavailable) {
            Ok(Err(e)) | Err(e) => {
                error!("responder returned error: {e:?}");
                return Err(e);
            }
            Ok(Ok(())) => {}
        }
    }

    Ok(())
}

impl Drop for AtemReceiver {
    fn drop(&mut self) {
        if let Err(e) = self.disconnect() {
            error!("disconnection error: {e:?}");
        }
    }
}

/// A storage lock.
///
/// This keeps a storage lock alive using [Drop] semantics.
struct StorageLock {
    store_id: u16,
    waiters: Semaphore,
    cmd_tx: Sender<AsyncCommand>,
}

impl StorageLock {
    fn new(store_id: u16, cmd_tx: Sender<AsyncCommand>) -> Self {
        Self {
            store_id,
            waiters: Semaphore::new(0),
            cmd_tx,
        }
    }

    fn make_available(&self) {
        self.waiters.add_permits(Semaphore::MAX_PERMITS);
    }

    fn make_unavailable(&self) {
        self.waiters.close();
    }

    async fn await_availability(&self) -> Result<(), Error> {
        // We don't actually care about the permit itself, we would just drop it
        // immediately and that's OK.
        match self.waiters.acquire().await {
            Ok(_) => Ok(()),
            Err(_) => Err(Error::ChannelUnavailable),
        }
    }
}

impl Drop for StorageLock {
    fn drop(&mut self) {
        self.make_unavailable();
        let cmd = Atom::new(MediaPoolLock::unlock(self.store_id));

        let cmd_tx = self.cmd_tx.clone();
        tokio::task::spawn(async move {
            cmd_tx
                .send(AsyncCommand::Commands {
                    cmds: vec![cmd],
                    responder: None,
                })
                .await
                .unwrap();
        });
    }
}

lazy_static! {
    static ref TIME_REQUEST_COMMAND: Atom = Atom::new(Payload::TimecodeRequest(TimecodeRequest {}));
}

use anyhow::{anyhow, bail, Context};
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
#[cfg(target_os = "macos")]
use cocoa::base::{id, NO, YES};
use futures_util::{SinkExt, StreamExt};
#[cfg(target_os = "macos")]
use objc::{class, msg_send, sel, sel_impl};
use openh264::decoder::Decoder;
use openh264::formats::YUVSource;
use serde_json::Value;
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, VecDeque};
#[cfg(any(target_os = "windows", target_os = "macos"))]
use std::ffi::CStr;
use std::ffi::CString;
use std::hash::{Hash, Hasher};
use std::os::raw::c_char;
#[cfg(any(target_os = "windows", target_os = "linux"))]
use std::ptr;
#[cfg(target_os = "windows")]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::Message;
#[cfg(target_os = "windows")]
use windows::core::{IUnknown, Interface, Type};
#[cfg(target_os = "windows")]
use windows::Win32::Foundation::{BOOL, RECT};
#[cfg(target_os = "windows")]
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
};
#[cfg(target_os = "windows")]
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, ID3D11VideoContext,
    ID3D11VideoDevice, ID3D11VideoProcessor, ID3D11VideoProcessorEnumerator,
    ID3D11VideoProcessorOutputView, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION,
    D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE, D3D11_VIDEO_PROCESSOR_CONTENT_DESC,
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_STREAM, D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
    D3D11_VPIV_DIMENSION_TEXTURE2D, D3D11_VPOV_DIMENSION_TEXTURE2D,
};
#[cfg(target_os = "windows")]
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_RATIONAL, DXGI_SAMPLE_DESC,
};
#[cfg(target_os = "windows")]
use windows::Win32::Media::MediaFoundation::{
    CLSID_MSH264DecoderMFT, CODECAPI_AVLowLatencyMode, ICodecAPI, IMF2DBuffer, IMFDXGIBuffer,
    IMFDXGIDeviceManager, IMFMediaBuffer, IMFMediaType, IMFSample, IMFTransform,
    MFCreateDXGIDeviceManager, MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample,
    MFMediaType_Video, MFShutdown, MFStartup, MFVideoFormat_ARGB32, MFVideoFormat_H264,
    MFVideoFormat_NV12, MFVideoFormat_RGB32, MFVideoFormat_YUY2, MFVideoInterlace_Progressive,
    MFSTARTUP_NOSOCKET, MFT_MESSAGE_COMMAND_FLUSH, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
    MFT_MESSAGE_NOTIFY_END_OF_STREAM, MFT_MESSAGE_NOTIFY_START_OF_STREAM,
    MFT_MESSAGE_SET_D3D_MANAGER, MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES,
    MF_E_NOTACCEPTING, MF_E_NO_MORE_TYPES, MF_E_TRANSFORM_NEED_MORE_INPUT,
    MF_E_TRANSFORM_STREAM_CHANGE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE,
    MF_MT_SUBTYPE,
};
#[cfg(target_os = "windows")]
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};

const PROTOCOL_VERSION: i64 = 1;
const MSG_TYPE_VIDEO: u8 = 1;
const MSG_TYPE_AUDIO: u8 = 2;
const FLAG_KEYFRAME: u8 = 0x01;
const HELPER_STATS_INTERVAL: Duration = Duration::from_secs(1);
#[cfg(target_os = "windows")]
const SPOUT_SEND_WARN_INTERVAL: Duration = Duration::from_secs(5);
#[cfg(target_os = "windows")]
const SPOUT_KEEPALIVE_INTERVAL: Duration = Duration::from_millis(800);
const PERF_EWMA_ALPHA: f64 = 0.2;
#[cfg(target_os = "windows")]
const MF_STALL_SWITCH_THRESHOLD: u32 = 180;
#[cfg(target_os = "macos")]
const VT_STALL_SWITCH_THRESHOLD: u32 = 60;
#[cfg(target_os = "macos")]
const VT_STALL_HEX_DUMP_LIMIT: usize = 3;
#[cfg(target_os = "macos")]
const VT_STALL_HEX_DUMP_BYTES: usize = 64;
const KEYFRAME_RESYNC_EMPTY_THRESHOLD: u32 = 45;
const BACKLOG_SEVERE_THRESHOLD: u64 = 120;
const CATCHUP_ENTER_LAG_MS: f64 = 140.0;
const CATCHUP_EXIT_LAG_MS: f64 = 90.0;
const SCALE_FULL: u32 = 100;
const SCALE_MEDIUM: u32 = 75;
const SCALE_LOW: u32 = 30;
const FRAME_STATS_SAMPLE_DEFAULT: u64 = 30;
const FASTPATH_HIGH_SEND_MS_THRESHOLD: f64 = 22.0;
const FASTPATH_HIGH_SEND_STREAK_THRESHOLD: u32 = 8;
const FASTPATH_RETRY_COOLDOWN: Duration = Duration::from_secs(3);
const FASTPATH_RECOVERY_STREAK_REQUIRED: u32 = 6;
#[cfg(target_os = "windows")]
const FASTPATH_DECODE_STALL_THRESHOLD: u32 = 24;
const COMPRESSED_VIDEO_QUEUE_CAPACITY: usize = 4;
const PIPELINE_TICK_INTERVAL: Duration = Duration::from_millis(4);
const PLAYER_IDLE_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(target_os = "macos")]
const SYPHON_STATIC_PLAYER_IDS: [u32; 4] = [1, 2, 3, 4];

#[cfg(target_os = "windows")]
static MF_RUNTIME_REFS: AtomicUsize = AtomicUsize::new(0);

pub struct OutputHelperArgs {
    mode: OutputMode,
    ws_url: String,
}

#[derive(Clone, Copy)]
struct OutputHelperPerfConfig {
    verbose: bool,
    frame_stats_every: u64,
    texture_fastpath: bool,
}

impl OutputHelperPerfConfig {
    fn from_env() -> Self {
        let verbose = std::env::var("BROWSER_PORT_PERF_VERBOSE")
            .ok()
            .as_deref()
            .and_then(parse_env_bool)
            .unwrap_or(false);
        let frame_stats_every = std::env::var("BROWSER_PORT_FRAME_STATS_EVERY")
            .ok()
            .and_then(|raw| raw.trim().parse::<u64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or_else(|| {
                if verbose {
                    1
                } else {
                    FRAME_STATS_SAMPLE_DEFAULT
                }
            });
        let texture_fastpath = std::env::var("BROWSER_PORT_SPOUT_TEXTURE_FASTPATH")
            .ok()
            .as_deref()
            .and_then(parse_env_bool)
            .unwrap_or(true);
        Self {
            verbose,
            frame_stats_every,
            texture_fastpath,
        }
    }
}

fn parse_env_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

#[cfg(target_os = "macos")]
fn vt_fallback_disabled() -> bool {
    std::env::var("BROWSER_PORT_DISABLE_VT_FALLBACK")
        .ok()
        .as_deref()
        .and_then(parse_env_bool)
        .unwrap_or(false)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OutputMode {
    Spout,
    Syphon,
    Ndi,
}

impl OutputMode {
    fn as_str(self) -> &'static str {
        match self {
            OutputMode::Spout => "spout",
            OutputMode::Syphon => "syphon",
            OutputMode::Ndi => "ndi",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "spout" => Some(Self::Spout),
            "syphon" => Some(Self::Syphon),
            "ndi" => Some(Self::Ndi),
            _ => None,
        }
    }
}

pub fn parse_from_env() -> anyhow::Result<Option<OutputHelperArgs>> {
    let mut args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.is_empty() || args[0] != "output-helper" {
        return Ok(None);
    }
    args.remove(0);
    let mut mode: Option<OutputMode> = None;
    let mut ws_url = "ws://127.0.0.1:9876".to_string();
    let mut idx = 0;
    while idx < args.len() {
        let key = args[idx].as_str();
        idx += 1;
        let Some(value) = args.get(idx) else {
            bail!("missing value for argument {key}");
        };
        idx += 1;
        match key {
            "--mode" => {
                let Some(parsed) = OutputMode::parse(value) else {
                    bail!("invalid --mode value: {value}");
                };
                mode = Some(parsed);
            }
            "--ws" => ws_url = value.to_string(),
            _ => bail!("unsupported argument: {key}"),
        }
    }
    let Some(mode) = mode else {
        bail!("--mode is required");
    };
    Ok(Some(OutputHelperArgs { mode, ws_url }))
}

struct PendingVideoChunk {
    payload: Vec<u8>,
    keyframe: bool,
    timestamp_us: u64,
}

pub async fn run(args: OutputHelperArgs) -> anyhow::Result<()> {
    let mut backend =
        OutputBackend::new(args.mode).context("failed to initialize output backend")?;
    let decode_preference = DecodeBackendPreference::from_env();
    let perf_config = OutputHelperPerfConfig::from_env();
    eprintln!(
        "output-helper: decode backend preference={}",
        decode_preference.as_str()
    );
    let mut decoders: HashMap<u32, DecoderState> = HashMap::new();
    let mut pending_video: HashMap<u32, VecDeque<PendingVideoChunk>> = HashMap::new();
    let mut latest_decoded: HashMap<u32, DecodedFrame> = HashMap::new();
    let mut last_stats_at = Instant::now();
    loop {
        let ws_stream = match tokio_tungstenite::connect_async(&args.ws_url).await {
            Ok(value) => value,
            Err(_) => {
                tokio::time::sleep(tokio::time::Duration::from_millis(800)).await;
                continue;
            }
        };
        let (mut ws, _) = ws_stream;
        ws.send(Message::Text(
            serde_json::json!({
                "type": "hello",
                "role": "client",
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {
                    "name": "rust-output-helper",
                    "outputMode": args.mode.as_str(),
                }
            })
            .to_string()
            .into(),
        ))
        .await
        .context("failed to send hello")?;
        let mut pipeline_tick = tokio::time::interval(PIPELINE_TICK_INTERVAL);
        pipeline_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        pipeline_tick.tick().await;

        'connection: loop {
            tokio::select! {
                next = ws.next() => {
                    let Some(next) = next else {
                        break 'connection;
                    };
                    let message = match next {
                        Ok(message) => message,
                        Err(_) => break 'connection,
                    };
                    match message {
                        Message::Binary(raw) => {
                            if let Some(info) = parse_chunk_header(&raw) {
                                if info.msg_type == MSG_TYPE_VIDEO {
                                    enqueue_video_chunk(
                                        &mut decoders,
                                        &mut pending_video,
                                        info.player_id,
                                        info.flags & FLAG_KEYFRAME != 0,
                                        info.timestamp_us,
                                        info.payload,
                                        decode_preference,
                                        perf_config,
                                    );
                                } else if info.msg_type == MSG_TYPE_AUDIO && args.mode == OutputMode::Ndi {
                                    backend.send_audio_ndi(info.player_id, &info.payload);
                                }
                            }
                        }
                        Message::Text(text) => {
                            handle_text_message(
                                &mut decoders,
                                &mut backend,
                                &mut pending_video,
                                &mut latest_decoded,
                                &text,
                                decode_preference,
                                perf_config,
                            );
                        }
                        Message::Ping(payload) => {
                            let _ = ws.send(Message::Pong(payload)).await;
                        }
                        Message::Close(_) => break 'connection,
                        _ => {}
                    }
                }
                _ = pipeline_tick.tick() => {}
            }

            run_decode_stage(
                &mut decoders,
                &mut pending_video,
                &mut latest_decoded,
                decode_preference,
                perf_config,
            );
            run_send_stage(&mut backend, &mut decoders, &mut latest_decoded);
            reap_idle_players(
                &mut backend,
                &mut decoders,
                &mut pending_video,
                &mut latest_decoded,
            );
            backend.tick();

            if last_stats_at.elapsed() >= HELPER_STATS_INTERVAL {
                send_helper_stats(
                    &mut ws,
                    args.mode,
                    &decoders,
                    backend.syphon_client_count(),
                    perf_config.verbose,
                )
                .await;
                last_stats_at = Instant::now();
            }
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(800)).await;
    }
}

fn enqueue_video_chunk(
    decoders: &mut HashMap<u32, DecoderState>,
    pending_video: &mut HashMap<u32, VecDeque<PendingVideoChunk>>,
    player_id: u32,
    keyframe: bool,
    timestamp_us: u64,
    payload: Vec<u8>,
    decode_preference: DecodeBackendPreference,
    perf_config: OutputHelperPerfConfig,
) {
    let decoder = decoders
        .entry(player_id)
        .or_insert_with(|| DecoderState::new(decode_preference, perf_config));
    decoder.observe_chunk_received();
    let queue = pending_video.entry(player_id).or_default();
    queue.push_back(PendingVideoChunk {
        payload,
        keyframe,
        timestamp_us,
    });
    if queue.len() > COMPRESSED_VIDEO_QUEUE_CAPACITY {
        let dropped = queue.len().saturating_sub(COMPRESSED_VIDEO_QUEUE_CAPACITY);
        queue.drain(0..dropped);
        decoder.observe_pipeline_drop(dropped as u64, "compressed-queue-overflow");
    }
}

fn run_decode_stage(
    decoders: &mut HashMap<u32, DecoderState>,
    pending_video: &mut HashMap<u32, VecDeque<PendingVideoChunk>>,
    latest_decoded: &mut HashMap<u32, DecodedFrame>,
    decode_preference: DecodeBackendPreference,
    perf_config: OutputHelperPerfConfig,
) {
    let players = pending_video.keys().copied().collect::<Vec<_>>();
    for player_id in players {
        let mut should_remove_queue = false;
        let decoder = decoders
            .entry(player_id)
            .or_insert_with(|| DecoderState::new(decode_preference, perf_config));

        if let Some(queue) = pending_video.get_mut(&player_id) {
            while let Some(chunk) = queue.pop_front() {
                if let Some(frame) =
                    decoder.decode(&chunk.payload, chunk.keyframe, chunk.timestamp_us)
                {
                    latest_decoded.insert(player_id, frame);
                }
            }
            should_remove_queue = queue.is_empty();
        }
        if should_remove_queue {
            pending_video.remove(&player_id);
        }
    }
}

fn run_send_stage(
    backend: &mut OutputBackend,
    decoders: &mut HashMap<u32, DecoderState>,
    latest_decoded: &mut HashMap<u32, DecodedFrame>,
) {
    let players = latest_decoded.keys().copied().collect::<Vec<_>>();
    for player_id in players {
        let Some(frame) = latest_decoded.remove(&player_id) else {
            continue;
        };
        let send_started = Instant::now();
        let send_result = backend.send_video(player_id, &frame);
        if let Some(decoder) = decoders.get_mut(&player_id) {
            decoder.observe_send(send_started.elapsed(), &send_result);
        }
    }
}

fn reap_idle_players(
    backend: &mut OutputBackend,
    decoders: &mut HashMap<u32, DecoderState>,
    pending_video: &mut HashMap<u32, VecDeque<PendingVideoChunk>>,
    latest_decoded: &mut HashMap<u32, DecodedFrame>,
) {
    let players = decoders.keys().copied().collect::<Vec<_>>();
    for player_id in players {
        if pending_video
            .get(&player_id)
            .map(|queue| !queue.is_empty())
            .unwrap_or(false)
        {
            continue;
        }
        let is_idle = decoders
            .get(&player_id)
            .map(|decoder| decoder.is_idle())
            .unwrap_or(false);
        if !is_idle {
            continue;
        }
        backend.clear_player(player_id, "idle-timeout");
        pending_video.remove(&player_id);
        latest_decoded.remove(&player_id);
        decoders.remove(&player_id);
    }
}

fn build_decoder_config_fingerprint(
    coded_width: Option<usize>,
    coded_height: Option<usize>,
    description_b64: Option<&str>,
) -> u64 {
    let mut hasher = DefaultHasher::new();
    coded_width.hash(&mut hasher);
    coded_height.hash(&mut hasher);
    description_b64.unwrap_or("").hash(&mut hasher);
    hasher.finish()
}

fn handle_text_message(
    decoders: &mut HashMap<u32, DecoderState>,
    backend: &mut OutputBackend,
    pending_video: &mut HashMap<u32, VecDeque<PendingVideoChunk>>,
    latest_decoded: &mut HashMap<u32, DecodedFrame>,
    text: &str,
    decode_preference: DecodeBackendPreference,
    perf_config: OutputHelperPerfConfig,
) {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return;
    };
    if value.get("type").and_then(Value::as_str) != Some("config") {
        return;
    }
    let Some(player_id) = value
        .get("playerId")
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
    else {
        return;
    };
    let codec = value.get("codec").and_then(Value::as_str).unwrap_or("");
    if !codec.is_empty() && !codec.to_ascii_lowercase().contains("avc") && !codec.contains("h264") {
        backend.clear_player(player_id, "codec-not-h264");
        decoders.remove(&player_id);
        pending_video.remove(&player_id);
        latest_decoded.remove(&player_id);
        return;
    }
    let decoder = decoders
        .entry(player_id)
        .or_insert_with(|| DecoderState::new(decode_preference, perf_config));
    decoder.observe_config_received();
    let coded_width = value
        .get("codedWidth")
        .and_then(Value::as_u64)
        .and_then(|v| usize::try_from(v).ok())
        .filter(|v| *v > 0);
    let coded_height = value
        .get("codedHeight")
        .and_then(Value::as_u64)
        .and_then(|v| usize::try_from(v).ok())
        .filter(|v| *v > 0);
    let description_b64 = value.get("description").and_then(Value::as_str);
    let config_fingerprint =
        build_decoder_config_fingerprint(coded_width, coded_height, description_b64);
    let preferred_dx11_device = backend.configure_player(player_id, coded_width, coded_height);
    decoder.set_preferred_dx11_device(preferred_dx11_device);
    decoder.set_coded_size(coded_width, coded_height);
    let should_reset = decoder.last_config_fingerprint != Some(config_fingerprint);
    if should_reset {
        pending_video.remove(&player_id);
        latest_decoded.remove(&player_id);
        decoder.reset_decoder();
        if let Some(description_b64) = description_b64 {
            if !description_b64.is_empty() {
                eprintln!(
                    "output-helper: config player={} description_b64_len={}",
                    player_id,
                    description_b64.len()
                );
                match BASE64_STANDARD.decode(description_b64) {
                    Ok(bytes) => {
                        eprintln!(
                            "output-helper: config player={} avcc_bytes={}",
                            player_id,
                            bytes.len()
                        );
                        match parse_avcc_record(&bytes) {
                            Some((nal_length_size, parameter_sets, sps, pps)) => {
                                let sps_sizes = sps
                                    .iter()
                                    .map(|set| set.len().to_string())
                                    .collect::<Vec<_>>()
                                    .join(",");
                                let pps_sizes = pps
                                    .iter()
                                    .map(|set| set.len().to_string())
                                    .collect::<Vec<_>>()
                                    .join(",");
                                eprintln!(
                                    "output-helper: config player={} avcc nal_length_size={} sps_count={} sps_sizes=[{}] pps_count={} pps_sizes=[{}] parameter_sets_total={}",
                                    player_id,
                                    nal_length_size,
                                    sps.len(),
                                    sps_sizes,
                                    pps.len(),
                                    pps_sizes,
                                    parameter_sets.len()
                                );
                            }
                            None => {
                                eprintln!(
                                    "output-helper: config player={} failed to parse avcc record",
                                    player_id
                                );
                            }
                        }
                        if !decoder.update_avcc_from_base64(description_b64) {
                            eprintln!(
                                "output-helper: failed to parse H.264 decoder config for player {}",
                                player_id
                            );
                            decoder.clear_avcc();
                        }
                    }
                    Err(err) => {
                        eprintln!(
                            "output-helper: failed to base64 decode H.264 config player={} err={}",
                            player_id, err
                        );
                        decoder.clear_avcc();
                    }
                }
            } else {
                decoder.clear_avcc();
            }
        } else {
            eprintln!(
                "output-helper: config player={} no description avcc present",
                player_id
            );
        }
        decoder.last_config_fingerprint = Some(config_fingerprint);
    }
}

async fn send_helper_stats<S>(
    ws: &mut S,
    mode: OutputMode,
    decoders: &HashMap<u32, DecoderState>,
    syphon_client_count: usize,
    verbose: bool,
) where
    S: futures_util::sink::Sink<Message> + Unpin,
{
    let mut players = Vec::with_capacity(decoders.len());
    for (player_id, decoder) in decoders {
        let perf = decoder.perf_metrics();
        players.push(serde_json::json!({
            "playerId": player_id,
            "decodeBackend": perf.backend,
            "decodeMs": perf.decode_ms,
            "convertMs": perf.convert_ms,
            "spoutSendMs": perf.spout_send_ms,
            "queueDepth": perf.queue_depth,
            "frameMeanLuma": perf.frame_mean_luma,
            "frameNonBlackRatio": perf.frame_non_black_ratio,
            "sendPath": perf.send_path,
            "mfProcessOutputMs": perf.mf_process_output_ms,
            "cpuColorConvertMs": perf.cpu_color_convert_ms,
            "frameStatsMs": perf.frame_stats_ms,
            "spoutBridgeMs": perf.spout_bridge_ms,
            "spoutSwapMs": perf.spout_swap_ms,
            "spoutUploadMs": perf.spout_upload_ms,
            "spoutSendTextureMs": perf.spout_send_texture_ms,
            "texturePathRatio": perf.texture_path_ratio,
            "streamLatencyMs": perf.stream_latency_ms,
            "fastpathState": perf.fastpath_state,
            "fastpathFallbackCount": perf.fastpath_fallback_count,
            "fastpathRecoverCount": perf.fastpath_recover_count,
            "cropApplied": perf.crop_applied,
            "effectiveWidth": perf.effective_width,
            "effectiveHeight": perf.effective_height,
        }));
        if verbose {
            eprintln!(
                "output-helper: perf player={} backend={} decode_ms={:.2} convert_ms={:.2} send_ms={:.2} queue={} path={} bridge_ms={:.2} texture_ratio={:.3}",
                player_id,
                perf.backend,
                perf.decode_ms,
                perf.convert_ms,
                perf.spout_send_ms,
                perf.queue_depth,
                perf.send_path,
                perf.spout_bridge_ms,
                perf.texture_path_ratio
            );
        }
    }
    let payload = serde_json::json!({
        "type": "helper-stats",
        "source": "output-helper",
        "mode": mode.as_str(),
        "syphonClientCount": syphon_client_count,
        "players": players,
    });
    let _ = ws.send(Message::Text(payload.to_string().into())).await;
}

struct ChunkInfo {
    msg_type: u8,
    flags: u8,
    player_id: u32,
    timestamp_us: u64,
    payload: Vec<u8>,
}

fn parse_chunk_header(raw: &[u8]) -> Option<ChunkInfo> {
    if raw.len() < 16 {
        return None;
    }
    let msg_type = raw[0];
    let version = raw[1];
    if version == 1 {
        if raw.len() < 20 {
            return None;
        }
        let flags = raw[2];
        let player_id = u32::from_le_bytes([raw[4], raw[5], raw[6], raw[7]]);
        let timestamp_us = u64::from_le_bytes([
            raw[8], raw[9], raw[10], raw[11], raw[12], raw[13], raw[14], raw[15],
        ]);
        let payload_len = u32::from_le_bytes([raw[16], raw[17], raw[18], raw[19]]) as usize;
        if raw.len() < 20 + payload_len {
            return None;
        }
        return Some(ChunkInfo {
            msg_type,
            flags,
            player_id,
            timestamp_us,
            payload: raw[20..20 + payload_len].to_vec(),
        });
    }

    let flags = raw[1];
    let header_size = u16::from_le_bytes([raw[2], raw[3]]) as usize;
    if header_size < 16 || raw.len() < header_size {
        return None;
    }
    let player_id = u32::from_le_bytes([raw[4], raw[5], raw[6], raw[7]]);
    let timestamp_us = u64::from_le_bytes([
        raw[8], raw[9], raw[10], raw[11], raw[12], raw[13], raw[14], raw[15],
    ]);
    Some(ChunkInfo {
        msg_type,
        flags,
        player_id,
        timestamp_us,
        payload: raw[header_size..].to_vec(),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DecodeBackendPreference {
    Auto,
    #[cfg(target_os = "macos")]
    VideoToolbox,
    Mf,
    OpenH264,
}

impl DecodeBackendPreference {
    fn from_env() -> Self {
        let Ok(raw) = std::env::var("BROWSER_PORT_DECODE_BACKEND") else {
            return Self::Auto;
        };
        parse_decode_backend_preference(&raw).unwrap_or(Self::Auto)
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            #[cfg(target_os = "macos")]
            Self::VideoToolbox => "videotoolbox",
            Self::Mf => "mf",
            Self::OpenH264 => "openh264",
        }
    }
}

fn parse_decode_backend_preference(value: &str) -> Option<DecodeBackendPreference> {
    match value.trim().to_ascii_lowercase().as_str() {
        "auto" => Some(DecodeBackendPreference::Auto),
        #[cfg(target_os = "macos")]
        "vt" | "videotoolbox" => Some(DecodeBackendPreference::VideoToolbox),
        "mf" => Some(DecodeBackendPreference::Mf),
        "openh264" => Some(DecodeBackendPreference::OpenH264),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DecodeBackendKind {
    #[cfg(target_os = "macos")]
    VideoToolbox,
    #[cfg(target_os = "windows")]
    MfD3d11,
    OpenH264,
}

impl DecodeBackendKind {
    fn as_str(self) -> &'static str {
        match self {
            #[cfg(target_os = "macos")]
            DecodeBackendKind::VideoToolbox => "videotoolbox",
            #[cfg(target_os = "windows")]
            DecodeBackendKind::MfD3d11 => "mf-d3d11",
            DecodeBackendKind::OpenH264 => "openh264",
        }
    }
}

fn preferred_backend_order(preference: DecodeBackendPreference) -> Vec<DecodeBackendKind> {
    let mut order = Vec::new();
    match preference {
        DecodeBackendPreference::OpenH264 => {
            order.push(DecodeBackendKind::OpenH264);
        }
        #[cfg(target_os = "macos")]
        DecodeBackendPreference::VideoToolbox => {
            order.push(DecodeBackendKind::VideoToolbox);
            order.push(DecodeBackendKind::OpenH264);
        }
        DecodeBackendPreference::Mf | DecodeBackendPreference::Auto => {
            #[cfg(target_os = "macos")]
            {
                order.push(DecodeBackendKind::VideoToolbox);
            }
            #[cfg(target_os = "windows")]
            {
                order.push(DecodeBackendKind::MfD3d11);
            }
            order.push(DecodeBackendKind::OpenH264);
        }
    }
    if order.is_empty() {
        order.push(DecodeBackendKind::OpenH264);
    }
    order
}

#[cfg(test)]
fn select_backend_with<F>(
    preference: DecodeBackendPreference,
    mut is_available: F,
) -> Option<DecodeBackendKind>
where
    F: FnMut(DecodeBackendKind) -> bool,
{
    preferred_backend_order(preference)
        .into_iter()
        .find(|backend| is_available(*backend))
}

#[derive(Clone, Copy, Default)]
struct DecodeTimings {
    decode: Duration,
    convert: Duration,
    mf_process_output: Duration,
    cpu_color_convert: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SendPath {
    None,
    #[cfg(target_os = "windows")]
    Bgra,
    Texture,
    Ndi,
    #[cfg(target_os = "macos")]
    SyphonBgra,
    #[cfg(target_os = "macos")]
    SyphonMetalTexture,
}

impl SendPath {
    fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            #[cfg(target_os = "windows")]
            Self::Bgra => "bgra",
            Self::Texture => "texture",
            Self::Ndi => "ndi",
            #[cfg(target_os = "macos")]
            Self::SyphonBgra => "syphon-bgra",
            #[cfg(target_os = "macos")]
            Self::SyphonMetalTexture => "syphon-metal-texture",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FastpathState {
    Texture,
    BgraFallback,
    Retrying,
}

impl FastpathState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Texture => "texture",
            Self::BgraFallback => "bgra-fallback",
            Self::Retrying => "retrying",
        }
    }
}

#[derive(Clone, Copy)]
struct SpoutBridgeMetrics {
    swap_ms: f64,
    upload_ms: f64,
    send_ms: f64,
    total_ms: f64,
}

#[derive(Clone, Copy)]
struct VideoSendResult {
    sent: bool,
    path: SendPath,
    spout_bridge_metrics: Option<SpoutBridgeMetrics>,
    texture_attempted: bool,
    texture_failed: bool,
}

impl VideoSendResult {
    fn not_sent() -> Self {
        Self {
            sent: false,
            path: SendPath::None,
            spout_bridge_metrics: None,
            texture_attempted: false,
            texture_failed: false,
        }
    }
}

#[cfg(target_os = "macos")]
type CVPixelBufferRef = *mut std::ffi::c_void;
#[cfg(target_os = "macos")]
type CFTypeRef = *const std::ffi::c_void;
#[cfg(target_os = "macos")]
type CFAllocatorRef = *const std::ffi::c_void;
#[cfg(target_os = "macos")]
type OSStatus = i32;

#[cfg(target_os = "macos")]
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFAllocatorDefault: CFAllocatorRef;
    fn CFRelease(value: CFTypeRef);
}

#[cfg(target_os = "macos")]
#[link(name = "CoreVideo", kind = "framework")]
extern "C" {
    fn CVPixelBufferRetain(pixel_buffer: CVPixelBufferRef) -> CVPixelBufferRef;
    fn CVPixelBufferGetWidth(pixel_buffer: CVPixelBufferRef) -> usize;
    fn CVPixelBufferGetHeight(pixel_buffer: CVPixelBufferRef) -> usize;
    fn CVPixelBufferGetPixelFormatType(pixel_buffer: CVPixelBufferRef) -> u32;
    fn CVPixelBufferGetPlaneCount(pixel_buffer: CVPixelBufferRef) -> usize;
    fn CVPixelBufferGetIOSurface(pixel_buffer: CVPixelBufferRef) -> *mut std::ffi::c_void;
    fn CVPixelBufferRelease(pixel_buffer: CVPixelBufferRef);
}

#[cfg(target_os = "macos")]
#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    fn CMVideoFormatDescriptionCreateFromH264ParameterSets(
        allocator: CFAllocatorRef,
        parameter_set_count: usize,
        parameter_set_pointers: *const *const u8,
        parameter_set_sizes: *const usize,
        nal_unit_header_length: i32,
        out_desc: *mut *mut std::ffi::c_void,
    ) -> OSStatus;
    fn CMBlockBufferCreateWithMemoryBlock(
        allocator: CFAllocatorRef,
        memory_block: *mut std::ffi::c_void,
        block_length: usize,
        block_allocator: CFAllocatorRef,
        custom_block_source: *const std::ffi::c_void,
        offset_to_data: usize,
        data_length: usize,
        flags: u32,
        block_buffer_out: *mut *mut std::ffi::c_void,
    ) -> OSStatus;
    fn CMBlockBufferReplaceDataBytes(
        source_bytes: *const std::ffi::c_void,
        block_buffer: *mut std::ffi::c_void,
        offset_into_destination: usize,
        data_length: usize,
    ) -> OSStatus;
    fn CMSampleBufferCreateReady(
        allocator: CFAllocatorRef,
        data_buffer: *mut std::ffi::c_void,
        format_description: *mut std::ffi::c_void,
        num_samples: usize,
        num_sample_timing_entries: usize,
        sample_timing_array: *const std::ffi::c_void,
        num_sample_size_entries: usize,
        sample_size_array: *const usize,
        sample_buffer_out: *mut *mut std::ffi::c_void,
    ) -> OSStatus;
}

#[cfg(target_os = "macos")]
#[link(name = "VideoToolbox", kind = "framework")]
extern "C" {
    static kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder:
        *const std::ffi::c_void;
    static kVTVideoDecoderSpecification_EnableHardwareAcceleratedVideoDecoder:
        *const std::ffi::c_void;
    static kVTDecompressionPropertyKey_UsingHardwareAcceleratedVideoDecoder:
        *const std::ffi::c_void;
    static kCVPixelBufferMetalCompatibilityKey: *const std::ffi::c_void;
    static kCVPixelBufferIOSurfacePropertiesKey: *const std::ffi::c_void;
    static kCVPixelBufferPixelFormatTypeKey: *const std::ffi::c_void;
    fn VTDecompressionSessionCreate(
        allocator: CFAllocatorRef,
        video_format_description: *mut std::ffi::c_void,
        decoder_specification: *const std::ffi::c_void,
        destination_image_buffer_attributes: *const std::ffi::c_void,
        output_callback: *const std::ffi::c_void,
        decompression_session_out: *mut *mut std::ffi::c_void,
    ) -> OSStatus;
    fn VTDecompressionSessionDecodeFrame(
        session: *mut std::ffi::c_void,
        sample_buffer: *mut std::ffi::c_void,
        flags: u32,
        source_frame_ref_con: *mut std::ffi::c_void,
        info_flags_out: *mut u32,
    ) -> OSStatus;
    fn VTDecompressionSessionWaitForAsynchronousFrames(session: *mut std::ffi::c_void) -> OSStatus;
    fn VTDecompressionSessionInvalidate(session: *mut std::ffi::c_void);
    fn VTSessionCopyProperty(
        session: *mut std::ffi::c_void,
        property_key: *const std::ffi::c_void,
        allocator: CFAllocatorRef,
        property_value_out: *mut *mut std::ffi::c_void,
    ) -> OSStatus;
    fn CFBooleanGetValue(boolean: *const std::ffi::c_void) -> bool;
}

#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Clone, Copy)]
struct CMTime {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}

#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Clone, Copy)]
struct CMSampleTimingInfo {
    duration: CMTime,
    presentation_time_stamp: CMTime,
    decode_time_stamp: CMTime,
}

#[cfg(target_os = "macos")]
const KCV_PIXELFORMAT_32_BGRA: u32 = 0x4247_5241;

#[cfg(target_os = "macos")]
#[derive(Default)]
struct DecodedFrame {
    width: usize,
    height: usize,
    crop_applied: bool,
    bgra: Vec<u8>,
    #[cfg(target_os = "macos")]
    cv_pixel_buffer: Option<CvPixelBufferHandle>,
    #[cfg(target_os = "windows")]
    dx11_texture: Option<*mut std::ffi::c_void>,
    #[cfg(target_os = "windows")]
    dx11_device: Option<*mut std::ffi::c_void>,
}

#[cfg(target_os = "macos")]
struct CvPixelBufferHandle {
    raw: CVPixelBufferRef,
}

#[cfg(target_os = "macos")]
impl CvPixelBufferHandle {
    unsafe fn retain(raw: CVPixelBufferRef) -> Option<Self> {
        if raw.is_null() {
            return None;
        }
        // Retain the decoded pixel buffer so it survives after VideoToolbox returns.
        let retained = CVPixelBufferRetain(raw);
        if retained.is_null() {
            return None;
        }
        Some(Self { raw: retained })
    }

    fn as_raw(&self) -> CVPixelBufferRef {
        self.raw
    }
}

#[cfg(target_os = "macos")]
impl Drop for CvPixelBufferHandle {
    fn drop(&mut self) {
        unsafe {
            CVPixelBufferRelease(self.raw);
        }
    }
}

#[cfg(target_os = "macos")]
unsafe fn ns_bool(value: bool) -> id {
    let number: id = msg_send![class!(NSNumber), numberWithBool: if value { YES } else { NO }];
    number
}

#[cfg(target_os = "macos")]
unsafe fn ns_u32(value: u32) -> id {
    let number: id = msg_send![class!(NSNumber), numberWithUnsignedInt: value];
    number
}

#[cfg(target_os = "macos")]
unsafe fn ns_mutable_dictionary() -> id {
    let dict: id = msg_send![class!(NSMutableDictionary), dictionary];
    dict
}

#[cfg(target_os = "macos")]
unsafe fn ns_dictionary_set(dict: id, key: *const std::ffi::c_void, value: id) {
    let key = key as id;
    let _: () = msg_send![dict, setObject: value forKey: key];
}

#[cfg(target_os = "macos")]
fn vt_verbose_enabled() -> bool {
    matches!(
        std::env::var("BROWSER_PORT_VT_VERBOSE").ok().as_deref(),
        Some("1")
            | Some("true")
            | Some("TRUE")
            | Some("yes")
            | Some("YES")
            | Some("on")
            | Some("ON")
    )
}

trait VideoDecoder {
    fn backend_kind(&self) -> DecodeBackendKind;
    fn decode_into(&mut self, packet: &[u8], frame: &mut DecodedFrame) -> Option<DecodeTimings>;
    fn set_expected_size(&mut self, _width: usize, _height: usize) {}
    fn set_texture_fastpath_enabled(&mut self, _enabled: bool) {}
    fn flush(&mut self) {}
}

struct OpenH264Decoder {
    decoder: Decoder,
}

impl OpenH264Decoder {
    fn new() -> anyhow::Result<Self> {
        let decoder = Decoder::new().map_err(|err| anyhow!("openh264 decoder create: {err:?}"))?;
        Ok(Self { decoder })
    }
}

impl VideoDecoder for OpenH264Decoder {
    fn backend_kind(&self) -> DecodeBackendKind {
        DecodeBackendKind::OpenH264
    }

    fn decode_into(&mut self, packet: &[u8], frame: &mut DecodedFrame) -> Option<DecodeTimings> {
        let decode_started = Instant::now();
        let Ok(Some(yuv)) = self.decoder.decode(packet) else {
            return None;
        };
        let decode_elapsed = decode_started.elapsed();

        let convert_started = Instant::now();
        let (width, height) = yuv.dimensions();
        let needed = yuv.rgba8_len();
        if frame.bgra.len() != needed {
            frame.bgra.resize(needed, 0);
        }
        yuv.write_rgba8(&mut frame.bgra);
        for px in frame.bgra.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
        frame.width = width;
        frame.height = height;
        frame.crop_applied = false;
        #[cfg(target_os = "macos")]
        {
            frame.cv_pixel_buffer = None;
        }
        #[cfg(target_os = "windows")]
        {
            frame.dx11_texture = None;
            frame.dx11_device = None;
        }
        let convert_elapsed = convert_started.elapsed();

        Some(DecodeTimings {
            decode: decode_elapsed,
            convert: convert_elapsed,
            mf_process_output: Duration::ZERO,
            cpu_color_convert: convert_elapsed,
        })
    }
}

#[cfg(target_os = "macos")]
#[repr(C)]
struct VTDecompressionOutputCallbackRecord {
    decompression_output_callback: Option<
        unsafe extern "C" fn(
            decompression_output_ref_con: *mut std::ffi::c_void,
            source_frame_ref_con: *mut std::ffi::c_void,
            status: OSStatus,
            info_flags: u32,
            image_buffer: CVPixelBufferRef,
            presentation_time_stamp: CMTime,
            presentation_duration: CMTime,
        ),
    >,
    decompression_output_ref_con: *mut std::ffi::c_void,
}

#[cfg(target_os = "macos")]
struct VideoToolboxOutputState {
    pixel_buffer: Option<CvPixelBufferHandle>,
    status: OSStatus,
    image_buffer_was_null: bool,
}

#[cfg(target_os = "macos")]
#[derive(Clone, Default)]
struct VideoToolboxH264Config {
    nal_length_size: usize,
    parameter_sets: Vec<Vec<u8>>,
}

#[cfg(target_os = "macos")]
unsafe extern "C" fn video_toolbox_output_callback(
    decompression_output_ref_con: *mut std::ffi::c_void,
    source_frame_ref_con: *mut std::ffi::c_void,
    status: OSStatus,
    _info_flags: u32,
    image_buffer: CVPixelBufferRef,
    _presentation_time_stamp: CMTime,
    _presentation_duration: CMTime,
) {
    let verbose = vt_verbose_enabled();
    if verbose {
        eprintln!(
            "output-helper: videotoolbox callback start status={status} source_frame_ref_con_null={} decompression_output_ref_con_null={} image_buffer_null={}",
            source_frame_ref_con.is_null(),
            decompression_output_ref_con.is_null(),
            image_buffer.is_null()
        );
    }
    let state_ptr = if !source_frame_ref_con.is_null() {
        if verbose {
            eprintln!("output-helper: videotoolbox callback using state=source_frame_ref_con");
        }
        source_frame_ref_con
    } else if !decompression_output_ref_con.is_null() {
        if verbose {
            eprintln!(
                "output-helper: videotoolbox callback using state=decompression_output_ref_con"
            );
        }
        decompression_output_ref_con
    } else {
        if verbose {
            eprintln!(
                "output-helper: videotoolbox callback dropped because both state refs were null"
            );
        }
        return;
    };
    let state = &mut *(state_ptr as *mut VideoToolboxOutputState);
    state.status = status;
    state.image_buffer_was_null = image_buffer.is_null();
    state.pixel_buffer = CvPixelBufferHandle::retain(image_buffer);
    if verbose {
        eprintln!(
            "output-helper: videotoolbox callback wrote status={} image_buffer_null={} pixel_buffer_present={}",
            state.status,
            state.image_buffer_was_null,
            state.pixel_buffer.is_some()
        );
    }
}

#[cfg(target_os = "macos")]
struct VideoToolboxDecoder {
    session: Option<*mut std::ffi::c_void>,
    format_description: Option<*mut std::ffi::c_void>,
    config: Option<VideoToolboxH264Config>,
    expected_width: usize,
    expected_height: usize,
    frame_width: usize,
    frame_height: usize,
    hardware_requested: bool,
    last_error_log: Option<Instant>,
    callback_record: Option<Box<VTDecompressionOutputCallbackRecord>>,
    chunk_index: u64,
    verbose: bool,
}

#[cfg(target_os = "macos")]
impl VideoToolboxDecoder {
    fn new(config: Option<VideoToolboxH264Config>, verbose: bool) -> anyhow::Result<Self> {
        Ok(Self {
            session: None,
            format_description: None,
            config,
            expected_width: 0,
            expected_height: 0,
            frame_width: 0,
            frame_height: 0,
            hardware_requested: true,
            last_error_log: None,
            callback_record: None,
            chunk_index: 0,
            verbose,
        })
    }

    fn log_error_rate_limited(&mut self, message: &str) {
        let now = Instant::now();
        let should_log = self
            .last_error_log
            .map(|last| now.duration_since(last) >= Duration::from_secs(2))
            .unwrap_or(true);
        if should_log {
            self.last_error_log = Some(now);
            eprintln!("output-helper: videotoolbox {message}");
        }
    }

    fn log_debug(&self, message: &str) {
        if vt_verbose_enabled() {
            eprintln!("output-helper: videotoolbox {message}");
        }
    }

    unsafe fn create_sample_block_buffer(
        &self,
        sample_payload: &[u8],
    ) -> anyhow::Result<*mut std::ffi::c_void> {
        let packet_len = sample_payload.len();
        let verbose = vt_verbose_enabled();
        if verbose {
            self.log_debug(&format!(
                "about to create CMBlockBuffer with packet_len={packet_len}"
            ));
        }
        let mut block: *mut std::ffi::c_void = std::ptr::null_mut();
        let create_status = CMBlockBufferCreateWithMemoryBlock(
            kCFAllocatorDefault,
            std::ptr::null_mut(),
            packet_len,
            kCFAllocatorDefault,
            std::ptr::null(),
            0,
            packet_len,
            0,
            &mut block,
        );
        if verbose {
            self.log_debug(&format!(
                "CMBlockBufferCreateWithMemoryBlock status={create_status} block_ok={}",
                !block.is_null()
            ));
        }
        if create_status != 0 || block.is_null() {
            bail!("CMBlockBufferCreateWithMemoryBlock failed status={create_status}");
        }

        let replace_status = CMBlockBufferReplaceDataBytes(
            sample_payload.as_ptr() as *const std::ffi::c_void,
            block,
            0,
            packet_len,
        );
        if verbose {
            self.log_debug(&format!(
                "CMBlockBufferReplaceDataBytes status={replace_status} packet_len={packet_len}"
            ));
        }
        if replace_status != 0 {
            CFRelease(block as CFTypeRef);
            bail!("CMBlockBufferReplaceDataBytes failed status={replace_status}");
        }

        Ok(block)
    }

    fn log_hardware_usage(&self, session: *mut std::ffi::c_void) {
        unsafe {
            let mut property_value: *mut std::ffi::c_void = std::ptr::null_mut();
            let status = VTSessionCopyProperty(
                session,
                kVTDecompressionPropertyKey_UsingHardwareAcceleratedVideoDecoder,
                std::ptr::null(),
                &mut property_value,
            );
            if status != 0 {
                self.log_debug(&format!(
                    "session hardware usage query failed status={status}"
                ));
                return;
            }
            if property_value.is_null() {
                self.log_debug("session hardware usage query returned null");
                return;
            }
            let using_hw = CFBooleanGetValue(property_value as *const std::ffi::c_void);
            self.log_debug(&format!("session hardware usage using_hw={using_hw}"));
            CFRelease(property_value as CFTypeRef);
        }
    }

    fn config_parameter_sets_summary(&self) -> Option<String> {
        let config = self.config.as_ref()?;
        let sizes = config
            .parameter_sets
            .iter()
            .map(|set| set.len().to_string())
            .collect::<Vec<_>>()
            .join(",");
        Some(format!(
            "nal_unit_header_length={} parameter_sets={} sizes=[{}]",
            config.nal_length_size,
            config.parameter_sets.len(),
            sizes
        ))
    }

    fn ensure_session(&mut self, packet: &[u8]) -> anyhow::Result<bool> {
        if self.session.is_some() {
            return Ok(true);
        }
        let mut parameter_sets = Vec::new();
        let mut nal_length_size = 4_usize;

        if let Some(config) = &self.config {
            parameter_sets = config.parameter_sets.clone();
            self.log_debug(&format!(
                "config available nal_unit_header_length={} parameter_sets={} sizes=[{}]",
                config.nal_length_size,
                parameter_sets.len(),
                parameter_sets
                    .iter()
                    .map(|set| set.len().to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            ));
        }

        if parameter_sets.is_empty() {
            let Some((packet_parameter_sets, packet_nal_length_size)) =
                extract_annexb_parameter_sets(packet)
            else {
                self.log_debug("waiting for H.264 config before creating session");
                return Ok(false);
            };
            if packet_parameter_sets.is_empty() {
                self.log_debug("packet did not include SPS/PPS for session creation");
                return Ok(false);
            }
            self.log_debug(&format!(
                "using packet parameter sets fallback nal_unit_header_length={packet_nal_length_size} parameter_sets={} sizes=[{}]",
                packet_parameter_sets.len(),
                packet_parameter_sets
                    .iter()
                    .map(|set| set.len().to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            ));
            parameter_sets = packet_parameter_sets;
            nal_length_size = 4;
        }

        let mut parameter_ptrs: Vec<*const u8> = Vec::with_capacity(parameter_sets.len());
        let mut parameter_sizes: Vec<usize> = Vec::with_capacity(parameter_sets.len());
        for set in &parameter_sets {
            parameter_ptrs.push(set.as_ptr());
            parameter_sizes.push(set.len());
        }

        let mut format_description: *mut std::ffi::c_void = std::ptr::null_mut();
        let status = unsafe {
            CMVideoFormatDescriptionCreateFromH264ParameterSets(
                std::ptr::null(),
                parameter_sets.len(),
                parameter_ptrs.as_ptr(),
                parameter_sizes.as_ptr(),
                nal_length_size as i32,
                &mut format_description,
            )
        };
        self.log_debug(&format!(
            "CMVideoFormatDescriptionCreateFromH264ParameterSets status={status} nal_unit_header_length={nal_length_size} parameter_sets={}",
            parameter_sets.len()
        ));
        if status != 0 || format_description.is_null() {
            self.log_error_rate_limited("failed to create H.264 format description");
            return Ok(false);
        }

        let output_attrs = unsafe {
            let dict = ns_mutable_dictionary();
            ns_dictionary_set(
                dict,
                kCVPixelBufferPixelFormatTypeKey,
                ns_u32(KCV_PIXELFORMAT_32_BGRA),
            );
            ns_dictionary_set(dict, kCVPixelBufferMetalCompatibilityKey, ns_bool(true));
            let empty_iosurface: id = msg_send![class!(NSMutableDictionary), dictionary];
            ns_dictionary_set(dict, kCVPixelBufferIOSurfacePropertiesKey, empty_iosurface);
            dict
        };

        let decoder_spec = unsafe {
            let dict = ns_mutable_dictionary();
            ns_dictionary_set(
                dict,
                kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder,
                ns_bool(self.hardware_requested),
            );
            ns_dictionary_set(
                dict,
                kVTVideoDecoderSpecification_EnableHardwareAcceleratedVideoDecoder,
                ns_bool(self.hardware_requested),
            );
            dict
        };

        self.callback_record = Some(Box::new(VTDecompressionOutputCallbackRecord {
            decompression_output_callback: Some(video_toolbox_output_callback),
            decompression_output_ref_con: std::ptr::null_mut(),
        }));

        let mut session: *mut std::ffi::c_void = std::ptr::null_mut();
        let status = unsafe {
            VTDecompressionSessionCreate(
                std::ptr::null(),
                format_description,
                decoder_spec as *const std::ffi::c_void,
                output_attrs as *const std::ffi::c_void,
                self.callback_record
                    .as_ref()
                    .map(|record| &**record as *const _ as *const std::ffi::c_void)
                    .unwrap_or(std::ptr::null()),
                &mut session,
            )
        };
        self.log_debug(&format!(
            "VTDecompressionSessionCreate status={status} session_is_null={}",
            session.is_null()
        ));
        if status != 0 || session.is_null() {
            self.log_error_rate_limited("failed to create decompression session");
            unsafe {
                CFRelease(format_description as CFTypeRef);
            }
            return Ok(false);
        }

        self.session = Some(session);
        self.format_description = Some(format_description);
        self.frame_width = 0;
        self.frame_height = 0;
        self.log_debug(&format!(
            "session initialized hardware_requested={} config={}",
            self.hardware_requested,
            self.config_parameter_sets_summary()
                .unwrap_or_else(|| "none".to_string())
        ));
        self.log_hardware_usage(session);
        Ok(true)
    }

    fn decode_sample(
        &mut self,
        packet: &[u8],
        keyframe: bool,
    ) -> anyhow::Result<Option<CvPixelBufferHandle>> {
        let Some(session) = self.session else {
            return Ok(None);
        };
        let sample_payload =
            vt_prepare_sample_payload(packet).context("vt_prepare_sample_payload failed")?;
        let packet_len = sample_payload.len();
        if packet_len == 0 {
            return Ok(None);
        }
        self.chunk_index = self.chunk_index.saturating_add(1);
        let nal_types = h264_nal_types(&sample_payload, Some(4));
        let keyframe_by_nal = nal_types
            .iter()
            .any(|nal| *nal == 5 || *nal == 7 || *nal == 8);
        if self.verbose && vt_verbose_enabled() {
            self.log_debug(&format!(
                "decode chunk={} size={} nal_types={:?} keyframe_flag={} keyframe_by_nal={} sample_format=avcc4",
                self.chunk_index,
                packet_len,
                nal_types,
                keyframe,
                keyframe_by_nal
            ));
            self.log_debug(&format!(
                "decode chunk={} sample_head={}",
                self.chunk_index,
                hex_dump_prefix(&sample_payload, VT_STALL_HEX_DUMP_BYTES)
            ));
        }
        let block = unsafe { self.create_sample_block_buffer(&sample_payload)? };
        let sample_buffer = unsafe {
            let mut sample_buffer: *mut std::ffi::c_void = std::ptr::null_mut();
            let timing = CMSampleTimingInfo {
                duration: CMTime {
                    value: 0,
                    timescale: 1,
                    flags: 0,
                    epoch: 0,
                },
                presentation_time_stamp: CMTime {
                    value: 0,
                    timescale: 1,
                    flags: 0,
                    epoch: 0,
                },
                decode_time_stamp: CMTime {
                    value: 0,
                    timescale: 1,
                    flags: 0,
                    epoch: 0,
                },
            };
            let verbose = vt_verbose_enabled();
            if verbose {
                self.log_debug("about to create CMSampleBuffer");
            }
            let status = CMSampleBufferCreateReady(
                std::ptr::null(),
                block,
                self.format_description
                    .context("video toolbox format description missing")?,
                1,
                1,
                &timing as *const _ as *const std::ffi::c_void,
                1,
                &packet_len as *const usize,
                &mut sample_buffer,
            );
            if verbose {
                self.log_debug(&format!(
                    "CMSampleBufferCreateReady status={status} sample_buffer_ok={}",
                    !sample_buffer.is_null()
                ));
            }
            if status != 0 || sample_buffer.is_null() {
                bail!("CMSampleBufferCreateReady failed status={status}");
            }
            sample_buffer
        };

        let mut output_state = VideoToolboxOutputState {
            pixel_buffer: None,
            status: 0,
            image_buffer_was_null: false,
        };
        let mut info_flags = 0_u32;
        let verbose = vt_verbose_enabled();
        if verbose {
            self.log_debug("about to call VTDecompressionSessionDecodeFrame");
        }
        let status = unsafe {
            VTDecompressionSessionDecodeFrame(
                session,
                sample_buffer,
                0,
                &mut output_state as *mut _ as *mut std::ffi::c_void,
                &mut info_flags,
            )
        };
        if verbose {
            self.log_debug(&format!(
                "VTDecompressionSessionDecodeFrame status={status} info_flags=0x{info_flags:08x}"
            ));
        }
        if status != 0 {
            unsafe {
                CFRelease(sample_buffer as CFTypeRef);
                CFRelease(block as CFTypeRef);
            }
            return Err(anyhow!(
                "VTDecompressionSessionDecodeFrame failed status={status}"
            ));
        }
        let status = unsafe { VTDecompressionSessionWaitForAsynchronousFrames(session) };
        if verbose {
            self.log_debug(&format!(
                "VTDecompressionSessionWaitForAsynchronousFrames status={status}"
            ));
        }
        if status != 0 {
            unsafe {
                CFRelease(sample_buffer as CFTypeRef);
                CFRelease(block as CFTypeRef);
            }
            return Err(anyhow!(
                "VTDecompressionSessionWaitForAsynchronousFrames failed status={status}"
            ));
        }
        if verbose {
            self.log_debug(&format!(
                "decode callback status={} image_buffer_null={} pixel_buffer_present={}",
                output_state.status,
                output_state.image_buffer_was_null,
                output_state.pixel_buffer.is_some()
            ));
        }
        if output_state.status != 0 {
            unsafe {
                CFRelease(sample_buffer as CFTypeRef);
                CFRelease(block as CFTypeRef);
            }
            return Err(anyhow!(
                "VT output callback failed status={}",
                output_state.status
            ));
        }
        let pixel_buffer = output_state.pixel_buffer;
        unsafe {
            CFRelease(sample_buffer as CFTypeRef);
            CFRelease(block as CFTypeRef);
        }
        Ok(pixel_buffer)
    }
}

#[cfg(target_os = "macos")]
impl VideoDecoder for VideoToolboxDecoder {
    fn backend_kind(&self) -> DecodeBackendKind {
        DecodeBackendKind::VideoToolbox
    }

    fn set_expected_size(&mut self, width: usize, height: usize) {
        self.expected_width = width;
        self.expected_height = height;
    }

    fn decode_into(&mut self, packet: &[u8], frame: &mut DecodedFrame) -> Option<DecodeTimings> {
        let decode_started = Instant::now();
        let nal_length_size = self.config.as_ref().map(|config| config.nal_length_size);
        let nal_types = h264_nal_types(packet, nal_length_size);
        let payload_has_idr = nal_types.iter().any(|nal| *nal == 5);
        let payload_has_parameter_sets = nal_types.iter().any(|nal| *nal == 7 || *nal == 8);
        let keyframe_hint = payload_has_idr || payload_has_parameter_sets;
        if self.verbose && vt_verbose_enabled() {
            self.log_debug(&format!(
                "decode input size={} nal_types={:?} keyframe_hint={} idr={} parameter_sets={}",
                packet.len(),
                nal_types,
                keyframe_hint,
                payload_has_idr,
                payload_has_parameter_sets
            ));
        }
        if !self.ensure_session(packet).ok()? {
            return None;
        }

        let convert_started = Instant::now();
        let pixel_buffer = match self.decode_sample(packet, keyframe_hint) {
            Ok(Some(buffer)) => buffer,
            Ok(None) => return None,
            Err(err) => {
                self.log_error_rate_limited(&format!("{err}"));
                return None;
            }
        };
        let decode_elapsed = decode_started.elapsed();
        let width = unsafe { CVPixelBufferGetWidth(pixel_buffer.as_raw()) };
        let height = unsafe { CVPixelBufferGetHeight(pixel_buffer.as_raw()) };
        let pixel_format = unsafe { CVPixelBufferGetPixelFormatType(pixel_buffer.as_raw()) };
        let plane_count = unsafe { CVPixelBufferGetPlaneCount(pixel_buffer.as_raw()) };
        let iosurface_present =
            unsafe { !CVPixelBufferGetIOSurface(pixel_buffer.as_raw()).is_null() };
        if !iosurface_present {
            self.log_error_rate_limited("pixel buffer is not IOSurface-backed");
        }
        if self.verbose && vt_verbose_enabled() {
            self.log_debug(&format!(
                "decoded pixel buffer width={} height={} pixel_format=0x{pixel_format:08x} plane_count={} iosurface_present={iosurface_present}",
                width,
                height,
                plane_count
            ));
        }
        frame.width = width;
        frame.height = height;
        frame.crop_applied = false;
        frame.bgra.clear();
        frame.cv_pixel_buffer = Some(pixel_buffer);
        let convert_elapsed = convert_started.elapsed();
        Some(DecodeTimings {
            decode: decode_elapsed,
            convert: convert_elapsed,
            mf_process_output: Duration::ZERO,
            cpu_color_convert: Duration::ZERO,
        })
    }
}

#[cfg(target_os = "macos")]
impl Drop for VideoToolboxDecoder {
    fn drop(&mut self) {
        if let Some(session) = self.session.take() {
            unsafe {
                VTDecompressionSessionInvalidate(session);
            }
        }
        if let Some(format_description) = self.format_description.take() {
            unsafe {
                CFRelease(format_description as CFTypeRef);
            }
        }
    }
}

#[cfg(target_os = "windows")]
struct D3d11VideoProcessorState {
    device: ID3D11Device,
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
    enumerator: Option<ID3D11VideoProcessorEnumerator>,
    processor: Option<ID3D11VideoProcessor>,
    output_texture: Option<ID3D11Texture2D>,
    output_view: Option<ID3D11VideoProcessorOutputView>,
    input_width: u32,
    input_height: u32,
    output_width: u32,
    output_height: u32,
}

#[cfg(target_os = "windows")]
impl D3d11VideoProcessorState {
    fn new(
        device: ID3D11Device,
        video_device: ID3D11VideoDevice,
        video_context: ID3D11VideoContext,
    ) -> Self {
        Self {
            device,
            video_device,
            video_context,
            enumerator: None,
            processor: None,
            output_texture: None,
            output_view: None,
            input_width: 0,
            input_height: 0,
            output_width: 0,
            output_height: 0,
        }
    }

    fn convert(
        &mut self,
        input_texture: &ID3D11Texture2D,
        input_subresource: u32,
        input_width: u32,
        input_height: u32,
        source_crop_width: u32,
        source_crop_height: u32,
        output_width: u32,
        output_height: u32,
    ) -> anyhow::Result<*mut std::ffi::c_void> {
        self.ensure_resources(input_width, input_height, output_width, output_height)?;

        let enumerator = self
            .enumerator
            .as_ref()
            .context("video processor enumerator missing")?;
        let processor = self.processor.as_ref().context("video processor missing")?;
        let output_view = self
            .output_view
            .as_ref()
            .context("video processor output view missing")?;

        let mut input_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: Default::default(),
        };
        let input_mip_levels = texture_mip_levels(input_texture);
        input_desc.Anonymous.Texture2D = D3D11_TEX2D_VPIV {
            MipSlice: input_subresource % input_mip_levels,
            ArraySlice: input_subresource / input_mip_levels,
        };
        let mut input_view = None;
        unsafe {
            self.video_device
                .CreateVideoProcessorInputView(
                    input_texture,
                    enumerator,
                    &input_desc,
                    Some(&mut input_view),
                )
                .context("CreateVideoProcessorInputView failed")?;
        }
        let input_view = input_view.context("video processor input view missing")?;

        let src_rect = RECT {
            left: 0,
            top: 0,
            right: source_crop_width.min(input_width) as i32,
            bottom: source_crop_height.min(input_height) as i32,
        };
        let dst_rect = RECT {
            left: 0,
            top: 0,
            right: output_width as i32,
            bottom: output_height as i32,
        };
        unsafe {
            self.video_context.VideoProcessorSetStreamSourceRect(
                processor,
                0,
                BOOL(1),
                Some(&src_rect),
            );
            self.video_context.VideoProcessorSetStreamDestRect(
                processor,
                0,
                BOOL(1),
                Some(&dst_rect),
            );
            self.video_context.VideoProcessorSetOutputTargetRect(
                processor,
                BOOL(1),
                Some(&dst_rect),
            );
        }

        let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: BOOL(1),
            OutputIndex: 0,
            InputFrameOrField: 0,
            PastFrames: 0,
            FutureFrames: 0,
            ppPastSurfaces: std::ptr::null_mut(),
            pInputSurface: std::mem::ManuallyDrop::new(Some(input_view)),
            ppFutureSurfaces: std::ptr::null_mut(),
            ppPastSurfacesRight: std::ptr::null_mut(),
            pInputSurfaceRight: std::mem::ManuallyDrop::new(None),
            ppFutureSurfacesRight: std::ptr::null_mut(),
        };
        unsafe {
            self.video_context
                .VideoProcessorBlt(processor, output_view, 0, std::slice::from_mut(&mut stream))
                .context("VideoProcessorBlt failed")?;
            let _ = std::mem::ManuallyDrop::take(&mut stream.pInputSurface);
            let _ = std::mem::ManuallyDrop::take(&mut stream.pInputSurfaceRight);
        }

        let output_texture = self
            .output_texture
            .as_ref()
            .context("output texture missing")?;
        Ok(output_texture.as_raw() as *mut std::ffi::c_void)
    }

    fn ensure_resources(
        &mut self,
        input_width: u32,
        input_height: u32,
        output_width: u32,
        output_height: u32,
    ) -> anyhow::Result<()> {
        if self.enumerator.is_some()
            && self.input_width == input_width
            && self.input_height == input_height
            && self.output_width == output_width
            && self.output_height == output_height
        {
            return Ok(());
        }

        let content_desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: DXGI_RATIONAL {
                Numerator: 60,
                Denominator: 1,
            },
            InputWidth: input_width,
            InputHeight: input_height,
            OutputFrameRate: DXGI_RATIONAL {
                Numerator: 60,
                Denominator: 1,
            },
            OutputWidth: output_width,
            OutputHeight: output_height,
            Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
        };
        let enumerator = unsafe {
            self.video_device
                .CreateVideoProcessorEnumerator(&content_desc)
                .context("CreateVideoProcessorEnumerator failed")?
        };
        let processor = unsafe {
            self.video_device
                .CreateVideoProcessor(&enumerator, 0)
                .context("CreateVideoProcessor failed")?
        };
        let texture_desc = D3D11_TEXTURE2D_DESC {
            Width: output_width,
            Height: output_height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut output_texture = None;
        unsafe {
            self.device
                .CreateTexture2D(&texture_desc, None, Some(&mut output_texture))
                .context("CreateTexture2D(video processor output) failed")?;
        }
        let output_texture = output_texture.context("video processor output texture missing")?;

        let mut output_view_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            Anonymous: Default::default(),
        };
        output_view_desc.Anonymous.Texture2D = D3D11_TEX2D_VPOV { MipSlice: 0 };
        let mut output_view = None;
        unsafe {
            self.video_device
                .CreateVideoProcessorOutputView(
                    &output_texture,
                    &enumerator,
                    &output_view_desc,
                    Some(&mut output_view),
                )
                .context("CreateVideoProcessorOutputView failed")?;
        }

        self.enumerator = Some(enumerator);
        self.processor = Some(processor);
        self.output_texture = Some(output_texture);
        self.output_view = output_view;
        self.input_width = input_width;
        self.input_height = input_height;
        self.output_width = output_width;
        self.output_height = output_height;
        Ok(())
    }
}

#[cfg(target_os = "windows")]
struct MfD3d11Decoder {
    _device: ID3D11Device,
    _context: ID3D11DeviceContext,
    transform: IMFTransform,
    _device_manager: IMFDXGIDeviceManager,
    video_processor: D3d11VideoProcessorState,
    output_subtype: windows::core::GUID,
    frame_width: usize,
    frame_height: usize,
    expected_width: usize,
    expected_height: usize,
    output_requires_sample: bool,
    output_sample_size: u32,
    started: bool,
    last_error_log: Option<Instant>,
    last_texture_fallback_log: Option<Instant>,
    texture_fallbacks_since_log: u64,
    last_mf_process_output_elapsed: Duration,
    last_cpu_color_convert_elapsed: Duration,
    texture_fastpath_enabled: bool,
}

#[cfg(target_os = "windows")]
impl MfD3d11Decoder {
    fn new(
        preferred_device: Option<*mut std::ffi::c_void>,
        texture_fastpath_enabled: bool,
    ) -> anyhow::Result<Self> {
        unsafe {
            let init_hr = CoInitializeEx(None, COINIT_MULTITHREADED);
            if init_hr.is_err() && init_hr.0 != 0x80010106u32 as i32 {
                init_hr.ok().context("CoInitializeEx failed")?;
            }
        }
        acquire_mf_runtime()?;

        let init_result = (|| -> anyhow::Result<Self> {
            let (device, context) =
                create_d3d11_device(preferred_device).context("failed to create D3D11 device")?;
            let video_device: ID3D11VideoDevice =
                device.cast().context("ID3D11VideoDevice cast failed")?;
            let video_context: ID3D11VideoContext =
                context.cast().context("ID3D11VideoContext cast failed")?;
            let (device_manager, reset_token) =
                create_dxgi_device_manager().context("failed to create DXGI device manager")?;
            unsafe {
                device_manager
                    .ResetDevice(&device, reset_token)
                    .context("ResetDevice failed")?;
            }
            let transform: IMFTransform =
                unsafe { CoCreateInstance(&CLSID_MSH264DecoderMFT, None, CLSCTX_INPROC_SERVER) }
                    .context("failed to create H.264 MFT")?;

            unsafe {
                let manager_unknown: IUnknown = device_manager
                    .clone()
                    .cast()
                    .context("device manager cast failed")?;
                transform
                    .ProcessMessage(
                        MFT_MESSAGE_SET_D3D_MANAGER,
                        manager_unknown.as_raw() as usize,
                    )
                    .context("ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER) failed")?;
            }

            match enable_mf_low_latency(&transform) {
                Ok(true) => eprintln!("output-helper: mf-d3d11 low-latency mode enabled"),
                Ok(false) => eprintln!(
                    "output-helper: mf-d3d11 low-latency mode unsupported on this decoder"
                ),
                Err(err) => {
                    eprintln!("output-helper: mf-d3d11 low-latency mode setup failed: {err}")
                }
            }

            set_decoder_input_type(&transform)?;
            let (output_subtype, frame_width, frame_height) = set_decoder_output_type(&transform)?;
            let (output_requires_sample, output_sample_size) =
                query_output_stream_info(&transform).context("GetOutputStreamInfo failed")?;
            eprintln!(
                "output-helper: mf-d3d11 output subtype={} size={}x{} requires_sample={} sample_size={}",
                mf_subtype_name(output_subtype),
                frame_width,
                frame_height,
                output_requires_sample,
                output_sample_size
            );

            Ok(Self {
                video_processor: D3d11VideoProcessorState::new(
                    device.clone(),
                    video_device,
                    video_context,
                ),
                _device: device,
                _context: context,
                transform,
                _device_manager: device_manager,
                output_subtype,
                frame_width,
                frame_height,
                expected_width: 0,
                expected_height: 0,
                output_requires_sample,
                output_sample_size,
                started: false,
                last_error_log: None,
                last_texture_fallback_log: None,
                texture_fallbacks_since_log: 0,
                last_mf_process_output_elapsed: Duration::ZERO,
                last_cpu_color_convert_elapsed: Duration::ZERO,
                texture_fastpath_enabled,
            })
        })();
        if init_result.is_err() {
            release_mf_runtime();
        }
        init_result
    }

    fn log_error_rate_limited(&mut self, message: &str) {
        let now = Instant::now();
        let can_log = match self.last_error_log {
            Some(last) => now.duration_since(last) >= Duration::from_secs(2),
            None => true,
        };
        if can_log {
            self.last_error_log = Some(now);
            eprintln!("output-helper: mf-d3d11 {message}");
        }
    }

    fn log_texture_fallback_rate_limited(&mut self, reason: &str) {
        self.texture_fallbacks_since_log = self.texture_fallbacks_since_log.saturating_add(1);
        let now = Instant::now();
        let can_log = match self.last_texture_fallback_log {
            Some(last) => now.duration_since(last) >= Duration::from_secs(2),
            None => true,
        };
        if can_log {
            eprintln!(
                "output-helper: mf-d3d11 texture path fallback count={} reason={}",
                self.texture_fallbacks_since_log, reason
            );
            self.texture_fallbacks_since_log = 0;
            self.last_texture_fallback_log = Some(now);
        }
    }

    fn ensure_streaming_started(&mut self) -> anyhow::Result<()> {
        if self.started {
            return Ok(());
        }
        unsafe {
            self.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .context("MFT_MESSAGE_NOTIFY_BEGIN_STREAMING failed")?;
            self.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                .context("MFT_MESSAGE_NOTIFY_START_OF_STREAM failed")?;
        }
        self.started = true;
        Ok(())
    }

    fn process_output(&mut self, frame: &mut DecodedFrame) -> anyhow::Result<bool> {
        self.last_mf_process_output_elapsed = Duration::ZERO;
        self.last_cpu_color_convert_elapsed = Duration::ZERO;
        let mut output_sample: Option<IMFSample> = None;
        let mut stream_change_count = 0_u32;
        loop {
            let mut output_buffer = MFT_OUTPUT_DATA_BUFFER::default();
            output_buffer.dwStreamID = 0;
            if self.output_requires_sample {
                let sample = unsafe { MFCreateSample().context("MFCreateSample(output) failed")? };
                let buffer_len = self.output_sample_size.max(1);
                let buffer = unsafe {
                    MFCreateMemoryBuffer(buffer_len)
                        .context("MFCreateMemoryBuffer(output) failed")?
                };
                unsafe {
                    sample
                        .AddBuffer(&buffer)
                        .context("IMFSample::AddBuffer(output) failed")?;
                }
                output_buffer.pSample = std::mem::ManuallyDrop::new(Some(sample));
            }

            let mut process_status = 0_u32;
            let process_started = Instant::now();
            let output_result = unsafe {
                self.transform.ProcessOutput(
                    0,
                    std::slice::from_mut(&mut output_buffer),
                    &mut process_status,
                )
            };
            self.last_mf_process_output_elapsed = process_started.elapsed();
            match output_result {
                Ok(()) => {
                    let sample_opt =
                        unsafe { std::mem::ManuallyDrop::take(&mut output_buffer.pSample) };
                    if let Some(sample) = sample_opt {
                        output_sample = Some(sample);
                    }
                    let _ = unsafe { std::mem::ManuallyDrop::take(&mut output_buffer.pEvents) };
                    break;
                }
                Err(err) => {
                    let code = err.code();
                    if code == MF_E_TRANSFORM_NEED_MORE_INPUT {
                        return Ok(false);
                    }
                    if code == MF_E_TRANSFORM_STREAM_CHANGE && stream_change_count < 2 {
                        stream_change_count = stream_change_count.saturating_add(1);
                        let (subtype, width, height) = set_decoder_output_type(&self.transform)?;
                        self.output_subtype = subtype;
                        self.frame_width = width;
                        self.frame_height = height;
                        eprintln!(
                            "output-helper: mf-d3d11 stream change subtype={} size={}x{}",
                            mf_subtype_name(subtype),
                            width,
                            height
                        );
                        if let Ok((requires_sample, sample_size)) =
                            query_output_stream_info(&self.transform)
                        {
                            self.output_requires_sample = requires_sample;
                            self.output_sample_size = sample_size;
                        }
                        continue;
                    }
                    return Err(anyhow!("MFT ProcessOutput failed: {err}"));
                }
            }
        }

        let Some(sample) = output_sample else {
            return Ok(false);
        };
        if self.texture_fastpath_enabled {
            match self.try_process_output_texture(&sample, frame) {
                Ok(true) => {
                    self.last_cpu_color_convert_elapsed = Duration::ZERO;
                    return Ok(true);
                }
                Ok(false) => {}
                Err(err) => {
                    self.log_texture_fallback_rate_limited(&err.to_string());
                }
            }
        }
        let mut cpu_color_convert_elapsed = Duration::ZERO;
        let primary_buffer = unsafe { sample.GetBufferByIndex(0).ok() };
        if let Some(buffer) = primary_buffer {
            let primary_started = Instant::now();
            match self.copy_output_buffer_to_frame(&buffer, frame) {
                Ok(()) => {
                    cpu_color_convert_elapsed =
                        cpu_color_convert_elapsed.saturating_add(primary_started.elapsed());
                    self.last_cpu_color_convert_elapsed = cpu_color_convert_elapsed;
                    return Ok(true);
                }
                Err(primary_err) => {
                    cpu_color_convert_elapsed =
                        cpu_color_convert_elapsed.saturating_add(primary_started.elapsed());
                    let contiguous = unsafe {
                        sample
                            .ConvertToContiguousBuffer()
                            .context("ConvertToContiguousBuffer fallback failed")?
                    };
                    let contiguous_started = Instant::now();
                    match self.copy_output_buffer_to_frame(&contiguous, frame) {
                        Ok(()) => {
                            cpu_color_convert_elapsed = cpu_color_convert_elapsed
                                .saturating_add(contiguous_started.elapsed());
                            self.last_cpu_color_convert_elapsed = cpu_color_convert_elapsed;
                            return Ok(true);
                        }
                        Err(contiguous_err) => {
                            cpu_color_convert_elapsed = cpu_color_convert_elapsed
                                .saturating_add(contiguous_started.elapsed());
                            let copied = self.copy_sample_to_memory_buffer(&sample)?;
                            let copied_started = Instant::now();
                            self.copy_output_buffer_to_frame(&copied, frame).map_err(
                                |copied_err| {
                                    anyhow!(
                                        "failed to read output buffer: primary={primary_err}; contiguous={contiguous_err}; copied={copied_err}"
                                    )
                                },
                            )?;
                            cpu_color_convert_elapsed =
                                cpu_color_convert_elapsed.saturating_add(copied_started.elapsed());
                            self.last_cpu_color_convert_elapsed = cpu_color_convert_elapsed;
                            return Ok(true);
                        }
                    }
                }
            }
        }

        let contiguous = unsafe {
            sample
                .ConvertToContiguousBuffer()
                .context("ConvertToContiguousBuffer failed")?
        };
        let contiguous_started = Instant::now();
        self.copy_output_buffer_to_frame(&contiguous, frame)?;
        cpu_color_convert_elapsed =
            cpu_color_convert_elapsed.saturating_add(contiguous_started.elapsed());
        self.last_cpu_color_convert_elapsed = cpu_color_convert_elapsed;
        Ok(true)
    }

    fn copy_sample_to_memory_buffer(&self, sample: &IMFSample) -> anyhow::Result<IMFMediaBuffer> {
        let total_length = unsafe {
            sample
                .GetTotalLength()
                .context("IMFSample::GetTotalLength failed")?
        };
        let buffer_len = total_length.max(1);
        let buffer = unsafe {
            MFCreateMemoryBuffer(buffer_len).context("MFCreateMemoryBuffer(copy) failed")?
        };
        unsafe {
            sample
                .CopyToBuffer(&buffer)
                .context("IMFSample::CopyToBuffer failed")?;
            buffer
                .SetCurrentLength(total_length)
                .context("IMFMediaBuffer::SetCurrentLength(copy) failed")?;
        }
        Ok(buffer)
    }

    fn try_process_output_texture(
        &mut self,
        sample: &IMFSample,
        frame: &mut DecodedFrame,
    ) -> anyhow::Result<bool> {
        let Some(primary_buffer) = (unsafe { sample.GetBufferByIndex(0).ok() }) else {
            return Ok(false);
        };
        let Some((input_texture, input_subresource)) = extract_dx11_texture(&primary_buffer)?
        else {
            return Ok(false);
        };

        let input_desc = texture_desc(&input_texture);
        let input_width = input_desc.Width;
        let input_height = input_desc.Height;
        let output_size = choose_effective_output_size(
            input_width as usize,
            input_height as usize,
            self.expected_width,
            self.expected_height,
        );
        let output_width = output_size.width as u32;
        let output_height = output_size.height as u32;
        let texture_ptr = self.video_processor.convert(
            &input_texture,
            input_subresource,
            input_width,
            input_height,
            output_width,
            output_height,
            output_width,
            output_height,
        )?;
        frame.width = output_width as usize;
        frame.height = output_height as usize;
        frame.crop_applied = output_size.crop_applied;
        frame.bgra.clear();
        frame.dx11_texture = Some(texture_ptr);
        frame.dx11_device = Some(self._device.as_raw() as *mut std::ffi::c_void);
        Ok(true)
    }

    fn copy_output_buffer_to_frame(
        &mut self,
        media_buffer: &IMFMediaBuffer,
        frame: &mut DecodedFrame,
    ) -> anyhow::Result<()> {
        if self.output_subtype == MFVideoFormat_ARGB32 || self.output_subtype == MFVideoFormat_RGB32
        {
            self.read_argb_buffer(media_buffer, frame)
        } else if self.output_subtype == MFVideoFormat_NV12
            || self.output_subtype == MFVideoFormat_YUY2
        {
            self.read_yuv_buffer_to_bgra(media_buffer, frame)
        } else {
            Err(anyhow!(
                "unsupported output subtype {:?}",
                self.output_subtype
            ))
        }
    }

    fn read_argb_buffer(
        &mut self,
        media_buffer: &IMFMediaBuffer,
        frame: &mut DecodedFrame,
    ) -> anyhow::Result<()> {
        let mut source_width = self.frame_width;
        let mut source_height = self.frame_height;
        if source_width == 0 || source_height == 0 {
            source_width = self.expected_width;
            source_height = self.expected_height;
        }
        if source_width == 0 || source_height == 0 {
            bail!("invalid ARGB frame size {}x{}", source_width, source_height);
        }
        if (self.frame_width == 0 || self.frame_height == 0)
            && source_width > 0
            && source_height > 0
        {
            self.frame_width = source_width;
            self.frame_height = source_height;
        }
        let output_size = choose_effective_output_size(
            source_width,
            source_height,
            self.expected_width,
            self.expected_height,
        );
        let output_width = output_size.width;
        let output_height = output_size.height;
        let needed = output_width.saturating_mul(output_height).saturating_mul(4);
        if frame.bgra.len() != needed {
            frame.bgra.resize(needed, 0);
        }

        if output_width == source_width && output_height == source_height {
            copy_argb32_to_bgra(media_buffer, source_width, source_height, &mut frame.bgra)?;
        } else {
            copy_argb32_to_bgra_cropped(
                media_buffer,
                source_width,
                source_height,
                output_width,
                output_height,
                &mut frame.bgra,
            )?;
        }
        frame.width = output_width;
        frame.height = output_height;
        frame.crop_applied = output_size.crop_applied;
        frame.dx11_texture = None;
        frame.dx11_device = None;
        Ok(())
    }

    fn read_yuv_buffer_to_bgra(
        &mut self,
        media_buffer: &IMFMediaBuffer,
        frame: &mut DecodedFrame,
    ) -> anyhow::Result<()> {
        let mut source_width = self.frame_width;
        let mut source_height = self.frame_height;
        if source_width == 0 || source_height == 0 {
            source_width = self.expected_width;
            source_height = self.expected_height;
        }
        if source_width == 0 || source_height == 0 {
            bail!("invalid YUV frame size {}x{}", source_width, source_height);
        }
        if (self.frame_width == 0 || self.frame_height == 0)
            && source_width > 0
            && source_height > 0
        {
            self.frame_width = source_width;
            self.frame_height = source_height;
        }
        let output_size = choose_effective_output_size(
            source_width,
            source_height,
            self.expected_width,
            self.expected_height,
        );
        let output_width = output_size.width;
        let output_height = output_size.height;
        let needed = output_width.saturating_mul(output_height).saturating_mul(4);
        if frame.bgra.len() != needed {
            frame.bgra.resize(needed, 0);
        }

        if self.output_subtype == MFVideoFormat_NV12 {
            if output_width == source_width && output_height == source_height {
                copy_nv12_to_bgra(media_buffer, source_width, source_height, &mut frame.bgra)?;
            } else {
                copy_nv12_to_bgra_cropped(
                    media_buffer,
                    source_width,
                    source_height,
                    output_width,
                    output_height,
                    &mut frame.bgra,
                )?;
            }
        } else if self.output_subtype == MFVideoFormat_YUY2 {
            if output_width == source_width && output_height == source_height {
                copy_yuy2_to_bgra(media_buffer, source_width, source_height, &mut frame.bgra)?;
            } else {
                copy_yuy2_to_bgra_cropped(
                    media_buffer,
                    source_width,
                    source_height,
                    output_width,
                    output_height,
                    &mut frame.bgra,
                )?;
            }
        } else {
            bail!("unsupported YUV subtype {:?}", self.output_subtype);
        }
        frame.width = output_width;
        frame.height = output_height;
        frame.crop_applied = output_size.crop_applied;
        frame.dx11_texture = None;
        frame.dx11_device = None;
        Ok(())
    }
}

#[cfg(target_os = "windows")]
impl VideoDecoder for MfD3d11Decoder {
    fn backend_kind(&self) -> DecodeBackendKind {
        DecodeBackendKind::MfD3d11
    }

    fn flush(&mut self) {
        unsafe {
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
        }
        self.started = false;
    }

    fn set_expected_size(&mut self, width: usize, height: usize) {
        self.expected_width = width;
        self.expected_height = height;
    }

    fn set_texture_fastpath_enabled(&mut self, enabled: bool) {
        self.texture_fastpath_enabled = enabled;
    }

    fn decode_into(&mut self, packet: &[u8], frame: &mut DecodedFrame) -> Option<DecodeTimings> {
        let decode_started = Instant::now();
        if self.ensure_streaming_started().is_err() {
            self.log_error_rate_limited("failed to start streaming state");
            return None;
        }
        let packet_len = u32::try_from(packet.len()).ok()?;
        let media_buffer = match unsafe { MFCreateMemoryBuffer(packet_len) } {
            Ok(value) => value,
            Err(err) => {
                self.log_error_rate_limited(&format!("MFCreateMemoryBuffer failed: {err}"));
                return None;
            }
        };
        let mut dest_ptr = std::ptr::null_mut::<u8>();
        let mut _max_len = 0_u32;
        let mut _cur_len = 0_u32;
        unsafe {
            if let Err(err) =
                media_buffer.Lock(&mut dest_ptr, Some(&mut _max_len), Some(&mut _cur_len))
            {
                self.log_error_rate_limited(&format!("IMFMediaBuffer::Lock failed: {err}"));
                return None;
            }
            std::ptr::copy_nonoverlapping(packet.as_ptr(), dest_ptr, packet.len());
            if let Err(err) = media_buffer.Unlock() {
                self.log_error_rate_limited(&format!("IMFMediaBuffer::Unlock failed: {err}"));
                return None;
            }
            if let Err(err) = media_buffer.SetCurrentLength(packet_len) {
                self.log_error_rate_limited(&format!(
                    "IMFMediaBuffer::SetCurrentLength failed: {err}"
                ));
                return None;
            }
        }
        let sample = match unsafe { MFCreateSample() } {
            Ok(value) => value,
            Err(err) => {
                self.log_error_rate_limited(&format!("MFCreateSample failed: {err}"));
                return None;
            }
        };
        unsafe {
            if let Err(err) = sample.AddBuffer(&media_buffer) {
                self.log_error_rate_limited(&format!("IMFSample::AddBuffer failed: {err}"));
                return None;
            }
        }

        let input_result = unsafe { self.transform.ProcessInput(0, &sample, 0) };
        if let Err(err) = input_result {
            if err.code() == MF_E_NOTACCEPTING {
                let _ = self.process_output(frame);
                let retry = unsafe { self.transform.ProcessInput(0, &sample, 0) };
                if retry.is_err() {
                    self.log_error_rate_limited(&format!(
                        "IMFTransform::ProcessInput retry failed: {}",
                        retry.err().map(|e| e.to_string()).unwrap_or_default()
                    ));
                    return None;
                }
            } else {
                self.log_error_rate_limited(&format!("IMFTransform::ProcessInput failed: {err}"));
                return None;
            }
        }
        let decode_elapsed = decode_started.elapsed();

        let convert_started = Instant::now();
        let got_frame = match self.process_output(frame) {
            Ok(value) => value,
            Err(err) => {
                self.log_error_rate_limited(&format!("process_output failed: {err}"));
                return None;
            }
        };
        if !got_frame {
            return None;
        }
        let convert_elapsed = convert_started.elapsed();
        Some(DecodeTimings {
            decode: decode_elapsed,
            convert: convert_elapsed,
            mf_process_output: self.last_mf_process_output_elapsed,
            cpu_color_convert: self.last_cpu_color_convert_elapsed,
        })
    }
}

#[cfg(target_os = "windows")]
impl Drop for MfD3d11Decoder {
    fn drop(&mut self) {
        unsafe {
            let _ = self
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
        }
        release_mf_runtime();
    }
}

#[cfg(target_os = "windows")]
fn create_d3d11_device(
    preferred_device: Option<*mut std::ffi::c_void>,
) -> anyhow::Result<(ID3D11Device, ID3D11DeviceContext)> {
    if let Some(raw_device) = preferred_device {
        let borrowed = unsafe { ID3D11Device::from_raw(raw_device as *mut _) };
        let device = borrowed.clone();
        std::mem::forget(borrowed);
        let context =
            unsafe { device.GetImmediateContext() }.context("GetImmediateContext failed")?;
        return Ok((device, context));
    }

    let mut device = None;
    let mut context = None;
    let feature_levels: [D3D_FEATURE_LEVEL; 2] = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            None,
            D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            Some(&feature_levels),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
        .context("D3D11CreateDevice failed")?;
    }
    let device = device.context("D3D11 device missing")?;
    let context = context.context("D3D11 context missing")?;
    Ok((device, context))
}

#[cfg(target_os = "windows")]
fn create_dxgi_device_manager() -> anyhow::Result<(IMFDXGIDeviceManager, u32)> {
    let mut reset_token = 0_u32;
    let mut manager = None;
    unsafe {
        MFCreateDXGIDeviceManager(&mut reset_token, &mut manager)
            .context("MFCreateDXGIDeviceManager failed")?;
    }
    let manager = manager.context("DXGI device manager missing")?;
    Ok((manager, reset_token))
}

#[cfg(target_os = "windows")]
fn acquire_mf_runtime() -> anyhow::Result<()> {
    let prev = MF_RUNTIME_REFS.fetch_add(1, Ordering::AcqRel);
    if prev == 0 {
        unsafe {
            MFStartup(0x0002_0070, MFSTARTUP_NOSOCKET).context("MFStartup failed")?;
        }
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn release_mf_runtime() {
    let current = MF_RUNTIME_REFS.load(Ordering::Acquire);
    if current == 0 {
        return;
    }
    if MF_RUNTIME_REFS.fetch_sub(1, Ordering::AcqRel) == 1 {
        unsafe {
            let _ = MFShutdown();
        }
    }
}

#[cfg(target_os = "windows")]
fn enable_mf_low_latency(transform: &IMFTransform) -> anyhow::Result<bool> {
    let codec_api: ICodecAPI = transform.cast().context("ICodecAPI cast failed")?;
    unsafe {
        if codec_api.IsSupported(&CODECAPI_AVLowLatencyMode).is_err() {
            return Ok(false);
        }
        if codec_api.IsModifiable(&CODECAPI_AVLowLatencyMode).is_err() {
            return Ok(false);
        }
        let enabled_bool = windows::core::VARIANT::from(true);
        if codec_api
            .SetValue(&CODECAPI_AVLowLatencyMode, &enabled_bool)
            .is_ok()
        {
            return Ok(true);
        }
        let enabled_i32 = windows::core::VARIANT::from(1_i32);
        if codec_api
            .SetValue(&CODECAPI_AVLowLatencyMode, &enabled_i32)
            .is_ok()
        {
            return Ok(true);
        }
        let enabled_u32 = windows::core::VARIANT::from(1_u32);
        codec_api
            .SetValue(&CODECAPI_AVLowLatencyMode, &enabled_u32)
            .context("SetValue(CODECAPI_AVLowLatencyMode) failed")?;
    }
    Ok(true)
}

#[cfg(target_os = "windows")]
fn set_decoder_input_type(transform: &IMFTransform) -> anyhow::Result<()> {
    let input_type = unsafe { MFCreateMediaType().context("MFCreateMediaType(input) failed")? };
    unsafe {
        input_type
            .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .context("SetGUID(MF_MT_MAJOR_TYPE) failed")?;
        input_type
            .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)
            .context("SetGUID(MF_MT_SUBTYPE=H264) failed")?;
        input_type
            .SetUINT32(
                &MF_MT_INTERLACE_MODE,
                u32::try_from(MFVideoInterlace_Progressive.0).unwrap_or(2),
            )
            .context("SetUINT32(MF_MT_INTERLACE_MODE) failed")?;
        transform
            .SetInputType(0, Some(&input_type), 0)
            .context("SetInputType failed")?;
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn set_decoder_output_type(
    transform: &IMFTransform,
) -> anyhow::Result<(windows::core::GUID, usize, usize)> {
    let preferred = [
        MFVideoFormat_NV12,
        MFVideoFormat_YUY2,
        MFVideoFormat_ARGB32,
        MFVideoFormat_RGB32,
    ];
    for subtype in preferred {
        if let Some((media_type, width, height)) = find_output_type(transform, subtype)? {
            unsafe {
                transform
                    .SetOutputType(0, Some(&media_type), 0)
                    .context("SetOutputType failed")?;
            }
            return Ok((subtype, width as usize, height as usize));
        }
    }
    bail!("no usable output type found for H.264 decoder MFT");
}

#[cfg(target_os = "windows")]
fn mf_subtype_name(guid: windows::core::GUID) -> &'static str {
    if guid == MFVideoFormat_ARGB32 {
        "ARGB32"
    } else if guid == MFVideoFormat_RGB32 {
        "RGB32"
    } else if guid == MFVideoFormat_NV12 {
        "NV12"
    } else if guid == MFVideoFormat_YUY2 {
        "YUY2"
    } else {
        "unknown"
    }
}

#[cfg(target_os = "windows")]
fn find_output_type(
    transform: &IMFTransform,
    target_subtype: windows::core::GUID,
) -> anyhow::Result<Option<(IMFMediaType, u32, u32)>> {
    let mut idx = 0_u32;
    loop {
        let available = unsafe { transform.GetOutputAvailableType(0, idx) };
        let media_type = match available {
            Ok(mt) => mt,
            Err(err) => {
                if err.code() == MF_E_NO_MORE_TYPES {
                    return Ok(None);
                }
                return Err(anyhow!("GetOutputAvailableType failed: {err}"));
            }
        };
        idx = idx.saturating_add(1);
        let subtype = unsafe { media_type.GetGUID(&MF_MT_SUBTYPE) };
        let Ok(subtype) = subtype else {
            continue;
        };
        if subtype != target_subtype {
            continue;
        }
        let (width, height) = frame_size_from_media_type(&media_type);
        return Ok(Some((media_type, width, height)));
    }
}

#[cfg(target_os = "windows")]
fn frame_size_from_media_type(media_type: &IMFMediaType) -> (u32, u32) {
    let frame_size = unsafe { media_type.GetUINT64(&MF_MT_FRAME_SIZE) };
    if let Ok(packed) = frame_size {
        let width = (packed >> 32) as u32;
        let height = packed as u32;
        return (width, height);
    }
    (0, 0)
}

#[cfg(target_os = "windows")]
fn query_output_stream_info(transform: &IMFTransform) -> anyhow::Result<(bool, u32)> {
    let info = unsafe {
        transform
            .GetOutputStreamInfo(0)
            .context("GetOutputStreamInfo failed")?
    };
    let provides_samples = (info.dwFlags as i32 & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0) != 0;
    let requires_sample = !provides_samples;
    Ok((requires_sample, info.cbSize))
}

#[cfg(target_os = "windows")]
fn extract_dx11_texture(
    media_buffer: &IMFMediaBuffer,
) -> anyhow::Result<Option<(ID3D11Texture2D, u32)>> {
    let Ok(dxgi_buffer) = media_buffer.cast::<IMFDXGIBuffer>() else {
        return Ok(None);
    };
    let mut raw = std::ptr::null_mut::<std::ffi::c_void>();
    unsafe {
        dxgi_buffer
            .GetResource(&ID3D11Texture2D::IID, &mut raw)
            .context("IMFDXGIBuffer::GetResource failed")?;
    }
    let texture =
        unsafe { ID3D11Texture2D::from_abi(raw) }.context("ID3D11Texture2D::from_abi failed")?;
    let subresource = unsafe {
        dxgi_buffer
            .GetSubresourceIndex()
            .context("IMFDXGIBuffer::GetSubresourceIndex failed")?
    };
    Ok(Some((texture, subresource)))
}

#[cfg(target_os = "windows")]
fn texture_desc(texture: &ID3D11Texture2D) -> D3D11_TEXTURE2D_DESC {
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    unsafe {
        texture.GetDesc(&mut desc);
    }
    desc
}

#[cfg(target_os = "windows")]
fn texture_mip_levels(texture: &ID3D11Texture2D) -> u32 {
    texture_desc(texture).MipLevels.max(1)
}

#[cfg(target_os = "windows")]
fn copy_argb32_to_bgra(
    media_buffer: &IMFMediaBuffer,
    width: usize,
    height: usize,
    out: &mut [u8],
) -> anyhow::Result<()> {
    let stride = width.saturating_mul(4);
    if let Ok(buffer2d) = media_buffer.cast::<IMF2DBuffer>() {
        let mut line0 = std::ptr::null_mut::<u8>();
        let mut pitch = 0_i32;
        if unsafe { buffer2d.Lock2D(&mut line0, &mut pitch) }.is_ok() {
            let pitch = pitch.unsigned_abs() as usize;
            for y in 0..height {
                let src = unsafe { std::slice::from_raw_parts(line0.add(y * pitch), stride) };
                let dst = &mut out[y * stride..(y + 1) * stride];
                dst.copy_from_slice(src);
            }
            unsafe {
                buffer2d
                    .Unlock2D()
                    .context("IMF2DBuffer::Unlock2D failed")?;
            }
            return Ok(());
        }
        if let Some(bytes) = copy_imf2d_contiguous_bytes(&buffer2d, stride.saturating_mul(height))?
        {
            out[..stride * height].copy_from_slice(&bytes[..stride * height]);
            return Ok(());
        }
    }

    let mut raw_ptr = std::ptr::null_mut::<u8>();
    let mut _max_len = 0_u32;
    let mut cur_len = 0_u32;
    unsafe {
        media_buffer
            .Lock(&mut raw_ptr, Some(&mut _max_len), Some(&mut cur_len))
            .context("IMFMediaBuffer::Lock failed")?;
    }
    let total = usize::try_from(cur_len).unwrap_or(0);
    if total < stride.saturating_mul(height) {
        unsafe {
            let _ = media_buffer.Unlock();
        }
        bail!(
            "ARGB buffer too small: len={} need={}",
            total,
            stride * height
        );
    }
    let src = unsafe { std::slice::from_raw_parts(raw_ptr, stride * height) };
    out[..stride * height].copy_from_slice(src);
    unsafe {
        media_buffer
            .Unlock()
            .context("IMFMediaBuffer::Unlock failed")?;
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn copy_argb32_to_bgra_cropped(
    media_buffer: &IMFMediaBuffer,
    source_width: usize,
    source_height: usize,
    output_width: usize,
    output_height: usize,
    out: &mut [u8],
) -> anyhow::Result<()> {
    if let Ok(buffer2d) = media_buffer.cast::<IMF2DBuffer>() {
        let mut line0 = std::ptr::null_mut::<u8>();
        let mut pitch = 0_i32;
        if unsafe { buffer2d.Lock2D(&mut line0, &mut pitch) }.is_ok() {
            let pitch = pitch.unsigned_abs() as usize;
            let row_bytes = output_width.saturating_mul(4);
            for y in 0..output_height {
                let src = unsafe { std::slice::from_raw_parts(line0.add(y * pitch), row_bytes) };
                let dst_offset = y.saturating_mul(row_bytes);
                out[dst_offset..dst_offset + row_bytes].copy_from_slice(src);
            }
            unsafe {
                buffer2d
                    .Unlock2D()
                    .context("IMF2DBuffer::Unlock2D failed")?;
            }
            return Ok(());
        }
    }

    let source_len = source_width.saturating_mul(source_height).saturating_mul(4);
    let mut full = vec![0_u8; source_len];
    copy_argb32_to_bgra(media_buffer, source_width, source_height, &mut full)?;
    let src_stride = source_width.saturating_mul(4);
    let dst_stride = output_width.saturating_mul(4);
    for y in 0..output_height {
        let src_offset = y.saturating_mul(src_stride);
        let dst_offset = y.saturating_mul(dst_stride);
        out[dst_offset..dst_offset + dst_stride]
            .copy_from_slice(&full[src_offset..src_offset + dst_stride]);
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn copy_nv12_to_bgra(
    media_buffer: &IMFMediaBuffer,
    width: usize,
    height: usize,
    out: &mut [u8],
) -> anyhow::Result<()> {
    let y_bytes = width.saturating_mul(height);
    let uv_bytes = y_bytes / 2;
    let required = y_bytes.saturating_add(uv_bytes);
    if let Ok(buffer2d) = media_buffer.cast::<IMF2DBuffer>() {
        let mut line0 = std::ptr::null_mut::<u8>();
        let mut pitch = 0_i32;
        if unsafe { buffer2d.Lock2D(&mut line0, &mut pitch) }.is_ok() {
            let pitch = pitch.unsigned_abs() as usize;
            if pitch < width {
                unsafe {
                    let _ = buffer2d.Unlock2D();
                }
                bail!("NV12 2D pitch too small: pitch={} width={}", pitch, width);
            }
            let y_plane_len = pitch.saturating_mul(height);
            let uv_plane_len = pitch.saturating_mul(height / 2);
            let all = unsafe { std::slice::from_raw_parts(line0, y_plane_len + uv_plane_len) };
            let y_plane = &all[..y_plane_len];
            let uv_plane = &all[y_plane_len..];
            nv12_to_bgra(y_plane, uv_plane, pitch, width, height, out);
            unsafe {
                buffer2d
                    .Unlock2D()
                    .context("IMF2DBuffer::Unlock2D failed")?;
            }
            return Ok(());
        }
        if let Some(bytes) = copy_imf2d_contiguous_bytes(&buffer2d, required)? {
            let y_plane = &bytes[..y_bytes];
            let uv_plane = &bytes[y_bytes..y_bytes + uv_bytes];
            nv12_to_bgra(y_plane, uv_plane, width, width, height, out);
            return Ok(());
        }
    }
    let mut raw_ptr = std::ptr::null_mut::<u8>();
    let mut _max_len = 0_u32;
    let mut cur_len = 0_u32;
    unsafe {
        media_buffer
            .Lock(&mut raw_ptr, Some(&mut _max_len), Some(&mut cur_len))
            .context("IMFMediaBuffer::Lock failed")?;
    }
    let total = usize::try_from(cur_len).unwrap_or(0);
    if total < required {
        unsafe {
            let _ = media_buffer.Unlock();
        }
        bail!("NV12 buffer too small: len={} need={}", total, required);
    }
    let all = unsafe { std::slice::from_raw_parts(raw_ptr, total) };
    let y_plane = &all[..y_bytes];
    let uv_plane = &all[y_bytes..y_bytes + uv_bytes];
    nv12_to_bgra(y_plane, uv_plane, width, width, height, out);
    unsafe {
        media_buffer
            .Unlock()
            .context("IMFMediaBuffer::Unlock failed")?;
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn copy_nv12_to_bgra_cropped(
    media_buffer: &IMFMediaBuffer,
    source_width: usize,
    source_height: usize,
    output_width: usize,
    output_height: usize,
    out: &mut [u8],
) -> anyhow::Result<()> {
    let y_bytes = source_width.saturating_mul(source_height);
    let uv_bytes = y_bytes / 2;
    let required = y_bytes.saturating_add(uv_bytes);
    if let Ok(buffer2d) = media_buffer.cast::<IMF2DBuffer>() {
        let mut line0 = std::ptr::null_mut::<u8>();
        let mut pitch = 0_i32;
        if unsafe { buffer2d.Lock2D(&mut line0, &mut pitch) }.is_ok() {
            let pitch = pitch.unsigned_abs() as usize;
            if pitch < source_width {
                unsafe {
                    let _ = buffer2d.Unlock2D();
                }
                bail!(
                    "NV12 resized 2D pitch too small: pitch={} width={}",
                    pitch,
                    source_width
                );
            }
            let y_plane_len = pitch.saturating_mul(source_height);
            let uv_plane_len = pitch.saturating_mul(source_height / 2);
            let all = unsafe { std::slice::from_raw_parts(line0, y_plane_len + uv_plane_len) };
            let y_plane = &all[..y_plane_len];
            let uv_plane = &all[y_plane_len..];
            nv12_to_bgra(y_plane, uv_plane, pitch, output_width, output_height, out);
            unsafe {
                buffer2d
                    .Unlock2D()
                    .context("IMF2DBuffer::Unlock2D failed")?;
            }
            return Ok(());
        }
        if let Some(bytes) = copy_imf2d_contiguous_bytes(&buffer2d, required)? {
            let y_plane = &bytes[..y_bytes];
            let uv_plane = &bytes[y_bytes..y_bytes + uv_bytes];
            nv12_to_bgra(
                y_plane,
                uv_plane,
                source_width,
                output_width,
                output_height,
                out,
            );
            return Ok(());
        }
    }
    let mut raw_ptr = std::ptr::null_mut::<u8>();
    let mut _max_len = 0_u32;
    let mut cur_len = 0_u32;
    unsafe {
        media_buffer
            .Lock(&mut raw_ptr, Some(&mut _max_len), Some(&mut cur_len))
            .context("IMFMediaBuffer::Lock failed")?;
    }
    let total = usize::try_from(cur_len).unwrap_or(0);
    if total < required {
        unsafe {
            let _ = media_buffer.Unlock();
        }
        bail!(
            "NV12 resized buffer too small: len={} need={}",
            total,
            required
        );
    }
    let all = unsafe { std::slice::from_raw_parts(raw_ptr, total) };
    let y_plane = &all[..y_bytes];
    let uv_plane = &all[y_bytes..y_bytes + uv_bytes];
    nv12_to_bgra(
        y_plane,
        uv_plane,
        source_width,
        output_width,
        output_height,
        out,
    );
    unsafe {
        media_buffer
            .Unlock()
            .context("IMFMediaBuffer::Unlock failed")?;
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn copy_yuy2_to_bgra(
    media_buffer: &IMFMediaBuffer,
    width: usize,
    height: usize,
    out: &mut [u8],
) -> anyhow::Result<()> {
    let stride = width.saturating_mul(2);
    let required = stride.saturating_mul(height);
    if let Ok(buffer2d) = media_buffer.cast::<IMF2DBuffer>() {
        let mut line0 = std::ptr::null_mut::<u8>();
        let mut pitch = 0_i32;
        if unsafe { buffer2d.Lock2D(&mut line0, &mut pitch) }.is_ok() {
            let pitch = pitch.unsigned_abs() as usize;
            let min_pitch = width.saturating_mul(2);
            if pitch < min_pitch {
                unsafe {
                    let _ = buffer2d.Unlock2D();
                }
                bail!(
                    "YUY2 2D pitch too small: pitch={} need={}",
                    pitch,
                    min_pitch
                );
            }
            let required = pitch.saturating_mul(height);
            let src = unsafe { std::slice::from_raw_parts(line0, required) };
            yuy2_to_bgra(src, pitch, width, height, out);
            unsafe {
                buffer2d
                    .Unlock2D()
                    .context("IMF2DBuffer::Unlock2D failed")?;
            }
            return Ok(());
        }
        if let Some(bytes) = copy_imf2d_contiguous_bytes(&buffer2d, required)? {
            yuy2_to_bgra(&bytes[..required], stride, width, height, out);
            return Ok(());
        }
    }
    let mut raw_ptr = std::ptr::null_mut::<u8>();
    let mut _max_len = 0_u32;
    let mut cur_len = 0_u32;
    unsafe {
        media_buffer
            .Lock(&mut raw_ptr, Some(&mut _max_len), Some(&mut cur_len))
            .context("IMFMediaBuffer::Lock failed")?;
    }
    let total = usize::try_from(cur_len).unwrap_or(0);
    if total < required {
        unsafe {
            let _ = media_buffer.Unlock();
        }
        bail!("YUY2 buffer too small: len={} need={}", total, required);
    }
    let src = unsafe { std::slice::from_raw_parts(raw_ptr, required) };
    yuy2_to_bgra(src, stride, width, height, out);
    unsafe {
        media_buffer
            .Unlock()
            .context("IMFMediaBuffer::Unlock failed")?;
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn copy_yuy2_to_bgra_cropped(
    media_buffer: &IMFMediaBuffer,
    source_width: usize,
    source_height: usize,
    output_width: usize,
    output_height: usize,
    out: &mut [u8],
) -> anyhow::Result<()> {
    let source_stride = source_width.saturating_mul(2);
    let required = source_stride.saturating_mul(source_height);
    if let Ok(buffer2d) = media_buffer.cast::<IMF2DBuffer>() {
        let mut line0 = std::ptr::null_mut::<u8>();
        let mut pitch = 0_i32;
        if unsafe { buffer2d.Lock2D(&mut line0, &mut pitch) }.is_ok() {
            let pitch = pitch.unsigned_abs() as usize;
            if pitch < source_stride {
                unsafe {
                    let _ = buffer2d.Unlock2D();
                }
                bail!(
                    "YUY2 cropped 2D pitch too small: pitch={} need={}",
                    pitch,
                    source_stride
                );
            }
            let row_len = pitch.saturating_mul(source_height);
            let src = unsafe { std::slice::from_raw_parts(line0, row_len) };
            yuy2_to_bgra(src, pitch, output_width, output_height, out);
            unsafe {
                buffer2d
                    .Unlock2D()
                    .context("IMF2DBuffer::Unlock2D failed")?;
            }
            return Ok(());
        }
    }

    let mut raw_ptr = std::ptr::null_mut::<u8>();
    let mut _max_len = 0_u32;
    let mut cur_len = 0_u32;
    unsafe {
        media_buffer
            .Lock(&mut raw_ptr, Some(&mut _max_len), Some(&mut cur_len))
            .context("IMFMediaBuffer::Lock failed")?;
    }
    let total = usize::try_from(cur_len).unwrap_or(0);
    if total < required {
        unsafe {
            let _ = media_buffer.Unlock();
        }
        bail!(
            "YUY2 cropped buffer too small: len={} need={}",
            total,
            required
        );
    }
    let src = unsafe { std::slice::from_raw_parts(raw_ptr, total) };
    yuy2_to_bgra(src, source_stride, output_width, output_height, out);
    unsafe {
        media_buffer
            .Unlock()
            .context("IMFMediaBuffer::Unlock failed")?;
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn copy_imf2d_contiguous_bytes(
    buffer2d: &IMF2DBuffer,
    required: usize,
) -> anyhow::Result<Option<Vec<u8>>> {
    let contiguous_len = match unsafe { buffer2d.GetContiguousLength() } {
        Ok(len) => usize::try_from(len).unwrap_or(0),
        Err(_) => 0,
    };
    let copy_len = contiguous_len.max(required);
    if copy_len == 0 {
        return Ok(None);
    }
    let mut bytes = vec![0_u8; copy_len];
    match unsafe { buffer2d.ContiguousCopyTo(&mut bytes) } {
        Ok(()) => {
            if bytes.len() < required {
                bail!(
                    "IMF2DBuffer::ContiguousCopyTo too small: len={} need={}",
                    bytes.len(),
                    required
                );
            }
            Ok(Some(bytes))
        }
        Err(_) => Ok(None),
    }
}

#[cfg(target_os = "windows")]
fn nv12_to_bgra(
    y_plane: &[u8],
    uv_plane: &[u8],
    stride: usize,
    width: usize,
    height: usize,
    out: &mut [u8],
) {
    #[inline]
    fn clamp(value: i32) -> u8 {
        value.clamp(0, 255) as u8
    }

    for y in 0..height {
        let y_row = y * stride;
        let uv_row = (y / 2) * stride;
        let mut x = 0_usize;
        while x + 1 < width {
            let uv_index = uv_row + x;
            let u_val = uv_plane[uv_index] as i32 - 128;
            let v_val = uv_plane[uv_index + 1] as i32 - 128;
            let r_uv = 409 * v_val + 128;
            let g_uv = -100 * u_val - 208 * v_val + 128;
            let b_uv = 516 * u_val + 128;

            let y0 = y_plane[y_row + x] as i32;
            let c0 = (y0 - 16).max(0);
            let r0 = (298 * c0 + r_uv) >> 8;
            let g0 = (298 * c0 + g_uv) >> 8;
            let b0 = (298 * c0 + b_uv) >> 8;
            let o0 = (y * width + x) * 4;
            out[o0] = clamp(b0);
            out[o0 + 1] = clamp(g0);
            out[o0 + 2] = clamp(r0);
            out[o0 + 3] = 255;

            let y1 = y_plane[y_row + x + 1] as i32;
            let c1 = (y1 - 16).max(0);
            let r1 = (298 * c1 + r_uv) >> 8;
            let g1 = (298 * c1 + g_uv) >> 8;
            let b1 = (298 * c1 + b_uv) >> 8;
            let o1 = (y * width + x + 1) * 4;
            out[o1] = clamp(b1);
            out[o1 + 1] = clamp(g1);
            out[o1 + 2] = clamp(r1);
            out[o1 + 3] = 255;
            x += 2;
        }

        if x < width {
            let uv_index = uv_row + (x / 2) * 2;
            let u_val = uv_plane[uv_index] as i32 - 128;
            let v_val = uv_plane[uv_index + 1] as i32 - 128;
            let y_val = y_plane[y_row + x] as i32;
            let c = (y_val - 16).max(0);
            let r = (298 * c + 409 * v_val + 128) >> 8;
            let g = (298 * c - 100 * u_val - 208 * v_val + 128) >> 8;
            let b = (298 * c + 516 * u_val + 128) >> 8;

            let o = (y * width + x) * 4;
            out[o] = clamp(b);
            out[o + 1] = clamp(g);
            out[o + 2] = clamp(r);
            out[o + 3] = 255;
        }
    }
}

#[cfg(target_os = "windows")]
fn yuy2_to_bgra(src: &[u8], stride: usize, width: usize, height: usize, out: &mut [u8]) {
    #[inline]
    fn clamp(value: i32) -> u8 {
        value.clamp(0, 255) as u8
    }

    for y in 0..height {
        let row = &src[y * stride..(y + 1) * stride];
        for x in (0..width).step_by(2) {
            let i = x * 2;
            let y0 = row[i] as i32;
            let u = row[i + 1] as i32 - 128;
            let y1 = row[i + 2] as i32;
            let v = row[i + 3] as i32 - 128;

            let c0 = (y0 - 16).max(0);
            let c1 = (y1 - 16).max(0);
            let r_uv = 409 * v + 128;
            let g_uv = -100 * u - 208 * v + 128;
            let b_uv = 516 * u + 128;
            let r0 = (298 * c0 + r_uv) >> 8;
            let g0 = (298 * c0 + g_uv) >> 8;
            let b0 = (298 * c0 + b_uv) >> 8;
            let r1 = (298 * c1 + r_uv) >> 8;
            let g1 = (298 * c1 + g_uv) >> 8;
            let b1 = (298 * c1 + b_uv) >> 8;

            let o0 = (y * width + x) * 4;
            out[o0] = clamp(b0);
            out[o0 + 1] = clamp(g0);
            out[o0 + 2] = clamp(r0);
            out[o0 + 3] = 255;

            if x + 1 < width {
                let o1 = (y * width + x + 1) * 4;
                out[o1] = clamp(b1);
                out[o1 + 1] = clamp(g1);
                out[o1 + 2] = clamp(r1);
                out[o1 + 3] = 255;
            }
        }
    }
}

#[cfg(target_os = "windows")]
struct EffectiveOutputSize {
    width: usize,
    height: usize,
    crop_applied: bool,
}

#[cfg(target_os = "windows")]
fn choose_effective_output_size(
    source_width: usize,
    source_height: usize,
    expected_width: usize,
    expected_height: usize,
) -> EffectiveOutputSize {
    if expected_width == 0 || expected_height == 0 {
        return EffectiveOutputSize {
            width: source_width,
            height: source_height,
            crop_applied: false,
        };
    }
    let target_width = expected_width.min(source_width).max(2) & !1;
    let target_height = expected_height.min(source_height).max(2) & !1;
    let width = target_width.max(2).min(source_width.max(2));
    let height = target_height.max(2).min(source_height.max(2));
    EffectiveOutputSize {
        width,
        height,
        crop_applied: width < source_width || height < source_height,
    }
}

#[derive(Default, Clone, Debug)]
struct DecoderPerfMetrics {
    backend: String,
    decode_ms: f64,
    convert_ms: f64,
    spout_send_ms: f64,
    send_path: String,
    mf_process_output_ms: f64,
    cpu_color_convert_ms: f64,
    frame_stats_ms: f64,
    spout_bridge_ms: f64,
    spout_swap_ms: f64,
    spout_upload_ms: f64,
    spout_send_texture_ms: f64,
    texture_path_ratio: f64,
    stream_latency_ms: f64,
    fastpath_state: String,
    fastpath_fallback_count: u64,
    fastpath_recover_count: u64,
    crop_applied: bool,
    effective_width: u64,
    effective_height: u64,
    queue_depth: u64,
    frame_mean_luma: f64,
    frame_non_black_ratio: f64,
}

struct DecoderState {
    decoder: Option<Box<dyn VideoDecoder>>,
    needs_keyframe: bool,
    h264: H264Context,
    decode_preference: DecodeBackendPreference,
    coded_width: Option<usize>,
    coded_height: Option<usize>,
    frame: DecodedFrame,
    perf: DecoderPerfMetrics,
    video_chunks_in: u64,
    video_frames_out: u64,
    video_chunks_dropped: u64,
    consecutive_empty_decodes: u32,
    output_scale_percent: u32,
    stream_time_anchor: Option<(Instant, u64)>,
    catchup_active: bool,
    last_config_fingerprint: Option<u64>,
    last_chunk_received_at: Option<Instant>,
    perf_config: OutputHelperPerfConfig,
    texture_fastpath_enabled: bool,
    fastpath_state: FastpathState,
    fastpath_retry_after: Option<Instant>,
    fastpath_high_send_streak: u32,
    fastpath_recovery_streak: u32,
    fastpath_retry_frames: u32,
    frame_stats_counter: u64,
    send_window_started: Instant,
    send_window_total: u64,
    send_window_texture: u64,
    #[cfg(target_os = "windows")]
    preferred_dx11_device: Option<*mut std::ffi::c_void>,
}

impl DecoderState {
    fn new(
        decode_preference: DecodeBackendPreference,
        perf_config: OutputHelperPerfConfig,
    ) -> Self {
        let mut perf = DecoderPerfMetrics::default();
        perf.send_path = SendPath::None.as_str().to_string();
        let texture_fastpath_enabled = perf_config.texture_fastpath;
        let initial_fastpath_state = if texture_fastpath_enabled {
            FastpathState::Texture
        } else {
            FastpathState::BgraFallback
        };
        perf.fastpath_state = initial_fastpath_state.as_str().to_string();
        Self {
            decoder: None,
            needs_keyframe: true,
            h264: H264Context::default(),
            decode_preference,
            coded_width: None,
            coded_height: None,
            frame: DecodedFrame::default(),
            perf,
            video_chunks_in: 0,
            video_frames_out: 0,
            video_chunks_dropped: 0,
            consecutive_empty_decodes: 0,
            output_scale_percent: SCALE_FULL,
            stream_time_anchor: None,
            catchup_active: false,
            last_config_fingerprint: None,
            last_chunk_received_at: None,
            perf_config,
            texture_fastpath_enabled,
            fastpath_state: initial_fastpath_state,
            fastpath_retry_after: None,
            fastpath_high_send_streak: 0,
            fastpath_recovery_streak: 0,
            fastpath_retry_frames: 0,
            frame_stats_counter: 0,
            send_window_started: Instant::now(),
            send_window_total: 0,
            send_window_texture: 0,
            #[cfg(target_os = "windows")]
            preferred_dx11_device: None,
        }
    }

    fn update_avcc_from_base64(&mut self, encoded: &str) -> bool {
        let Ok(bytes) = BASE64_STANDARD.decode(encoded) else {
            return false;
        };
        self.h264.update_from_avcc_record(&bytes)
    }

    fn clear_avcc(&mut self) {
        self.h264.clear();
    }

    fn is_idle(&self) -> bool {
        self.last_chunk_received_at
            .map(|at| at.elapsed() >= PLAYER_IDLE_TIMEOUT)
            .unwrap_or(false)
    }

    fn observe_config_received(&mut self) {
        self.last_chunk_received_at = Some(Instant::now());
    }

    fn observe_chunk_received(&mut self) {
        self.video_chunks_in = self.video_chunks_in.saturating_add(1);
        self.last_chunk_received_at = Some(Instant::now());
        self.update_queue_depth();
    }

    fn observe_pipeline_drop(&mut self, dropped: u64, reason: &str) {
        if dropped == 0 {
            return;
        }
        self.video_chunks_dropped = self.video_chunks_dropped.saturating_add(dropped);
        if reason == "compressed-queue-overflow" {
            self.request_keyframe_resync("compressed-queue-overflow");
        }
        self.update_queue_depth();
    }

    fn request_keyframe_resync(&mut self, reason: &str) {
        self.catchup_active = false;
        self.needs_keyframe = true;
        self.flush_decoder();
        eprintln!("output-helper: request keyframe resync reason={reason}");
    }

    fn decode(&mut self, packet: &[u8], keyframe: bool, timestamp_us: u64) -> Option<DecodedFrame> {
        self.maybe_retry_fastpath();
        let latency_ms = self.estimate_stream_latency_ms(timestamp_us);
        self.perf.stream_latency_ms = latency_ms.max(0.0);
        let payload_has_idr = self.h264.payload_contains_idr(packet);
        let payload_has_parameter_sets = self.h264.payload_contains_parameter_sets(packet);
        let payload_nal_types = h264_nal_types(packet, self.h264.nal_length_size);
        let effective_keyframe = keyframe || payload_has_idr || payload_has_parameter_sets;
        if (payload_has_idr || payload_has_parameter_sets) && !keyframe {
            eprintln!(
                "output-helper: keyframe flag missing; treating packet as keyframe via H264 NAL detection"
            );
        }
        if self.perf_config.verbose {
            eprintln!(
                "output-helper: h264 chunk size={} nal_types={:?} keyframe_flag={} effective_keyframe={} idr={} sps_pps={}",
                packet.len(),
                payload_nal_types,
                keyframe,
                effective_keyframe,
                payload_has_idr,
                payload_has_parameter_sets
            );
        }
        if self.catchup_active {
            if latency_ms <= CATCHUP_EXIT_LAG_MS {
                self.catchup_active = false;
                if self.perf_config.verbose {
                    eprintln!(
                        "output-helper: catchup disabled latency_ms={:.1}",
                        latency_ms
                    );
                }
            }
        } else if !effective_keyframe && latency_ms > CATCHUP_ENTER_LAG_MS {
            self.catchup_active = true;
            if self.perf_config.verbose {
                eprintln!(
                    "output-helper: catchup enabled latency_ms={:.1}; decode continues (no packet drop)",
                    latency_ms
                );
            }
        }
        let backlog = self.pending_backlog();
        if self.needs_keyframe && !effective_keyframe {
            self.video_chunks_dropped = self.video_chunks_dropped.saturating_add(1);
            self.update_queue_depth();
            return None;
        }
        if effective_keyframe {
            self.needs_keyframe = false;
        }
        let normalized_packet = match self.decoder.as_ref().map(|decoder| decoder.backend_kind()) {
            Some(DecodeBackendKind::OpenH264) => {
                self.h264.normalize_for_decode(packet, effective_keyframe)
            }
            #[cfg(target_os = "macos")]
            Some(DecodeBackendKind::VideoToolbox) => None,
            #[cfg(target_os = "windows")]
            Some(DecodeBackendKind::MfD3d11) => {
                self.h264.normalize_for_decode(packet, effective_keyframe)
            }
            None => self.h264.normalize_for_decode(packet, effective_keyframe),
        };

        if !self.ensure_decoder() {
            self.needs_keyframe = true;
            self.update_queue_depth();
            return None;
        }
        let mut target_size: Option<(usize, usize)> = None;
        if let (Some(width), Some(height)) = (self.coded_width, self.coded_height) {
            let adaptive_scale = self.desired_scale_percent();
            if self.output_scale_percent != SCALE_FULL {
                eprintln!(
                    "output-helper: adaptive scale disabled forcing {}% -> {}% requested={} backlog={} convert_ms={:.2}",
                    self.output_scale_percent,
                    SCALE_FULL,
                    adaptive_scale,
                    backlog,
                    self.perf.convert_ms
                );
                self.output_scale_percent = SCALE_FULL;
            }
            // Keep pipeline interfaces intact but lock effective output scale at 100%.
            target_size = Some(scaled_coded_size(width, height, SCALE_FULL));
        }
        let decoder = self.decoder.as_mut().expect("decoder initialized");
        if let Some((target_width, target_height)) = target_size {
            decoder.set_expected_size(target_width, target_height);
        }
        let backend = decoder.backend_kind();
        let timings = if backend == DecodeBackendKind::OpenH264 {
            let Some(normalized_packet) = normalized_packet.as_ref() else {
                self.needs_keyframe = true;
                self.update_queue_depth();
                return None;
            };
            decoder.decode_into(normalized_packet, &mut self.frame)
        } else {
            #[cfg(target_os = "macos")]
            {
                if backend == DecodeBackendKind::VideoToolbox {
                    decoder.decode_into(packet, &mut self.frame)
                } else {
                    // Media Foundation backend can accept different NAL framing depending on stream.
                    // Try the original payload first, then AnnexB-normalized payload as fallback.
                    let direct = decoder.decode_into(packet, &mut self.frame);
                    if direct.is_some() {
                        direct
                    } else if let Some(normalized) = normalized_packet.as_ref() {
                        if normalized.as_slice() != packet {
                            decoder.decode_into(normalized, &mut self.frame)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                }
            }
            #[cfg(not(target_os = "macos"))]
            {
                // Media Foundation backend can accept different NAL framing depending on stream.
                // Try the original payload first, then AnnexB-normalized payload as fallback.
                let direct = decoder.decode_into(packet, &mut self.frame);
                if direct.is_some() {
                    direct
                } else if let Some(normalized) = normalized_packet.as_ref() {
                    if normalized.as_slice() != packet {
                        decoder.decode_into(normalized, &mut self.frame)
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
        };
        let Some(timings) = timings else {
            self.consecutive_empty_decodes = self.consecutive_empty_decodes.saturating_add(1);
            #[cfg(target_os = "macos")]
            if backend == DecodeBackendKind::VideoToolbox
                && self.consecutive_empty_decodes <= VT_STALL_HEX_DUMP_LIMIT as u32
            {
                eprintln!(
                    "output-helper: videotoolbox stall chunk={} empty_decodes={} head={}",
                    self.video_chunks_in,
                    self.consecutive_empty_decodes,
                    hex_dump_prefix(packet, VT_STALL_HEX_DUMP_BYTES)
                );
            }
            #[cfg(target_os = "windows")]
            if backend == DecodeBackendKind::MfD3d11
                && self.texture_fastpath_enabled
                && self.consecutive_empty_decodes >= FASTPATH_DECODE_STALL_THRESHOLD
            {
                self.enter_fastpath_fallback("decode-stall");
                self.reset_decoder();
                self.update_queue_depth();
                return None;
            }
            if !self.needs_keyframe
                && self.consecutive_empty_decodes >= KEYFRAME_RESYNC_EMPTY_THRESHOLD
            {
                self.needs_keyframe = true;
                eprintln!(
                    "output-helper: decoder stalled backend={} empty_decodes={}; waiting keyframe",
                    backend.as_str(),
                    self.consecutive_empty_decodes
                );
            }
            self.maybe_fallback_decoder(backend);
            self.update_queue_depth();
            return None;
        };
        self.consecutive_empty_decodes = 0;
        self.needs_keyframe = false;
        if self.video_frames_out == 0 {
            eprintln!(
                "output-helper: first decoded frame backend={} size={}x{}",
                backend.as_str(),
                self.frame.width,
                self.frame.height
            );
        }
        self.video_frames_out = self.video_frames_out.saturating_add(1);
        self.observe_decode(timings);
        self.update_queue_depth();
        Some(std::mem::take(&mut self.frame))
    }

    fn observe_send(&mut self, elapsed: Duration, send_result: &VideoSendResult) {
        let elapsed_ms = elapsed.as_secs_f64() * 1000.0;
        update_ewma_ms(&mut self.perf.spout_send_ms, elapsed);
        self.perf.send_path = send_result.path.as_str().to_string();
        if let Some(metrics) = send_result.spout_bridge_metrics {
            update_ewma_ms_f64(&mut self.perf.spout_bridge_ms, metrics.total_ms);
            update_ewma_ms_f64(&mut self.perf.spout_swap_ms, metrics.swap_ms);
            update_ewma_ms_f64(&mut self.perf.spout_upload_ms, metrics.upload_ms);
            update_ewma_ms_f64(&mut self.perf.spout_send_texture_ms, metrics.send_ms);
        }

        if self.perf_config.texture_fastpath {
            self.observe_fastpath(send_result, elapsed_ms);
        }

        if !send_result.sent {
            return;
        }
        self.send_window_total = self.send_window_total.saturating_add(1);
        if send_result.path == SendPath::Texture {
            self.send_window_texture = self.send_window_texture.saturating_add(1);
        }
        let now = Instant::now();
        if now.duration_since(self.send_window_started) >= Duration::from_secs(1) {
            self.perf.texture_path_ratio = if self.send_window_total > 0 {
                self.send_window_texture as f64 / self.send_window_total as f64
            } else {
                0.0
            };
            self.send_window_started = now;
            self.send_window_total = 0;
            self.send_window_texture = 0;
        }
    }

    fn observe_fastpath(&mut self, send_result: &VideoSendResult, elapsed_ms: f64) {
        match self.fastpath_state {
            FastpathState::Texture => {
                if send_result.texture_attempted && send_result.texture_failed {
                    self.enter_fastpath_fallback("sendtexture-failed");
                    return;
                }
                if send_result.path == SendPath::Texture
                    && elapsed_ms >= FASTPATH_HIGH_SEND_MS_THRESHOLD
                {
                    self.fastpath_high_send_streak =
                        self.fastpath_high_send_streak.saturating_add(1);
                    if self.fastpath_high_send_streak >= FASTPATH_HIGH_SEND_STREAK_THRESHOLD {
                        self.enter_fastpath_fallback("high-send-ms");
                    }
                } else {
                    self.fastpath_high_send_streak = 0;
                }
            }
            FastpathState::BgraFallback => {}
            FastpathState::Retrying => {
                if send_result.texture_attempted && send_result.texture_failed {
                    self.enter_fastpath_fallback("retry-failed");
                    return;
                }
                self.fastpath_retry_frames = self.fastpath_retry_frames.saturating_add(1);
                if send_result.path == SendPath::Texture && send_result.sent {
                    self.fastpath_recovery_streak = self.fastpath_recovery_streak.saturating_add(1);
                    if self.fastpath_recovery_streak >= FASTPATH_RECOVERY_STREAK_REQUIRED {
                        self.mark_fastpath_recovered();
                    }
                    return;
                }
                self.fastpath_recovery_streak = 0;
                if self.fastpath_retry_frames >= FASTPATH_RECOVERY_STREAK_REQUIRED {
                    self.enter_fastpath_fallback("retry-timeout");
                }
            }
        }
    }

    fn maybe_retry_fastpath(&mut self) {
        if self.fastpath_state != FastpathState::BgraFallback {
            return;
        }
        if !self.perf_config.texture_fastpath {
            return;
        }
        let Some(retry_after) = self.fastpath_retry_after else {
            return;
        };
        if Instant::now() < retry_after {
            return;
        }
        self.fastpath_state = FastpathState::Retrying;
        self.fastpath_retry_after = None;
        self.fastpath_recovery_streak = 0;
        self.fastpath_retry_frames = 0;
        self.fastpath_high_send_streak = 0;
        self.perf.fastpath_state = self.fastpath_state.as_str().to_string();
        self.set_texture_fastpath_enabled(true);
        if self.perf_config.verbose {
            eprintln!("output-helper: fastpath retry");
        }
    }

    fn enter_fastpath_fallback(&mut self, reason: &str) {
        if self.fastpath_state == FastpathState::BgraFallback {
            return;
        }
        self.fastpath_state = FastpathState::BgraFallback;
        self.fastpath_retry_after = Some(Instant::now() + FASTPATH_RETRY_COOLDOWN);
        self.fastpath_high_send_streak = 0;
        self.fastpath_recovery_streak = 0;
        self.fastpath_retry_frames = 0;
        self.perf.fastpath_state = self.fastpath_state.as_str().to_string();
        self.perf.fastpath_fallback_count = self.perf.fastpath_fallback_count.saturating_add(1);
        self.set_texture_fastpath_enabled(false);
        if self.perf_config.verbose {
            eprintln!("output-helper: fastpath enter-fallback reason={reason}");
        }
    }

    fn mark_fastpath_recovered(&mut self) {
        self.fastpath_state = FastpathState::Texture;
        self.fastpath_retry_after = None;
        self.fastpath_high_send_streak = 0;
        self.fastpath_recovery_streak = 0;
        self.fastpath_retry_frames = 0;
        self.perf.fastpath_state = self.fastpath_state.as_str().to_string();
        self.perf.fastpath_recover_count = self.perf.fastpath_recover_count.saturating_add(1);
        self.set_texture_fastpath_enabled(true);
        if self.perf_config.verbose {
            eprintln!("output-helper: fastpath recovered");
        }
    }

    fn set_texture_fastpath_enabled(&mut self, enabled: bool) {
        self.texture_fastpath_enabled = enabled;
        if let Some(decoder) = self.decoder.as_mut() {
            decoder.set_texture_fastpath_enabled(enabled);
        }
    }

    fn flush_decoder(&mut self) {
        if let Some(decoder) = self.decoder.as_mut() {
            decoder.flush();
        }
        self.consecutive_empty_decodes = 0;
    }

    fn perf_metrics(&self) -> &DecoderPerfMetrics {
        &self.perf
    }

    fn reset_decoder(&mut self) {
        self.decoder = None;
        self.needs_keyframe = true;
        self.consecutive_empty_decodes = 0;
        self.output_scale_percent = SCALE_FULL;
        self.frame = DecodedFrame::default();
        self.frame_stats_counter = 0;
        self.send_window_started = Instant::now();
        self.send_window_total = 0;
        self.send_window_texture = 0;
        self.fastpath_high_send_streak = 0;
        self.fastpath_recovery_streak = 0;
        self.fastpath_retry_frames = 0;
    }

    #[cfg(target_os = "windows")]
    fn set_preferred_dx11_device(&mut self, device: Option<*mut std::ffi::c_void>) {
        self.preferred_dx11_device = device;
    }

    #[cfg(not(target_os = "windows"))]
    fn set_preferred_dx11_device(&mut self, _device: Option<*mut std::ffi::c_void>) {}

    fn set_coded_size(&mut self, coded_width: Option<usize>, coded_height: Option<usize>) {
        self.coded_width = coded_width;
        self.coded_height = coded_height;
        if let (Some(width), Some(height), Some(decoder)) =
            (self.coded_width, self.coded_height, self.decoder.as_mut())
        {
            decoder.set_expected_size(width, height);
        }
    }

    fn observe_decode(&mut self, timings: DecodeTimings) {
        update_ewma_ms(&mut self.perf.decode_ms, timings.decode);
        update_ewma_ms(&mut self.perf.convert_ms, timings.convert);
        update_ewma_ms(
            &mut self.perf.mf_process_output_ms,
            timings.mf_process_output,
        );
        update_ewma_ms(
            &mut self.perf.cpu_color_convert_ms,
            timings.cpu_color_convert,
        );
        self.perf.backend = self
            .decoder
            .as_ref()
            .map(|decoder| decoder.backend_kind().as_str().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        self.perf.crop_applied = self.frame.crop_applied;
        self.perf.effective_width = self.frame.width as u64;
        self.perf.effective_height = self.frame.height as u64;
        #[cfg(target_os = "windows")]
        if self.frame.dx11_texture.is_some() && self.frame.bgra.is_empty() {
            self.perf.frame_mean_luma = -1.0;
            self.perf.frame_non_black_ratio = -1.0;
            self.perf.frame_stats_ms = 0.0;
            return;
        }
        #[cfg(target_os = "macos")]
        if self.frame.cv_pixel_buffer.is_some() && self.frame.bgra.is_empty() {
            self.perf.frame_mean_luma = -1.0;
            self.perf.frame_non_black_ratio = -1.0;
            self.perf.frame_stats_ms = 0.0;
            return;
        }
        if self.frame.bgra.is_empty() {
            self.perf.frame_mean_luma = 0.0;
            self.perf.frame_non_black_ratio = 0.0;
            self.perf.frame_stats_ms = 0.0;
            return;
        }
        self.frame_stats_counter = self.frame_stats_counter.saturating_add(1);
        let sample_every = self.perf_config.frame_stats_every.max(1);
        if self.frame_stats_counter % sample_every != 0 {
            return;
        }
        let frame_stats_started = Instant::now();
        if let Some((mean_luma, non_black_ratio, _first)) = bgra_frame_stats(&self.frame.bgra) {
            self.perf.frame_mean_luma = mean_luma;
            self.perf.frame_non_black_ratio = non_black_ratio;
        }
        update_ewma_ms(&mut self.perf.frame_stats_ms, frame_stats_started.elapsed());
    }

    fn update_queue_depth(&mut self) {
        self.perf.queue_depth = self.pending_backlog();
    }

    fn estimate_stream_latency_ms(&mut self, timestamp_us: u64) -> f64 {
        let now = Instant::now();
        let Some((anchor_instant, anchor_timestamp_us)) = self.stream_time_anchor else {
            self.stream_time_anchor = Some((now, timestamp_us));
            return 0.0;
        };
        if timestamp_us.saturating_add(5_000_000) < anchor_timestamp_us {
            self.stream_time_anchor = Some((now, timestamp_us));
            return 0.0;
        }
        if timestamp_us < anchor_timestamp_us {
            return 0.0;
        }
        let delta_us = timestamp_us.saturating_sub(anchor_timestamp_us);
        let expected = anchor_instant + Duration::from_micros(delta_us);
        let latency_ms = if now >= expected {
            now.duration_since(expected).as_secs_f64() * 1000.0
        } else {
            -(expected.duration_since(now).as_secs_f64() * 1000.0)
        };
        if latency_ms < -250.0 {
            self.stream_time_anchor = Some((now, timestamp_us));
            return 0.0;
        }
        latency_ms
    }

    fn pending_backlog(&self) -> u64 {
        self.video_chunks_in
            .saturating_sub(self.video_frames_out)
            .saturating_sub(self.video_chunks_dropped)
    }

    fn desired_scale_percent(&self) -> u32 {
        let convert_ms = self.perf.convert_ms;
        let backlog = self.pending_backlog();
        let severe_pressure = backlog >= BACKLOG_SEVERE_THRESHOLD;
        match self.output_scale_percent {
            SCALE_LOW => {
                if !severe_pressure && convert_ms < 8.0 {
                    SCALE_MEDIUM
                } else {
                    SCALE_LOW
                }
            }
            SCALE_MEDIUM => {
                if severe_pressure || convert_ms > 14.0 {
                    SCALE_LOW
                } else if convert_ms < 6.0 {
                    SCALE_FULL
                } else {
                    SCALE_MEDIUM
                }
            }
            _ => {
                if severe_pressure || convert_ms > 11.0 {
                    SCALE_MEDIUM
                } else {
                    SCALE_FULL
                }
            }
        }
    }

    fn maybe_fallback_decoder(&mut self, backend: DecodeBackendKind) {
        #[cfg(target_os = "windows")]
        {
            if backend == DecodeBackendKind::MfD3d11
                && self.video_frames_out == 0
                && self.consecutive_empty_decodes >= MF_STALL_SWITCH_THRESHOLD
            {
                eprintln!(
                    "output-helper: mf-d3d11 produced no frames for {} chunks; fallback to openh264",
                    self.consecutive_empty_decodes
                );
                self.decoder = None;
                self.decode_preference = DecodeBackendPreference::OpenH264;
                self.needs_keyframe = true;
                self.consecutive_empty_decodes = 0;
            }
        }
        #[cfg(target_os = "macos")]
        {
            if backend == DecodeBackendKind::VideoToolbox
                && self.consecutive_empty_decodes >= VT_STALL_SWITCH_THRESHOLD
            {
                if vt_fallback_disabled() {
                    eprintln!(
                        "output-helper: videotoolbox stall detected for {} chunks; fallback suppressed by BROWSER_PORT_DISABLE_VT_FALLBACK=1",
                        self.consecutive_empty_decodes
                    );
                    return;
                }
                eprintln!(
                    "output-helper: videotoolbox stalled for {} chunks; fallback to openh264",
                    self.consecutive_empty_decodes
                );
                self.decoder = None;
                self.decode_preference = DecodeBackendPreference::OpenH264;
                self.needs_keyframe = true;
                self.consecutive_empty_decodes = 0;
            }
        }
        #[cfg(not(target_os = "windows"))]
        let _ = backend;
    }

    fn ensure_decoder(&mut self) -> bool {
        if self.decoder.is_none() {
            for backend in preferred_backend_order(self.decode_preference) {
                match create_decoder_backend(
                    backend,
                    &self.h264,
                    #[cfg(target_os = "windows")]
                    self.preferred_dx11_device,
                    self.texture_fastpath_enabled,
                    self.perf_config.verbose,
                ) {
                    Ok(mut decoder) => {
                        decoder.set_texture_fastpath_enabled(self.texture_fastpath_enabled);
                        self.perf.backend = backend.as_str().to_string();
                        eprintln!(
                            "output-helper: decoder backend selected backend={}",
                            backend.as_str()
                        );
                        self.decoder = Some(decoder);
                        break;
                    }
                    Err(err) => {
                        eprintln!(
                            "output-helper: decoder backend init failed backend={} reason={}",
                            backend.as_str(),
                            err
                        );
                    }
                }
            }
        }
        self.decoder.is_some()
    }
}

fn create_decoder_backend(
    backend: DecodeBackendKind,
    h264: &H264Context,
    #[cfg(target_os = "windows")] preferred_dx11_device: Option<*mut std::ffi::c_void>,
    texture_fastpath_enabled: bool,
    verbose: bool,
) -> anyhow::Result<Box<dyn VideoDecoder>> {
    #[cfg(not(target_os = "macos"))]
    let _ = h264;
    #[cfg(not(target_os = "windows"))]
    let _ = texture_fastpath_enabled;
    let _ = verbose;
    match backend {
        #[cfg(target_os = "macos")]
        DecodeBackendKind::VideoToolbox => Ok(Box::new(VideoToolboxDecoder::new(
            h264.video_toolbox_config(),
            verbose,
        )?)),
        #[cfg(target_os = "windows")]
        DecodeBackendKind::MfD3d11 => Ok(Box::new(MfD3d11Decoder::new(
            preferred_dx11_device,
            texture_fastpath_enabled,
        )?)),
        DecodeBackendKind::OpenH264 => Ok(Box::new(OpenH264Decoder::new()?)),
    }
}

fn update_ewma_ms(slot: &mut f64, elapsed: Duration) {
    let sample_ms = elapsed.as_secs_f64() * 1000.0;
    update_ewma_ms_f64(slot, sample_ms);
}

fn update_ewma_ms_f64(slot: &mut f64, sample_ms: f64) {
    if *slot == 0.0 {
        *slot = sample_ms;
    } else {
        *slot = *slot * (1.0 - PERF_EWMA_ALPHA) + sample_ms * PERF_EWMA_ALPHA;
    }
}

fn bgra_frame_stats(bgra: &[u8]) -> Option<(f64, f64, [u8; 4])> {
    if bgra.len() < 4 {
        return None;
    }
    let mut sum_luma = 0.0_f64;
    let mut non_black = 0_u64;
    let mut first = [0_u8; 4];
    let mut first_set = false;
    let pixel_count = bgra.len() / 4;
    for chunk in bgra.chunks_exact(4) {
        let b = f64::from(chunk[0]);
        let g = f64::from(chunk[1]);
        let r = f64::from(chunk[2]);
        sum_luma += 0.114 * b + 0.587 * g + 0.299 * r;
        if chunk[0] != 0 || chunk[1] != 0 || chunk[2] != 0 {
            non_black = non_black.saturating_add(1);
            if !first_set {
                first.copy_from_slice(chunk);
                first_set = true;
            }
        }
    }
    Some((
        sum_luma / pixel_count.max(1) as f64,
        non_black as f64 / pixel_count.max(1) as f64,
        first,
    ))
}

fn scaled_coded_size(width: usize, height: usize, scale_percent: u32) -> (usize, usize) {
    if scale_percent >= SCALE_FULL {
        return (width, height);
    }
    let scale = usize::try_from(scale_percent).unwrap_or(100);
    let scaled_w = (width.saturating_mul(scale) / 100).max(16) & !1;
    let scaled_h = (height.saturating_mul(scale) / 100).max(16) & !1;
    (scaled_w.max(16), scaled_h.max(16))
}

#[derive(Default)]
struct H264Context {
    nal_length_size: Option<usize>,
    parameter_sets: Vec<Vec<u8>>,
}

impl H264Context {
    fn clear(&mut self) {
        self.nal_length_size = None;
        self.parameter_sets.clear();
    }

    fn update_from_avcc_record(&mut self, record: &[u8]) -> bool {
        let Some((nal_length_size, parameter_sets, _sps, _pps)) = parse_avcc_record(record) else {
            return false;
        };
        self.nal_length_size = Some(nal_length_size);
        self.parameter_sets = parameter_sets;
        true
    }

    fn video_toolbox_config(&self) -> Option<VideoToolboxH264Config> {
        if self.parameter_sets.is_empty() {
            return None;
        }
        Some(VideoToolboxH264Config {
            nal_length_size: self.nal_length_size.unwrap_or(4).max(1).min(4),
            parameter_sets: self.parameter_sets.clone(),
        })
    }

    fn normalize_for_decode(&self, payload: &[u8], keyframe: bool) -> Option<Vec<u8>> {
        if payload.is_empty() {
            return None;
        }

        let mut normalized = if is_annexb(payload) {
            payload.to_vec()
        } else if let Some(nal_length_size) = self.nal_length_size {
            avcc_to_annexb(payload, nal_length_size)?
        } else if let Some(best_effort) = best_effort_avcc_to_annexb(payload) {
            best_effort
        } else {
            return None;
        };

        if keyframe && !self.parameter_sets.is_empty() && !annexb_contains_sps_or_pps(&normalized) {
            normalized = prepend_parameter_sets(&self.parameter_sets, &normalized);
        }

        Some(normalized)
    }

    fn payload_contains_idr(&self, payload: &[u8]) -> bool {
        self.payload_to_annexb(payload)
            .map(|annexb| annexb_contains_idr(&annexb))
            .unwrap_or(false)
    }

    fn payload_contains_parameter_sets(&self, payload: &[u8]) -> bool {
        self.payload_to_annexb(payload)
            .map(|annexb| annexb_contains_sps_or_pps(&annexb))
            .unwrap_or(false)
    }

    fn payload_to_annexb(&self, payload: &[u8]) -> Option<Vec<u8>> {
        if payload.is_empty() {
            return None;
        }
        if is_annexb(payload) {
            return Some(payload.to_vec());
        }
        if let Some(nal_length_size) = self.nal_length_size {
            if let Some(annexb) = avcc_to_annexb(payload, nal_length_size) {
                return Some(annexb);
            }
        }
        best_effort_avcc_to_annexb(payload)
    }
}

fn parse_avcc_record(record: &[u8]) -> Option<(usize, Vec<Vec<u8>>, Vec<Vec<u8>>, Vec<Vec<u8>>)> {
    if record.len() < 7 || record[0] != 1 {
        return None;
    }

    let nal_length_size = ((record[4] & 0x03) + 1) as usize;
    if nal_length_size == 0 || nal_length_size > 4 {
        return None;
    }

    let mut offset = 5;
    let sps_count = (record[offset] & 0x1F) as usize;
    offset += 1;

    let mut sps = Vec::new();
    for _ in 0..sps_count {
        let (unit, next_offset) = parse_avcc_unit(record, offset)?;
        if unit.first().map(|value| value & 0x1F) == Some(7) {
            sps.push(unit.to_vec());
        }
        offset = next_offset;
    }

    if offset >= record.len() {
        return None;
    }
    let pps_count = record[offset] as usize;
    offset += 1;

    let mut pps = Vec::new();
    for _ in 0..pps_count {
        let (unit, next_offset) = parse_avcc_unit(record, offset)?;
        if unit.first().map(|value| value & 0x1F) == Some(8) {
            pps.push(unit.to_vec());
        }
        offset = next_offset;
    }

    let mut parameter_sets = Vec::with_capacity(sps.len() + pps.len());
    parameter_sets.extend_from_slice(&sps);
    parameter_sets.extend_from_slice(&pps);
    Some((nal_length_size, parameter_sets, sps, pps))
}

fn parse_avcc_unit(record: &[u8], offset: usize) -> Option<(&[u8], usize)> {
    if offset + 2 > record.len() {
        return None;
    }
    let size = u16::from_be_bytes([record[offset], record[offset + 1]]) as usize;
    let start = offset + 2;
    let end = start + size;
    if size == 0 || end > record.len() {
        return None;
    }
    Some((&record[start..end], end))
}

fn is_annexb(payload: &[u8]) -> bool {
    payload.starts_with(&[0, 0, 1]) || payload.starts_with(&[0, 0, 0, 1])
}

fn split_annexb_nals(payload: &[u8]) -> Option<Vec<Vec<u8>>> {
    let mut units = Vec::new();
    let mut index = 0_usize;
    let mut saw_start_code = false;
    while index + 3 <= payload.len() {
        let start_code_len = if payload[index..].starts_with(&[0, 0, 1]) {
            3
        } else if payload[index..].starts_with(&[0, 0, 0, 1]) {
            4
        } else {
            index += 1;
            continue;
        };
        saw_start_code = true;
        index += start_code_len;
        if index >= payload.len() {
            break;
        }
        let nal_start = index;
        let mut nal_end = payload.len();
        let mut scan = index;
        while scan + 3 <= payload.len() {
            if payload[scan..].starts_with(&[0, 0, 1]) || payload[scan..].starts_with(&[0, 0, 0, 1])
            {
                nal_end = scan;
                break;
            }
            scan += 1;
        }
        if nal_end > nal_start {
            units.push(payload[nal_start..nal_end].to_vec());
        }
        index = nal_end;
    }
    if saw_start_code && !units.is_empty() {
        Some(units)
    } else {
        None
    }
}

fn split_avcc_nals(payload: &[u8], nal_length_size: usize) -> Option<Vec<Vec<u8>>> {
    if nal_length_size == 0 || nal_length_size > 4 {
        return None;
    }
    let mut index = 0_usize;
    let mut units = Vec::new();
    while index < payload.len() {
        if index + nal_length_size > payload.len() {
            return None;
        }
        let mut nal_len = 0_usize;
        for byte in &payload[index..index + nal_length_size] {
            nal_len = (nal_len << 8) | usize::from(*byte);
        }
        index += nal_length_size;
        if nal_len == 0 || index + nal_len > payload.len() {
            return None;
        }
        units.push(payload[index..index + nal_len].to_vec());
        index += nal_len;
    }
    if units.is_empty() {
        None
    } else {
        Some(units)
    }
}

fn h264_nal_units(payload: &[u8], nal_length_size: Option<usize>) -> Option<Vec<Vec<u8>>> {
    if payload.is_empty() {
        return None;
    }
    if is_annexb(payload) {
        return split_annexb_nals(payload);
    }
    if let Some(nal_length_size) = nal_length_size {
        if let Some(units) = split_avcc_nals(payload, nal_length_size) {
            return Some(units);
        }
    }
    for nal_length_size in [4_usize, 2, 1] {
        if let Some(units) = split_avcc_nals(payload, nal_length_size) {
            return Some(units);
        }
    }
    None
}

fn h264_nal_types(payload: &[u8], nal_length_size: Option<usize>) -> Vec<u8> {
    h264_nal_units(payload, nal_length_size)
        .map(|units| {
            units
                .into_iter()
                .filter_map(|unit| unit.first().copied().map(|value| value & 0x1F))
                .collect()
        })
        .unwrap_or_default()
}

fn avcc_to_annexb(payload: &[u8], nal_length_size: usize) -> Option<Vec<u8>> {
    if nal_length_size == 0 || nal_length_size > 4 {
        return None;
    }

    let mut offset = 0_usize;
    let mut converted = Vec::with_capacity(payload.len() + 64);

    while offset < payload.len() {
        if offset + nal_length_size > payload.len() {
            return None;
        }

        let mut nal_len = 0_usize;
        for byte in &payload[offset..offset + nal_length_size] {
            nal_len = (nal_len << 8) | usize::from(*byte);
        }
        offset += nal_length_size;

        if nal_len == 0 || offset + nal_len > payload.len() {
            return None;
        }

        converted.extend_from_slice(&[0, 0, 0, 1]);
        converted.extend_from_slice(&payload[offset..offset + nal_len]);
        offset += nal_len;
    }

    if converted.is_empty() {
        return None;
    }
    Some(converted)
}

fn best_effort_avcc_to_annexb(payload: &[u8]) -> Option<Vec<u8>> {
    [4_usize, 2, 1]
        .iter()
        .find_map(|nal_length_size| avcc_to_annexb(payload, *nal_length_size))
}

fn hex_dump_prefix(payload: &[u8], limit: usize) -> String {
    payload
        .iter()
        .take(limit)
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn annexb_to_avcc(payload: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(payload.len() + 32);
    let mut index = 0_usize;
    while index + 3 <= payload.len() {
        let start_code_len = if payload[index..].starts_with(&[0, 0, 1]) {
            3
        } else if payload[index..].starts_with(&[0, 0, 0, 1]) {
            4
        } else {
            index += 1;
            continue;
        };
        index += start_code_len;
        if index >= payload.len() {
            break;
        }
        let nal_start = index;
        let mut nal_end = payload.len();
        let mut scan = index;
        while scan + 3 <= payload.len() {
            if payload[scan..].starts_with(&[0, 0, 1]) || payload[scan..].starts_with(&[0, 0, 0, 1])
            {
                nal_end = scan;
                break;
            }
            scan += 1;
        }
        if nal_end <= nal_start {
            index = nal_end;
            continue;
        }
        let nal_size = nal_end - nal_start;
        let nal_size_u32 = u32::try_from(nal_size).ok()?;
        out.extend_from_slice(&nal_size_u32.to_be_bytes());
        out.extend_from_slice(&payload[nal_start..nal_end]);
        index = nal_end;
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn vt_prepare_sample_payload(payload: &[u8]) -> Option<Vec<u8>> {
    if payload.is_empty() {
        return None;
    }
    if is_annexb(payload) {
        return annexb_to_avcc(payload);
    }
    // Some streams use non-4-byte AVCC length fields. Normalize everything to
    // 4-byte AVCC before handing off to VideoToolbox.
    if avcc_to_annexb(payload, 4).is_some() {
        return Some(payload.to_vec());
    }
    for nal_length_size in [2_usize, 1] {
        if let Some(annexb) = avcc_to_annexb(payload, nal_length_size) {
            if let Some(avcc4) = annexb_to_avcc(&annexb) {
                return Some(avcc4);
            }
        }
    }
    None
}

fn annexb_contains_sps_or_pps(payload: &[u8]) -> bool {
    let mut index = 0_usize;
    while index + 3 <= payload.len() {
        let start_code_len = if payload[index..].starts_with(&[0, 0, 1]) {
            3
        } else if payload[index..].starts_with(&[0, 0, 0, 1]) {
            4
        } else {
            index += 1;
            continue;
        };
        index += start_code_len;
        if index >= payload.len() {
            break;
        }
        let nal_type = payload[index] & 0x1F;
        if nal_type == 7 || nal_type == 8 {
            return true;
        }
    }
    false
}

fn annexb_contains_idr(payload: &[u8]) -> bool {
    let mut index = 0_usize;
    while index + 3 <= payload.len() {
        let start_code_len = if payload[index..].starts_with(&[0, 0, 1]) {
            3
        } else if payload[index..].starts_with(&[0, 0, 0, 1]) {
            4
        } else {
            index += 1;
            continue;
        };
        index += start_code_len;
        if index >= payload.len() {
            break;
        }
        let nal_type = payload[index] & 0x1F;
        if nal_type == 5 {
            return true;
        }
    }
    false
}

fn prepend_parameter_sets(parameter_sets: &[Vec<u8>], payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + parameter_sets.len() * 8);
    for unit in parameter_sets {
        if unit.is_empty() {
            continue;
        }
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(unit);
    }
    out.extend_from_slice(payload);
    out
}

fn extract_annexb_parameter_sets(payload: &[u8]) -> Option<(Vec<Vec<u8>>, usize)> {
    let mut parameter_sets = Vec::new();
    let mut index = 0_usize;
    while index + 3 <= payload.len() {
        let start_code_len = if payload[index..].starts_with(&[0, 0, 1]) {
            3
        } else if payload[index..].starts_with(&[0, 0, 0, 1]) {
            4
        } else {
            index += 1;
            continue;
        };
        index += start_code_len;
        if index >= payload.len() {
            break;
        }
        let nal_start = index;
        let mut nal_end = payload.len();
        let mut scan = index;
        while scan + 3 <= payload.len() {
            if payload[scan..].starts_with(&[0, 0, 1]) || payload[scan..].starts_with(&[0, 0, 0, 1])
            {
                nal_end = scan;
                break;
            }
            scan += 1;
        }
        let nal = &payload[nal_start..nal_end];
        if let Some(nal_type) = nal.first().map(|byte| byte & 0x1F) {
            if nal_type == 7 || nal_type == 8 {
                parameter_sets.push(nal.to_vec());
            }
        }
        index = nal_end;
    }
    if parameter_sets.is_empty() {
        None
    } else {
        Some((parameter_sets, 4))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        annexb_contains_idr, annexb_contains_sps_or_pps, annexb_to_avcc, avcc_to_annexb,
        h264_nal_types, is_annexb, parse_avcc_record, parse_decode_backend_preference,
        preferred_backend_order, select_backend_with, vt_prepare_sample_payload, DecodeBackendKind,
        DecodeBackendPreference, H264Context,
    };

    fn sample_avcc() -> Vec<u8> {
        let sps = [0x67, 0x42, 0x00, 0x1F];
        let pps = [0x68, 0xCE, 0x06, 0xE2];
        let mut avcc = vec![1, 0x42, 0x00, 0x1F, 0xFF, 0xE1];
        avcc.extend_from_slice(&(sps.len() as u16).to_be_bytes());
        avcc.extend_from_slice(&sps);
        avcc.push(1);
        avcc.extend_from_slice(&(pps.len() as u16).to_be_bytes());
        avcc.extend_from_slice(&pps);
        avcc
    }

    #[test]
    fn annexb_input_passes_through() {
        let ctx = H264Context::default();
        let payload = vec![0, 0, 0, 1, 0x65, 0x88];
        let normalized = ctx
            .normalize_for_decode(&payload, true)
            .expect("annexb payload should be preserved");
        assert!(is_annexb(&normalized));
        assert_eq!(normalized, payload);
    }

    #[test]
    fn avcc_payload_converts_to_annexb() {
        let payload = vec![0, 0, 0, 2, 0x65, 0x88, 0, 0, 0, 2, 0x41, 0x99];
        let converted = avcc_to_annexb(&payload, 4).expect("avcc convert");
        assert_eq!(
            converted,
            vec![0, 0, 0, 1, 0x65, 0x88, 0, 0, 0, 1, 0x41, 0x99]
        );
    }

    #[test]
    fn avcc_record_parses_nal_length_and_parameter_sets() {
        let record = sample_avcc();
        let (nal_length_size, parameter_sets, sps, pps) =
            parse_avcc_record(&record).expect("valid avcc record");
        assert_eq!(nal_length_size, 4);
        assert_eq!(parameter_sets.len(), 2);
        assert_eq!(sps.len(), 1);
        assert_eq!(pps.len(), 1);
        assert_eq!(parameter_sets[0][0] & 0x1F, 7);
        assert_eq!(parameter_sets[1][0] & 0x1F, 8);
    }

    #[test]
    fn keyframe_prepends_sps_pps_when_missing() {
        let mut ctx = H264Context::default();
        let record = sample_avcc();
        assert!(ctx.update_from_avcc_record(&record));

        let idr_only_avcc = vec![0, 0, 0, 2, 0x65, 0x88];
        let normalized = ctx
            .normalize_for_decode(&idr_only_avcc, true)
            .expect("normalized keyframe");
        assert!(annexb_contains_sps_or_pps(&normalized));
        assert!(normalized.ends_with(&[0, 0, 0, 1, 0x65, 0x88]));
    }

    #[test]
    fn annexb_payload_converts_to_avcc() {
        let annexb = vec![0, 0, 0, 1, 0x65, 0x88, 0, 0, 1, 0x41, 0x99];
        let avcc = annexb_to_avcc(&annexb).expect("annexb convert");
        assert_eq!(avcc, vec![0, 0, 0, 2, 0x65, 0x88, 0, 0, 0, 2, 0x41, 0x99]);
    }

    #[test]
    fn unknown_non_annexb_payload_is_not_normalized() {
        let ctx = H264Context::default();
        let payload = vec![0xAA, 0xBB, 0xCC, 0xDD, 0xEE];
        assert!(ctx.normalize_for_decode(&payload, false).is_none());
    }

    #[test]
    fn invalid_avcc_payload_is_rejected() {
        let mut ctx = H264Context::default();
        ctx.nal_length_size = Some(4);
        let invalid = vec![0, 0, 0, 5, 0x65, 0x88];
        assert!(ctx.normalize_for_decode(&invalid, true).is_none());
    }

    #[test]
    fn annexb_idr_detection_works() {
        let annexb_idr = vec![0, 0, 0, 1, 0x65, 0x88, 0, 0, 0, 1, 0x41, 0x99];
        assert!(annexb_contains_idr(&annexb_idr));
        let annexb_non_idr = vec![0, 0, 0, 1, 0x41, 0x88];
        assert!(!annexb_contains_idr(&annexb_non_idr));
    }

    #[test]
    fn payload_idr_detection_supports_avcc() {
        let mut ctx = H264Context::default();
        ctx.nal_length_size = Some(4);
        let avcc_idr = vec![0, 0, 0, 2, 0x65, 0x88];
        assert!(ctx.payload_contains_idr(&avcc_idr));
        let avcc_non_idr = vec![0, 0, 0, 2, 0x41, 0x88];
        assert!(!ctx.payload_contains_idr(&avcc_non_idr));
    }

    #[test]
    fn payload_parameter_set_detection_supports_avcc() {
        let mut ctx = H264Context::default();
        ctx.nal_length_size = Some(4);
        let avcc_sps = vec![0, 0, 0, 2, 0x67, 0x88];
        assert!(ctx.payload_contains_parameter_sets(&avcc_sps));
        let avcc_non_param = vec![0, 0, 0, 2, 0x41, 0x88];
        assert!(!ctx.payload_contains_parameter_sets(&avcc_non_param));
    }

    #[test]
    fn nal_type_summary_supports_annexb_and_avcc() {
        let annexb = vec![0, 0, 0, 1, 0x67, 0x88, 0, 0, 0, 1, 0x65, 0x99];
        assert_eq!(h264_nal_types(&annexb, None), vec![7, 5]);
        let avcc = vec![0, 0, 0, 2, 0x67, 0x88, 0, 0, 0, 2, 0x65, 0x99];
        assert_eq!(h264_nal_types(&avcc, Some(4)), vec![7, 5]);
    }

    #[test]
    fn vt_prepare_accepts_annexb() {
        let annexb = vec![0, 0, 0, 1, 0x65, 0x88];
        let avcc = vt_prepare_sample_payload(&annexb).expect("vt sample payload");
        assert_eq!(avcc, vec![0, 0, 0, 2, 0x65, 0x88]);
    }

    #[test]
    fn vt_prepare_normalizes_two_byte_avcc_to_four_byte() {
        let two_byte_avcc = vec![0, 2, 0x65, 0x88];
        let normalized = vt_prepare_sample_payload(&two_byte_avcc).expect("vt sample payload");
        assert_eq!(normalized, vec![0, 0, 0, 2, 0x65, 0x88]);
    }

    #[test]
    fn parse_decode_backend_switches() {
        assert_eq!(
            parse_decode_backend_preference("auto"),
            Some(DecodeBackendPreference::Auto)
        );
        assert_eq!(
            parse_decode_backend_preference("mf"),
            Some(DecodeBackendPreference::Mf)
        );
        assert_eq!(
            parse_decode_backend_preference("openh264"),
            Some(DecodeBackendPreference::OpenH264)
        );
        assert_eq!(parse_decode_backend_preference("unknown"), None);
    }

    #[test]
    fn backend_order_for_auto_is_expected() {
        let order = preferred_backend_order(DecodeBackendPreference::Auto);
        #[cfg(target_os = "windows")]
        assert_eq!(
            order,
            vec![DecodeBackendKind::MfD3d11, DecodeBackendKind::OpenH264]
        );
        #[cfg(target_os = "macos")]
        assert_eq!(
            order,
            vec![DecodeBackendKind::VideoToolbox, DecodeBackendKind::OpenH264]
        );
        #[cfg(not(target_os = "windows"))]
        #[cfg(not(target_os = "macos"))]
        assert_eq!(order, vec![DecodeBackendKind::OpenH264]);
    }

    #[test]
    fn fallback_selects_openh264_when_mf_unavailable() {
        let selected = select_backend_with(DecodeBackendPreference::Auto, |backend| {
            backend == DecodeBackendKind::OpenH264
        });
        assert_eq!(selected, Some(DecodeBackendKind::OpenH264));
    }
}

struct OutputBackend {
    mode: OutputMode,
    #[cfg(target_os = "windows")]
    spout: HashMap<u32, *mut BrowserPortSpoutSender>,
    #[cfg(target_os = "windows")]
    spout_last_warn: HashMap<u32, Instant>,
    #[cfg(target_os = "windows")]
    spout_last_send: HashMap<u32, Instant>,
    #[cfg(target_os = "windows")]
    spout_has_real_frame: HashMap<u32, bool>,
    #[cfg(target_os = "windows")]
    spout_device_key: HashMap<u32, usize>,
    #[cfg(target_os = "windows")]
    spout_dimensions: HashMap<u32, (usize, usize)>,
    #[cfg(target_os = "macos")]
    syphon: HashMap<u32, *mut BrowserPortSyphonSender>,
    #[cfg(target_os = "macos")]
    syphon_last_discovery_publish: HashMap<u32, Instant>,
    #[cfg(target_os = "macos")]
    syphon_has_real_frame: HashMap<u32, bool>,
    ndi: Option<NdiState>,
}

impl OutputBackend {
    #[cfg(target_os = "macos")]
    fn static_syphon_senders_disabled() -> bool {
        matches!(
            std::env::var("BROWSER_PORT_DISABLE_STATIC_SYPHON_SENDERS")
                .ok()
                .as_deref(),
            Some("1")
                | Some("true")
                | Some("TRUE")
                | Some("yes")
                | Some("YES")
                | Some("on")
                | Some("ON")
        )
    }

    fn new(mode: OutputMode) -> anyhow::Result<Self> {
        match mode {
            OutputMode::Spout => {
                #[cfg(target_os = "windows")]
                {
                    Ok(Self {
                        mode,
                        spout: HashMap::new(),
                        spout_last_warn: HashMap::new(),
                        spout_last_send: HashMap::new(),
                        spout_has_real_frame: HashMap::new(),
                        spout_device_key: HashMap::new(),
                        spout_dimensions: HashMap::new(),
                        ndi: None,
                    })
                }
                #[cfg(not(target_os = "windows"))]
                {
                    bail!("spout mode is only available on windows")
                }
            }
            OutputMode::Syphon => {
                #[cfg(target_os = "macos")]
                {
                    let mut backend = Self {
                        mode,
                        syphon: HashMap::new(),
                        syphon_last_discovery_publish: HashMap::new(),
                        syphon_has_real_frame: HashMap::new(),
                        ndi: None,
                    };
                    if Self::static_syphon_senders_disabled() {
                        eprintln!(
                            "output-helper: static syphon sender prime disabled via BROWSER_PORT_DISABLE_STATIC_SYPHON_SENDERS"
                        );
                    } else {
                        backend.ensure_static_syphon_senders();
                    }
                    Ok(backend)
                }
                #[cfg(not(target_os = "macos"))]
                {
                    bail!("syphon mode is only available on macos")
                }
            }
            OutputMode::Ndi => {
                #[cfg(any(target_os = "windows", target_os = "linux"))]
                {
                    Ok(Self {
                        mode,
                        #[cfg(target_os = "windows")]
                        spout: HashMap::new(),
                        #[cfg(target_os = "windows")]
                        spout_last_warn: HashMap::new(),
                        #[cfg(target_os = "windows")]
                        spout_last_send: HashMap::new(),
                        #[cfg(target_os = "windows")]
                        spout_has_real_frame: HashMap::new(),
                        #[cfg(target_os = "windows")]
                        spout_device_key: HashMap::new(),
                        #[cfg(target_os = "windows")]
                        spout_dimensions: HashMap::new(),
                        #[cfg(target_os = "macos")]
                        syphon: HashMap::new(),
                        #[cfg(target_os = "macos")]
                        syphon_last_discovery_publish: HashMap::new(),
                        #[cfg(target_os = "macos")]
                        syphon_has_real_frame: HashMap::new(),
                        ndi: Some(NdiState::new().context("failed to init ndi state")?),
                    })
                }
                #[cfg(not(any(target_os = "windows", target_os = "linux")))]
                {
                    bail!("ndi mode is not available on this platform in current build")
                }
            }
        }
    }

    fn send_video(&mut self, player_id: u32, frame: &DecodedFrame) -> VideoSendResult {
        match self.mode {
            OutputMode::Spout => {
                #[cfg(target_os = "windows")]
                {
                    self.send_spout(player_id, frame)
                }
                #[cfg(not(target_os = "windows"))]
                {
                    VideoSendResult::not_sent()
                }
            }
            OutputMode::Syphon => {
                #[cfg(target_os = "macos")]
                {
                    self.send_syphon(player_id, frame)
                }
                #[cfg(not(target_os = "macos"))]
                {
                    VideoSendResult::not_sent()
                }
            }
            OutputMode::Ndi => {
                if let Some(state) = self.ndi.as_mut() {
                    state.send_video(player_id, frame.width, frame.height, &frame.bgra);
                    VideoSendResult {
                        sent: true,
                        path: SendPath::Ndi,
                        spout_bridge_metrics: None,
                        texture_attempted: false,
                        texture_failed: false,
                    }
                } else {
                    VideoSendResult::not_sent()
                }
            }
        }
    }

    fn configure_player(
        &mut self,
        _player_id: u32,
        _coded_width: Option<usize>,
        _coded_height: Option<usize>,
    ) -> Option<*mut std::ffi::c_void> {
        match self.mode {
            OutputMode::Spout => {
                #[cfg(target_os = "windows")]
                {
                    return self.prime_spout_sender(_player_id, _coded_width, _coded_height);
                }
                #[cfg(not(target_os = "windows"))]
                {
                    None
                }
            }
            OutputMode::Syphon => {
                #[cfg(target_os = "macos")]
                {
                    self.prime_syphon_sender(_player_id, _coded_width, _coded_height);
                }
                None
            }
            OutputMode::Ndi => None,
        }
    }

    fn tick(&mut self) {
        #[cfg(target_os = "windows")]
        {
            if self.mode == OutputMode::Spout {
                self.send_spout_keepalive();
            }
        }
        #[cfg(target_os = "macos")]
        {
            if self.mode == OutputMode::Syphon {
                if !Self::static_syphon_senders_disabled() {
                    self.keepalive_static_syphon_senders();
                }
            }
        }
    }

    fn clear_player(&mut self, player_id: u32, _reason: &str) {
        match self.mode {
            OutputMode::Spout => {
                #[cfg(target_os = "windows")]
                {
                    if let Some(sender) = self.spout.remove(&player_id) {
                        if !sender.is_null() {
                            unsafe { browser_port_spout_destroy_sender(sender) };
                        }
                    }
                    self.spout_last_warn.remove(&player_id);
                    self.spout_last_send.remove(&player_id);
                    self.spout_has_real_frame.remove(&player_id);
                    self.spout_device_key.remove(&player_id);
                    self.spout_dimensions.remove(&player_id);
                    eprintln!(
                        "output-helper: cleared spout sender player={} reason={}",
                        player_id, _reason
                    );
                }
            }
            OutputMode::Syphon => {
                #[cfg(target_os = "macos")]
                {
                    if self.is_static_syphon_player(player_id) {
                        return;
                    }
                    if let Some(sender) = self.syphon.remove(&player_id) {
                        if !sender.is_null() {
                            unsafe { browser_port_syphon_destroy_sender(sender) };
                        }
                    }
                    self.syphon_last_discovery_publish.remove(&player_id);
                    self.syphon_has_real_frame.remove(&player_id);
                }
            }
            OutputMode::Ndi => {}
        }
    }

    fn send_audio_ndi(&mut self, player_id: u32, payload: &[u8]) {
        if self.mode != OutputMode::Ndi {
            return;
        }
        if let Some(state) = self.ndi.as_mut() {
            state.send_audio(player_id, payload);
        }
    }

    #[cfg(target_os = "windows")]
    fn default_spout_dimensions(
        &self,
        coded_width: Option<usize>,
        coded_height: Option<usize>,
    ) -> (usize, usize) {
        let width = coded_width.filter(|v| *v > 0).unwrap_or(1280);
        let height = coded_height.filter(|v| *v > 0).unwrap_or(720);
        (width, height)
    }

    #[cfg(target_os = "windows")]
    fn ensure_spout_sender(
        &mut self,
        player_id: u32,
        device_ptr: Option<*mut std::ffi::c_void>,
    ) -> *mut BrowserPortSpoutSender {
        let desired_device_key = device_ptr.map(|ptr| ptr as usize).unwrap_or(0);
        let current_device_key = self.spout_device_key.get(&player_id).copied().unwrap_or(0);
        if let Some(sender) = self.spout.get(&player_id).copied() {
            if sender.is_null() {
                self.spout.remove(&player_id);
                self.spout_device_key.remove(&player_id);
            } else if desired_device_key == 0 || desired_device_key == current_device_key {
                return sender;
            } else {
                let actual_device_key =
                    unsafe { browser_port_spout_sender_device(sender) as usize };
                if actual_device_key != 0 && actual_device_key == desired_device_key {
                    self.spout_device_key.insert(player_id, actual_device_key);
                    return sender;
                }
                unsafe { browser_port_spout_destroy_sender(sender) };
                self.spout.remove(&player_id);
                self.spout_device_key.remove(&player_id);
            }
        }
        let name = CString::new(format!("browser-port-spout-{player_id}")).expect("valid cstring");
        let mut sender = unsafe {
            if let Some(device_ptr) = device_ptr {
                browser_port_spout_create_sender_with_device(name.as_ptr(), device_ptr)
            } else {
                browser_port_spout_create_sender(name.as_ptr())
            }
        };
        if sender.is_null() && device_ptr.is_some() {
            sender = unsafe { browser_port_spout_create_sender(name.as_ptr()) };
            if !sender.is_null() {
                eprintln!(
                    "output-helper: spout sender fallback to default device player={}",
                    player_id
                );
            }
        }
        if sender.is_null() {
            self.spout.remove(&player_id);
            self.spout_device_key.remove(&player_id);
            return ptr::null_mut();
        }
        self.spout.insert(player_id, sender);
        let actual_device_key = unsafe { browser_port_spout_sender_device(sender) as usize };
        let stored_device_key = if actual_device_key != 0 {
            actual_device_key
        } else {
            desired_device_key
        };
        self.spout_device_key.insert(player_id, stored_device_key);
        sender
    }

    #[cfg(target_os = "windows")]
    fn prime_spout_sender(
        &mut self,
        player_id: u32,
        coded_width: Option<usize>,
        coded_height: Option<usize>,
    ) -> Option<*mut std::ffi::c_void> {
        let (width, height) = self.default_spout_dimensions(coded_width, coded_height);
        self.spout_dimensions.insert(player_id, (width, height));
        self.spout_has_real_frame.insert(player_id, false);
        let sender = self.ensure_spout_sender(player_id, None);
        if sender.is_null() {
            self.warn_spout_send_failure(player_id, width, height, "sender initialization failed");
            return None;
        }
        let mut black = vec![0_u8; width.saturating_mul(height).saturating_mul(4)];
        let sent = unsafe {
            browser_port_spout_send_bgra(
                sender,
                black.as_mut_ptr(),
                width.try_into().unwrap_or(u32::MAX),
                height.try_into().unwrap_or(u32::MAX),
            )
        };
        if sent {
            self.spout_last_send.insert(player_id, Instant::now());
            eprintln!(
                "output-helper: spout sender primed player={} size={}x{}",
                player_id, width, height
            );
        } else {
            self.warn_spout_send_failure(player_id, width, height, "prime send failed");
        }
        let device = unsafe { browser_port_spout_sender_device(sender) };
        if device.is_null() {
            None
        } else {
            Some(device)
        }
    }

    #[cfg(target_os = "windows")]
    fn send_spout_keepalive(&mut self) {
        let now = Instant::now();
        let players = self.spout.keys().copied().collect::<Vec<_>>();
        for player_id in players {
            if self
                .spout_has_real_frame
                .get(&player_id)
                .copied()
                .unwrap_or(false)
            {
                continue;
            }
            let Some(last_send) = self.spout_last_send.get(&player_id).copied() else {
                continue;
            };
            if now.duration_since(last_send) < SPOUT_KEEPALIVE_INTERVAL {
                continue;
            }
            let (width, height) = self
                .spout_dimensions
                .get(&player_id)
                .copied()
                .unwrap_or((1280, 720));
            let sender = self.ensure_spout_sender(player_id, None);
            if sender.is_null() {
                self.warn_spout_send_failure(player_id, width, height, "keepalive sender missing");
                continue;
            }
            let mut black = vec![0_u8; width.saturating_mul(height).saturating_mul(4)];
            let sent = unsafe {
                browser_port_spout_send_bgra(
                    sender,
                    black.as_mut_ptr(),
                    width.try_into().unwrap_or(u32::MAX),
                    height.try_into().unwrap_or(u32::MAX),
                )
            };
            if sent {
                self.spout_last_send.insert(player_id, now);
            } else {
                self.warn_spout_send_failure(player_id, width, height, "keepalive send failed");
            }
        }
    }

    #[cfg(target_os = "windows")]
    fn send_spout(&mut self, player_id: u32, frame: &DecodedFrame) -> VideoSendResult {
        let width = frame.width;
        let height = frame.height;
        let sender = self.ensure_spout_sender(player_id, frame.dx11_device);
        if sender.is_null() {
            self.warn_spout_send_failure(player_id, width, height, "sender initialization failed");
            return VideoSendResult::not_sent();
        }
        if width == 0 || height == 0 {
            self.warn_spout_send_failure(player_id, width, height, "invalid frame size");
            return VideoSendResult::not_sent();
        }
        self.spout_dimensions.insert(player_id, (width, height));

        let mut result = VideoSendResult::not_sent();
        if let Some(texture) = frame.dx11_texture {
            result.texture_attempted = true;
            let sent_texture = unsafe { browser_port_spout_send_dx11_texture(sender, texture) };
            let bridge_metrics = self.read_spout_bridge_metrics(sender);
            result.spout_bridge_metrics = bridge_metrics;
            if sent_texture {
                let first_real_frame = !self
                    .spout_has_real_frame
                    .get(&player_id)
                    .copied()
                    .unwrap_or(false);
                self.spout_last_send.insert(player_id, Instant::now());
                self.spout_has_real_frame.insert(player_id, true);
                if first_real_frame {
                    if let Some((mean_luma, non_black_ratio, first)) = bgra_frame_stats(&frame.bgra)
                    {
                        eprintln!(
                            "output-helper: spout real frame sent player={} size={}x{} path=texture mean_luma={:.2} non_black_ratio={:.4} first_bgra={:02X}{:02X}{:02X}{:02X}",
                            player_id,
                            width,
                            height,
                            mean_luma,
                            non_black_ratio,
                            first[0],
                            first[1],
                            first[2],
                            first[3]
                        );
                    } else {
                        eprintln!(
                            "output-helper: spout real frame sent player={} size={}x{} path=texture bgra=empty",
                            player_id, width, height
                        );
                    }
                }
                result.sent = true;
                result.path = SendPath::Texture;
                return result;
            }
            result.texture_failed = true;
            self.warn_spout_send_failure(player_id, width, height, "native bridge texture failed");
        }

        if frame.bgra.is_empty() {
            self.warn_spout_send_failure(player_id, width, height, "empty BGRA payload");
            return result;
        }
        let sent = unsafe {
            browser_port_spout_send_bgra(
                sender,
                frame.bgra.as_ptr(),
                width.try_into().unwrap_or(u32::MAX),
                height.try_into().unwrap_or(u32::MAX),
            )
        };
        let bridge_metrics = self.read_spout_bridge_metrics(sender);
        if !sent {
            self.warn_spout_send_failure(player_id, width, height, "native bridge send failed");
        } else {
            let first_real_frame = !self
                .spout_has_real_frame
                .get(&player_id)
                .copied()
                .unwrap_or(false);
            self.spout_last_send.insert(player_id, Instant::now());
            self.spout_has_real_frame.insert(player_id, true);
            if first_real_frame {
                if let Some((mean_luma, non_black_ratio, first)) = bgra_frame_stats(&frame.bgra) {
                    eprintln!(
                        "output-helper: spout real frame sent player={} size={}x{} path=bgra mean_luma={:.2} non_black_ratio={:.4} first_bgra={:02X}{:02X}{:02X}{:02X}",
                        player_id,
                        width,
                        height,
                        mean_luma,
                        non_black_ratio,
                        first[0],
                        first[1],
                        first[2],
                        first[3]
                    );
                } else {
                    eprintln!(
                        "output-helper: spout real frame sent player={} size={}x{} path=bgra bgra=empty",
                        player_id, width, height
                    );
                }
            }
        }
        result.sent = sent;
        result.path = SendPath::Bgra;
        result.spout_bridge_metrics = bridge_metrics;
        result
    }

    #[cfg(target_os = "windows")]
    fn read_spout_bridge_metrics(
        &self,
        sender: *mut BrowserPortSpoutSender,
    ) -> Option<SpoutBridgeMetrics> {
        let mut swap_ms = 0.0_f64;
        let mut upload_ms = 0.0_f64;
        let mut send_ms = 0.0_f64;
        let mut total_ms = 0.0_f64;
        let ok = unsafe {
            browser_port_spout_sender_take_last_send_metrics(
                sender,
                &mut swap_ms,
                &mut upload_ms,
                &mut send_ms,
                &mut total_ms,
            )
        };
        if ok {
            Some(SpoutBridgeMetrics {
                swap_ms,
                upload_ms,
                send_ms,
                total_ms,
            })
        } else {
            None
        }
    }

    #[cfg(target_os = "windows")]
    fn warn_spout_send_failure(
        &mut self,
        player_id: u32,
        width: usize,
        height: usize,
        reason: &str,
    ) {
        let native_reason = unsafe {
            let ptr = browser_port_spout_last_error();
            if ptr.is_null() {
                None
            } else {
                CStr::from_ptr(ptr).to_str().ok()
            }
        };
        let now = Instant::now();
        let should_log = match self.spout_last_warn.get(&player_id) {
            Some(last) => now.duration_since(*last) >= SPOUT_SEND_WARN_INTERVAL,
            None => true,
        };
        if !should_log {
            return;
        }
        self.spout_last_warn.insert(player_id, now);
        eprintln!(
            "output-helper: spout send failed player={} size={}x{} reason={} native={}",
            player_id,
            width,
            height,
            reason,
            native_reason.unwrap_or("(none)")
        );
    }

    #[cfg(target_os = "macos")]
    fn is_static_syphon_player(&self, player_id: u32) -> bool {
        SYPHON_STATIC_PLAYER_IDS.contains(&player_id)
    }

    #[cfg(target_os = "macos")]
    fn ensure_static_syphon_senders(&mut self) {
        if Self::static_syphon_senders_disabled() {
            return;
        }
        for player_id in SYPHON_STATIC_PLAYER_IDS {
            self.prime_syphon_sender(player_id, Some(640), Some(360));
        }
    }

    #[cfg(target_os = "macos")]
    fn keepalive_static_syphon_senders(&mut self) {
        if Self::static_syphon_senders_disabled() {
            return;
        }
        for player_id in SYPHON_STATIC_PLAYER_IDS {
            if self
                .syphon_has_real_frame
                .get(&player_id)
                .copied()
                .unwrap_or(false)
            {
                continue;
            }
            let should_publish = self
                .syphon_last_discovery_publish
                .get(&player_id)
                .map(|last| last.elapsed() >= Duration::from_millis(1000))
                .unwrap_or(true);
            if should_publish {
                self.prime_syphon_sender(player_id, Some(640), Some(360));
            }
        }
    }

    #[cfg(target_os = "macos")]
    fn ensure_syphon_sender(&mut self, player_id: u32) -> *mut BrowserPortSyphonSender {
        if let Some(existing) = self.syphon.get(&player_id).copied() {
            if !existing.is_null() {
                return existing;
            }
        }
        let name = CString::new(format!("Player {player_id}")).expect("valid cstring");
        let sender = unsafe { browser_port_syphon_create_sender(name.as_ptr()) };
        if sender.is_null() {
            eprintln!("output-helper: syphon sender create returned null player={player_id}");
        }
        self.syphon.insert(player_id, sender);
        sender
    }

    #[cfg(target_os = "macos")]
    fn prime_syphon_sender(
        &mut self,
        player_id: u32,
        coded_width: Option<usize>,
        coded_height: Option<usize>,
    ) {
        let sender = self.ensure_syphon_sender(player_id);
        if sender.is_null() {
            let native_reason = unsafe {
                let ptr = browser_port_syphon_last_error();
                if ptr.is_null() {
                    None
                } else {
                    CStr::from_ptr(ptr).to_str().ok()
                }
            };
            eprintln!(
                "output-helper: syphon sender create failed player={} reason={}",
                player_id,
                native_reason.unwrap_or("(none)")
            );
            return;
        }
        let width = coded_width.filter(|v| *v > 0).unwrap_or(640);
        let height = coded_height.filter(|v| *v > 0).unwrap_or(360);
        let mut black = vec![0_u8; width.saturating_mul(height).saturating_mul(4)];
        let sent = unsafe {
            browser_port_syphon_send_bgra(
                sender,
                black.as_mut_ptr(),
                width.try_into().unwrap_or(u32::MAX),
                height.try_into().unwrap_or(u32::MAX),
            )
        };
        if sent {
            self.syphon_last_discovery_publish
                .insert(player_id, Instant::now());
            self.syphon_has_real_frame.entry(player_id).or_insert(false);
        } else {
            let native_reason = unsafe {
                let ptr = browser_port_syphon_last_error();
                if ptr.is_null() {
                    None
                } else {
                    CStr::from_ptr(ptr).to_str().ok()
                }
            };
            eprintln!(
                "output-helper: syphon sender prime failed player={} size={}x{} reason={}",
                player_id,
                width,
                height,
                native_reason.unwrap_or("(none)")
            );
        }
    }

    #[cfg(target_os = "macos")]
    fn send_syphon(&mut self, player_id: u32, frame: &DecodedFrame) -> VideoSendResult {
        let width = frame.width;
        let height = frame.height;
        if width == 0 || height == 0 {
            return VideoSendResult::not_sent();
        }
        let sender_was_cached = self
            .syphon
            .get(&player_id)
            .copied()
            .map(|sender| !sender.is_null())
            .unwrap_or(false);
        let sender = self.ensure_syphon_sender(player_id);
        if sender.is_null() {
            return VideoSendResult::not_sent();
        }
        if let Some(pixel_buffer) = frame.cv_pixel_buffer.as_ref() {
            let pixel_format = unsafe { CVPixelBufferGetPixelFormatType(pixel_buffer.as_raw()) };
            let plane_count = unsafe { CVPixelBufferGetPlaneCount(pixel_buffer.as_raw()) };
            let first_real_frame = !self
                .syphon_has_real_frame
                .get(&player_id)
                .copied()
                .unwrap_or(false);
            let sent =
                unsafe { browser_port_syphon_send_cv_pixel_buffer(sender, pixel_buffer.as_raw()) };
            if !sent {
                let native_reason = unsafe {
                    let ptr = browser_port_syphon_last_error();
                    if ptr.is_null() {
                        None
                    } else {
                        CStr::from_ptr(ptr).to_str().ok()
                    }
                };
                eprintln!(
                    "output-helper: syphon metal send failed player={} size={}x{} reason={}",
                    player_id,
                    width,
                    height,
                    native_reason.unwrap_or("(none)")
                );
                return VideoSendResult::not_sent();
            }
            if sent {
                self.syphon_last_discovery_publish
                    .insert(player_id, Instant::now());
                self.syphon_has_real_frame.insert(player_id, true);
            }
            if first_real_frame {
                eprintln!(
                    "output-helper: syphon first cv frame player={} size={}x{} pixel_format=0x{pixel_format:08x} plane_count={} sender_reused={}",
                    player_id, width, height, plane_count, sender_was_cached
                );
            }
            return VideoSendResult {
                sent,
                path: SendPath::SyphonMetalTexture,
                spout_bridge_metrics: None,
                texture_attempted: true,
                texture_failed: false,
            };
        }
        if frame.bgra.is_empty() {
            return VideoSendResult::not_sent();
        }
        let sent = unsafe {
            browser_port_syphon_send_bgra(
                sender,
                frame.bgra.as_ptr(),
                width.try_into().unwrap_or(u32::MAX),
                height.try_into().unwrap_or(u32::MAX),
            )
        };
        if !sent {
            let native_reason = unsafe {
                let ptr = browser_port_syphon_last_error();
                if ptr.is_null() {
                    None
                } else {
                    CStr::from_ptr(ptr).to_str().ok()
                }
            };
            eprintln!(
                "output-helper: syphon send failed player={} size={}x{} reason={}",
                player_id,
                width,
                height,
                native_reason.unwrap_or("(none)")
            );
            return VideoSendResult::not_sent();
        }
        self.syphon_last_discovery_publish
            .insert(player_id, Instant::now());
        let first_real_frame = !self
            .syphon_has_real_frame
            .get(&player_id)
            .copied()
            .unwrap_or(false);
        self.syphon_has_real_frame.insert(player_id, true);
        if first_real_frame {
            eprintln!(
                "output-helper: syphon first bgra frame player={} size={}x{} sender_reused={}",
                player_id, width, height, sender_was_cached
            );
        }
        VideoSendResult {
            sent,
            path: SendPath::SyphonBgra,
            spout_bridge_metrics: None,
            texture_attempted: false,
            texture_failed: false,
        }
    }

    #[cfg(target_os = "macos")]
    fn syphon_client_count(&self) -> usize {
        self.syphon
            .values()
            .map(|sender| unsafe { browser_port_syphon_client_count(*sender) as usize })
            .sum()
    }

    #[cfg(not(target_os = "macos"))]
    fn syphon_client_count(&self) -> usize {
        0
    }
}

impl Drop for OutputBackend {
    fn drop(&mut self) {
        #[cfg(target_os = "windows")]
        {
            for sender in self.spout.values() {
                unsafe { browser_port_spout_destroy_sender(*sender) };
            }
            self.spout.clear();
            self.spout_last_warn.clear();
            self.spout_last_send.clear();
            self.spout_has_real_frame.clear();
            self.spout_device_key.clear();
            self.spout_dimensions.clear();
        }
        #[cfg(target_os = "macos")]
        {
            for sender in self.syphon.values() {
                if !sender.is_null() {
                    eprintln!("output-helper: syphon sender destroy");
                }
                unsafe { browser_port_syphon_destroy_sender(*sender) };
            }
            self.syphon.clear();
            self.syphon_last_discovery_publish.clear();
            self.syphon_has_real_frame.clear();
        }
    }
}

#[cfg(any(target_os = "windows", target_os = "linux"))]
struct NdiRawSender {
    ptr: ndi::internal::bindings::NDIlib_send_instance_t,
    _name: CString,
}

#[cfg(any(target_os = "windows", target_os = "linux"))]
struct NdiState {
    senders: HashMap<u32, NdiRawSender>,
}

#[cfg(any(target_os = "windows", target_os = "linux"))]
impl NdiState {
    fn new() -> anyhow::Result<Self> {
        if !unsafe { ndi::internal::bindings::NDIlib_initialize() } {
            bail!("NDIlib_initialize failed");
        }
        Ok(Self {
            senders: HashMap::new(),
        })
    }

    fn get_sender(
        &mut self,
        player_id: u32,
    ) -> Option<&ndi::internal::bindings::NDIlib_send_instance_t> {
        if !self.senders.contains_key(&player_id) {
            let name = CString::new(format!("browser-port-ndi-{player_id}")).ok()?;
            let settings = ndi::internal::bindings::NDIlib_send_create_t {
                p_ndi_name: name.as_ptr(),
                p_groups: ptr::null(),
                clock_video: true,
                clock_audio: true,
            };
            let ptr = unsafe { ndi::internal::bindings::NDIlib_send_create(&settings) };
            if ptr.is_null() {
                return None;
            }
            self.senders
                .insert(player_id, NdiRawSender { ptr, _name: name });
        }
        self.senders.get(&player_id).map(|s| &s.ptr)
    }

    fn send_video(&mut self, player_id: u32, width: usize, height: usize, bgra: &[u8]) {
        let Some(sender) = self.get_sender(player_id) else {
            return;
        };
        let mut bgrx = bgra.to_vec();
        for px in bgrx.chunks_exact_mut(4) {
            px[3] = 255;
        }
        let mut frame = ndi::internal::bindings::NDIlib_video_frame_v2_t {
            xres: width as i32,
            yres: height as i32,
            FourCC: ndi::internal::bindings::NDIlib_FourCC_video_type_e_NDIlib_FourCC_type_BGRX,
            frame_rate_N: 60,
            frame_rate_D: 1,
            picture_aspect_ratio: width as f32 / height as f32,
            frame_format_type:
                ndi::internal::bindings::NDIlib_frame_format_type_e_NDIlib_frame_format_type_progressive,
            timecode: 0,
            p_data: bgrx.as_mut_ptr(),
            __bindgen_anon_1: ndi::internal::bindings::NDIlib_video_frame_v2_t__bindgen_ty_1 {
                line_stride_in_bytes: (width * 4) as i32,
            },
            p_metadata: ptr::null(),
            timestamp: ndi::internal::bindings::NDIlib_recv_timestamp_undefined,
        };
        unsafe {
            ndi::internal::bindings::NDIlib_send_send_video_v2(*sender, &mut frame);
        }
    }

    fn send_audio(&mut self, player_id: u32, payload: &[u8]) {
        if payload.len() < 8 {
            return;
        }
        let sample_rate = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
        let channels = u16::from_le_bytes([payload[4], payload[5]]) as usize;
        let frame_count = u16::from_le_bytes([payload[6], payload[7]]) as usize;
        if sample_rate == 0 || channels == 0 || frame_count == 0 {
            return;
        }
        let expected_bytes = frame_count.saturating_mul(channels).saturating_mul(4);
        if payload.len() < 8 + expected_bytes {
            return;
        }
        let Some(sender) = self.get_sender(player_id) else {
            return;
        };
        let interleaved_bytes = &payload[8..8 + expected_bytes];
        let mut planar = vec![0_f32; frame_count * channels];
        for i in 0..frame_count {
            for ch in 0..channels {
                let src_index = (i * channels + ch) * 4;
                let sample = f32::from_le_bytes([
                    interleaved_bytes[src_index],
                    interleaved_bytes[src_index + 1],
                    interleaved_bytes[src_index + 2],
                    interleaved_bytes[src_index + 3],
                ]);
                planar[ch * frame_count + i] = sample;
            }
        }
        let mut frame = ndi::internal::bindings::NDIlib_audio_frame_v2_t {
            sample_rate: sample_rate as i32,
            no_channels: channels as i32,
            no_samples: frame_count as i32,
            timecode: 0,
            p_data: planar.as_mut_ptr(),
            channel_stride_in_bytes: (frame_count * 4) as i32,
            p_metadata: ptr::null(),
            timestamp: ndi::internal::bindings::NDIlib_recv_timestamp_undefined,
        };
        unsafe {
            ndi::internal::bindings::NDIlib_send_send_audio_v2(*sender, &mut frame);
        }
    }
}

#[cfg(any(target_os = "windows", target_os = "linux"))]
impl Drop for NdiState {
    fn drop(&mut self) {
        for sender in self.senders.values() {
            unsafe { ndi::internal::bindings::NDIlib_send_destroy(sender.ptr) };
        }
        self.senders.clear();
        unsafe { ndi::internal::bindings::NDIlib_destroy() };
    }
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
struct NdiState;

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
impl NdiState {
    #[allow(dead_code)]
    fn new() -> anyhow::Result<Self> {
        bail!("NDI is not available on this platform")
    }

    fn send_video(&mut self, _player_id: u32, _width: usize, _height: usize, _bgra: &[u8]) {}

    fn send_audio(&mut self, _player_id: u32, _payload: &[u8]) {}
}

#[cfg(target_os = "windows")]
#[repr(C)]
struct BrowserPortSpoutSender {
    _private: [u8; 0],
}

#[cfg(target_os = "windows")]
unsafe extern "C" {
    fn browser_port_spout_create_sender(name: *const c_char) -> *mut BrowserPortSpoutSender;
    fn browser_port_spout_create_sender_with_device(
        name: *const c_char,
        device: *mut std::ffi::c_void,
    ) -> *mut BrowserPortSpoutSender;
    fn browser_port_spout_send_bgra(
        sender: *mut BrowserPortSpoutSender,
        bgra: *const u8,
        width: u32,
        height: u32,
    ) -> bool;
    fn browser_port_spout_send_dx11_texture(
        sender: *mut BrowserPortSpoutSender,
        texture: *mut std::ffi::c_void,
    ) -> bool;
    fn browser_port_spout_last_error() -> *const c_char;
    fn browser_port_spout_sender_device(
        sender: *mut BrowserPortSpoutSender,
    ) -> *mut std::ffi::c_void;
    fn browser_port_spout_sender_take_last_send_metrics(
        sender: *mut BrowserPortSpoutSender,
        swap_ms: *mut f64,
        upload_ms: *mut f64,
        send_ms: *mut f64,
        total_ms: *mut f64,
    ) -> bool;
    fn browser_port_spout_destroy_sender(sender: *mut BrowserPortSpoutSender);
}

#[cfg(target_os = "macos")]
#[repr(C)]
struct BrowserPortSyphonSender {
    _private: [u8; 0],
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn browser_port_syphon_create_sender(name: *const c_char) -> *mut BrowserPortSyphonSender;
    fn browser_port_syphon_send_bgra(
        sender: *mut BrowserPortSyphonSender,
        bgra: *const u8,
        width: u32,
        height: u32,
    ) -> bool;
    fn browser_port_syphon_send_cv_pixel_buffer(
        sender: *mut BrowserPortSyphonSender,
        pixel_buffer: *mut std::ffi::c_void,
    ) -> bool;
    fn browser_port_syphon_last_error() -> *const c_char;
    fn browser_port_syphon_client_count(sender: *mut BrowserPortSyphonSender) -> usize;
    fn browser_port_syphon_destroy_sender(sender: *mut BrowserPortSyphonSender);
}

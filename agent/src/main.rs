use anyhow::Context;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::env;
#[cfg(target_os = "macos")]
use std::ffi::{CStr, CString};
#[cfg(target_os = "windows")]
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration as StdDuration, Instant as StdInstant};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, RwLock};
use tokio::time::{timeout, Duration};
use tokio_tungstenite::tungstenite::Message;
#[cfg(target_os = "windows")]
use windows::Win32::System::Console::{GetConsoleProcessList, GetConsoleWindow};
#[cfg(target_os = "windows")]
use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE};

mod output_helper;

#[cfg(target_os = "macos")]
mod macos_login_item;
#[cfg(target_os = "macos")]
mod macos_menu;
#[cfg(target_os = "windows")]
mod windows_single_instance;
#[cfg(target_os = "windows")]
mod windows_tray;

type ConnId = u64;

const PROTOCOL_VERSION: i64 = 1;
const MSG_TYPE_VIDEO: u8 = 1;
const MSG_TYPE_AUDIO: u8 = 2;
const SPOUT_HELPER_STALL_GRACE_CHUNKS: u64 = 60;
const SPOUT_HELPER_STALL_TIMEOUT: StdDuration = StdDuration::from_secs(3);
const SPOUT_FASTPATH_RETRY_COOLDOWN: StdDuration = StdDuration::from_secs(20);

pub(crate) fn app_version() -> &'static str {
    option_env!("BROWSER_PORT_APP_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    BrowserPortExtension,
    Client,
}

#[derive(Clone)]
struct ConnectionHandle {
    role: Role,
    tx: mpsc::UnboundedSender<Message>,
}

#[derive(Clone, Debug, Default)]
struct PlayerStreamState {
    connected: bool,
    codec: Option<String>,
    coded_width: Option<u32>,
    coded_height: Option<u32>,
    fps: Option<f64>,
    bitrate: Option<f64>,
    tab_title: Option<String>,
    ndi_connected: Option<bool>,
    ndi_receivers: Option<u64>,
    syphon_connected: Option<bool>,
    syphon_clients: Option<u64>,
    video_chunks: u64,
    audio_chunks: u64,
    last_timestamp_us: u64,
    last_payload_len: usize,
    decode_backend: Option<String>,
    decode_ms: Option<f64>,
    convert_ms: Option<f64>,
    spout_send_ms: Option<f64>,
    send_path: Option<String>,
    mf_process_output_ms: Option<f64>,
    cpu_color_convert_ms: Option<f64>,
    frame_stats_ms: Option<f64>,
    spout_bridge_ms: Option<f64>,
    spout_swap_ms: Option<f64>,
    spout_upload_ms: Option<f64>,
    spout_send_texture_ms: Option<f64>,
    texture_path_ratio: Option<f64>,
    stream_latency_ms: Option<f64>,
    fastpath_state: Option<String>,
    fastpath_fallback_count: Option<u64>,
    fastpath_recover_count: Option<u64>,
    crop_applied: Option<bool>,
    effective_width: Option<u64>,
    effective_height: Option<u64>,
    queue_depth: Option<u64>,
    frame_mean_luma: Option<f64>,
    frame_non_black_ratio: Option<f64>,
    last_video_chunk_at: Option<StdInstant>,
    last_helper_stats_at: Option<StdInstant>,
}

#[derive(Clone, Debug)]
struct OutputFlags {
    ndi_enabled: bool,
    spout_enabled: bool,
    syphon_enabled: bool,
}

impl Default for OutputFlags {
    fn default() -> Self {
        Self {
            ndi_enabled: true,
            spout_enabled: cfg!(target_os = "windows"),
            syphon_enabled: cfg!(target_os = "macos"),
        }
    }
}

#[derive(Clone, Debug)]
struct OutputAvailability {
    ndi_available: bool,
    spout_available: bool,
    syphon_available: bool,
}

impl Default for OutputAvailability {
    fn default() -> Self {
        Self {
            ndi_available: detect_ndi_runtime(),
            spout_available: cfg!(target_os = "windows"),
            syphon_available: cfg!(target_os = "macos"),
        }
    }
}

struct SharedState {
    connections: HashMap<ConnId, ConnectionHandle>,
    player_routes: HashMap<u32, ConnId>,
    player_streams: HashMap<u32, PlayerStreamState>,
    player_configs: HashMap<u32, Value>,
    syphon_client_count: usize,
    outputs: OutputFlags,
    output_availability: OutputAvailability,
}

impl Default for SharedState {
    fn default() -> Self {
        let output_availability = OutputAvailability::default();
        let outputs = OutputFlags {
            ndi_enabled: output_availability.ndi_available,
            spout_enabled: output_availability.spout_available,
            syphon_enabled: output_availability.syphon_available,
        };
        Self {
            connections: HashMap::new(),
            player_routes: HashMap::new(),
            player_streams: HashMap::new(),
            player_configs: HashMap::new(),
            syphon_client_count: 0,
            outputs,
            output_availability,
        }
    }
}

impl SharedState {
    fn add_connection(&mut self, conn_id: ConnId, role: Role, tx: mpsc::UnboundedSender<Message>) {
        self.connections
            .insert(conn_id, ConnectionHandle { role, tx });
    }

    fn remove_connection(&mut self, conn_id: ConnId) -> Vec<u32> {
        self.connections.remove(&conn_id);
        let disconnected_players: Vec<u32> = self
            .player_routes
            .iter()
            .filter_map(|(player, owner)| {
                if *owner == conn_id {
                    Some(*player)
                } else {
                    None
                }
            })
            .collect();
        for player in &disconnected_players {
            self.player_routes.remove(player);
            self.player_configs.remove(player);
            if let Some(stream) = self.player_streams.get_mut(player) {
                stream.connected = false;
                stream.ndi_connected = Some(false);
                stream.ndi_receivers = Some(0);
                stream.syphon_connected = Some(false);
                stream.syphon_clients = Some(0);
            }
        }
        disconnected_players
    }

    fn route_player(&mut self, player_id: u32, conn_id: ConnId) {
        self.player_routes.insert(player_id, conn_id);
        self.player_streams.entry(player_id).or_default().connected = true;
    }

    fn sender_for_player(&self, player_id: u32) -> Option<mpsc::UnboundedSender<Message>> {
        let conn_id = self.player_routes.get(&player_id)?;
        self.connections
            .get(conn_id)
            .map(|handle| handle.tx.clone())
    }

    fn senders_by_role(&self, role: Role) -> Vec<mpsc::UnboundedSender<Message>> {
        self.connections
            .values()
            .filter_map(|handle| {
                if handle.role == role {
                    Some(handle.tx.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    fn all_senders(&self) -> Vec<mpsc::UnboundedSender<Message>> {
        self.connections.values().map(|c| c.tx.clone()).collect()
    }

    fn counts(&self) -> (usize, usize) {
        let mut ext_count = 0;
        let mut client_count = 0;
        for connection in self.connections.values() {
            match connection.role {
                Role::BrowserPortExtension => ext_count += 1,
                Role::Client => client_count += 1,
            }
        }
        (ext_count, client_count)
    }

    fn set_syphon_client_count(&mut self, count: usize) {
        self.syphon_client_count = count;
    }

    fn update_stream_config(&mut self, player_id: u32, message: &Value) {
        let stream = self.player_streams.entry(player_id).or_default();
        stream.connected = true;
        stream.codec = message
            .get("codec")
            .and_then(Value::as_str)
            .map(std::string::ToString::to_string);
        stream.coded_width = message
            .get("codedWidth")
            .and_then(Value::as_u64)
            .and_then(|v| u32::try_from(v).ok());
        stream.coded_height = message
            .get("codedHeight")
            .and_then(Value::as_u64)
            .and_then(|v| u32::try_from(v).ok());
        self.player_configs.insert(player_id, message.clone());
    }

    fn update_stream_status(&mut self, player_id: u32, message: &Value) {
        let stream = self.player_streams.entry(player_id).or_default();
        stream.connected = true;
        let stats = message
            .get("stats")
            .filter(|value| value.is_object())
            .unwrap_or(message);

        if let Some(codec) = stats
            .get("codec")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            stream.codec = Some(codec.to_string());
        }

        if let Some(resolution) = stats.get("resolution").and_then(Value::as_object) {
            stream.coded_width = resolution
                .get("width")
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok());
            stream.coded_height = resolution
                .get("height")
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok());
        } else {
            if let Some(width) = stats
                .get("codedWidth")
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
            {
                stream.coded_width = Some(width);
            }
            if let Some(height) = stats
                .get("codedHeight")
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
            {
                stream.coded_height = Some(height);
            }
        }

        if let Some(fps) = stats
            .get("fps")
            .and_then(Value::as_f64)
            .filter(|value| value.is_finite() && *value > 0.0)
        {
            stream.fps = Some(fps);
        }

        if let Some(bitrate) = stats
            .get("bitrate")
            .and_then(Value::as_f64)
            .filter(|value| value.is_finite() && *value >= 0.0)
        {
            stream.bitrate = Some(bitrate);
        }

        if let Some(tab_title) = stats
            .get("tabTitle")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            stream.tab_title = Some(tab_title.to_string());
        }
    }

    fn player_config_messages(&self) -> Vec<Value> {
        let mut entries = self
            .player_configs
            .iter()
            .map(|(player_id, config)| (*player_id, config.clone()))
            .collect::<Vec<_>>();
        entries.sort_by_key(|(player_id, _)| *player_id);
        entries.into_iter().map(|(_, config)| config).collect()
    }

    fn record_binary_chunk(
        &mut self,
        player_id: u32,
        msg_type: u8,
        timestamp_us: u64,
        payload_len: usize,
    ) {
        let stream = self.player_streams.entry(player_id).or_default();
        stream.connected = true;
        stream.last_timestamp_us = timestamp_us;
        stream.last_payload_len = payload_len;
        if msg_type == MSG_TYPE_VIDEO {
            stream.video_chunks = stream.video_chunks.saturating_add(1);
            stream.last_video_chunk_at = Some(StdInstant::now());
        } else if msg_type == MSG_TYPE_AUDIO {
            stream.audio_chunks = stream.audio_chunks.saturating_add(1);
        }
    }

    fn set_output(&mut self, key: &str, enabled: bool) -> bool {
        match key {
            "ndi" => {
                if !self.output_availability.ndi_available {
                    return false;
                }
                self.outputs.ndi_enabled = enabled;
                true
            }
            "spout" => {
                if !self.output_availability.spout_available {
                    return false;
                }
                self.outputs.spout_enabled = enabled;
                true
            }
            "syphon" => {
                if !self.output_availability.syphon_available {
                    return false;
                }
                self.outputs.syphon_enabled = enabled;
                true
            }
            _ => false,
        }
    }

    fn output_available(&self, key: &str) -> Option<bool> {
        match key {
            "ndi" => Some(self.output_availability.ndi_available),
            "spout" => Some(self.output_availability.spout_available),
            "syphon" => Some(self.output_availability.syphon_available),
            _ => None,
        }
    }

    fn output_enabled(&self, key: &str) -> Option<bool> {
        match key {
            "ndi" => Some(self.outputs.ndi_enabled),
            "spout" => Some(self.outputs.spout_enabled),
            "syphon" => Some(self.outputs.syphon_enabled),
            _ => None,
        }
    }

    fn update_helper_perf(&mut self, perf: &HelperPlayerPerf) {
        let stream = self.player_streams.entry(perf.player_id).or_default();
        if let Some(backend) = perf.decode_backend.as_ref() {
            if !backend.is_empty() {
                stream.decode_backend = Some(backend.clone());
            }
        }
        stream.decode_ms = perf.decode_ms;
        stream.convert_ms = perf.convert_ms;
        stream.spout_send_ms = perf.spout_send_ms;
        stream.send_path = perf.send_path.clone();
        stream.mf_process_output_ms = perf.mf_process_output_ms;
        stream.cpu_color_convert_ms = perf.cpu_color_convert_ms;
        stream.frame_stats_ms = perf.frame_stats_ms;
        stream.spout_bridge_ms = perf.spout_bridge_ms;
        stream.spout_swap_ms = perf.spout_swap_ms;
        stream.spout_upload_ms = perf.spout_upload_ms;
        stream.spout_send_texture_ms = perf.spout_send_texture_ms;
        stream.texture_path_ratio = perf.texture_path_ratio;
        stream.stream_latency_ms = perf.stream_latency_ms;
        stream.fastpath_state = perf.fastpath_state.clone();
        stream.fastpath_fallback_count = perf.fastpath_fallback_count;
        stream.fastpath_recover_count = perf.fastpath_recover_count;
        stream.crop_applied = perf.crop_applied;
        stream.effective_width = perf.effective_width;
        stream.effective_height = perf.effective_height;
        stream.queue_depth = perf.queue_depth;
        stream.frame_mean_luma = perf.frame_mean_luma;
        stream.frame_non_black_ratio = perf.frame_non_black_ratio;
        if let Some(connected) = perf.ndi_connected {
            stream.ndi_connected = Some(connected);
        }
        if let Some(receivers) = perf.ndi_receivers {
            stream.ndi_receivers = Some(receivers);
            stream.ndi_connected = Some(receivers > 0);
        }
        if let Some(connected) = perf.syphon_connected {
            stream.syphon_connected = Some(connected);
        }
        if let Some(clients) = perf.syphon_clients {
            stream.syphon_clients = Some(clients);
            stream.syphon_connected = Some(clients > 0);
        }
        stream.last_helper_stats_at = Some(StdInstant::now());
    }

    fn spout_helper_stalled(&self, now: StdInstant) -> bool {
        self.player_streams.values().any(|stream| {
            let active_video = stream
                .last_video_chunk_at
                .map(|at| now.duration_since(at) <= SPOUT_HELPER_STALL_TIMEOUT)
                .unwrap_or(false);
            if !active_video {
                return false;
            }
            match stream.last_helper_stats_at {
                Some(last_helper) => now.duration_since(last_helper) > SPOUT_HELPER_STALL_TIMEOUT,
                None => stream.video_chunks >= SPOUT_HELPER_STALL_GRACE_CHUNKS,
            }
        })
    }

    fn spout_helper_healthy(&self, now: StdInstant) -> bool {
        let mut has_active_video = false;
        for stream in self.player_streams.values() {
            let active_video = stream
                .last_video_chunk_at
                .map(|at| now.duration_since(at) <= SPOUT_HELPER_STALL_TIMEOUT)
                .unwrap_or(false);
            if !active_video {
                continue;
            }
            has_active_video = true;
            let healthy = stream
                .last_helper_stats_at
                .map(|last_helper| now.duration_since(last_helper) <= SPOUT_HELPER_STALL_TIMEOUT)
                .unwrap_or(false);
            if !healthy {
                return false;
            }
        }
        has_active_video
    }
}

#[derive(Clone, Copy)]
struct BinaryInfo {
    msg_type: u8,
    player_id: u32,
    timestamp_us: u64,
    payload_len: usize,
}

#[derive(Clone, Debug, Default)]
struct HelperPlayerPerf {
    player_id: u32,
    decode_backend: Option<String>,
    decode_ms: Option<f64>,
    convert_ms: Option<f64>,
    spout_send_ms: Option<f64>,
    send_path: Option<String>,
    mf_process_output_ms: Option<f64>,
    cpu_color_convert_ms: Option<f64>,
    frame_stats_ms: Option<f64>,
    spout_bridge_ms: Option<f64>,
    spout_swap_ms: Option<f64>,
    spout_upload_ms: Option<f64>,
    spout_send_texture_ms: Option<f64>,
    texture_path_ratio: Option<f64>,
    stream_latency_ms: Option<f64>,
    fastpath_state: Option<String>,
    fastpath_fallback_count: Option<u64>,
    fastpath_recover_count: Option<u64>,
    crop_applied: Option<bool>,
    effective_width: Option<u64>,
    effective_height: Option<u64>,
    queue_depth: Option<u64>,
    frame_mean_luma: Option<f64>,
    frame_non_black_ratio: Option<f64>,
    ndi_connected: Option<bool>,
    ndi_receivers: Option<u64>,
    syphon_connected: Option<bool>,
    syphon_clients: Option<u64>,
}

struct HandshakeInfo {
    role: Role,
    source: Option<String>,
}

struct OutputProcessManager {
    bind_addr: String,
    processes: HashMap<String, Child>,
    spout_fastpath_forced_off: bool,
    spout_fastpath_switched_at: Option<StdInstant>,
    spout_fastpath_retry_in_progress: bool,
    spout_fastpath_retry_failures: u32,
}

impl OutputProcessManager {
    fn new(bind_addr: &str) -> Self {
        Self {
            bind_addr: bind_addr.to_string(),
            processes: HashMap::new(),
            spout_fastpath_forced_off: false,
            spout_fastpath_switched_at: None,
            spout_fastpath_retry_in_progress: false,
            spout_fastpath_retry_failures: 0,
        }
    }

    fn apply(&mut self, output_name: &str, enabled: bool) -> Result<(), String> {
        if enabled {
            self.ensure_started(output_name)
        } else {
            self.ensure_stopped(output_name)
        }
    }

    fn ensure_started(&mut self, output_name: &str) -> Result<(), String> {
        if let Some(child) = self.processes.get_mut(output_name) {
            if let Ok(None) = child.try_wait() {
                return Ok(());
            }
            if let Ok(Some(status)) = child.try_wait() {
                eprintln!(
                    "output-helper exited output={} pid={} status={}",
                    output_name,
                    child.id(),
                    status
                );
            }
            self.processes.remove(output_name);
        }

        let mut command = self.make_command(output_name)?;
        let child = command
            .spawn()
            .map_err(|err| format!("failed to spawn {output_name} helper: {err}"))?;
        eprintln!(
            "output-helper started output={} pid={}",
            output_name,
            child.id()
        );
        self.processes.insert(output_name.to_string(), child);
        Ok(())
    }

    fn ensure_stopped(&mut self, output_name: &str) -> Result<(), String> {
        let Some(mut child) = self.processes.remove(output_name) else {
            return Ok(());
        };
        if let Ok(None) = child.try_wait() {
            let _ = child.kill();
            let _ = child.wait();
        }
        Ok(())
    }

    fn shutdown(&mut self) {
        let outputs = self.processes.keys().cloned().collect::<Vec<_>>();
        for output_name in outputs {
            let _ = self.ensure_stopped(&output_name);
        }
    }

    fn reconcile_enabled(&mut self, outputs: &OutputFlags) {
        let targets = [
            ("spout", outputs.spout_enabled),
            ("syphon", outputs.syphon_enabled),
            ("ndi", outputs.ndi_enabled),
        ];
        for (name, enabled) in targets {
            if enabled {
                if let Err(err) = self.ensure_started(name) {
                    eprintln!("{err}");
                }
            } else {
                let _ = self.ensure_stopped(name);
            }
        }
    }

    fn make_command(&self, output_name: &str) -> Result<Command, String> {
        let env_name = format!(
            "BROWSER_PORT_{}_HELPER_CMD",
            output_name.to_ascii_uppercase()
        );
        if let Ok(command_line) = env::var(&env_name) {
            if command_line.trim().is_empty() {
                return Err(format!("{env_name} is empty"));
            }
            #[cfg(target_os = "windows")]
            {
                let mut command = Command::new("cmd");
                command.arg("/C").arg(command_line);
                command.stdout(Stdio::inherit());
                command.stderr(Stdio::inherit());
                return Ok(command);
            }
            #[cfg(not(target_os = "windows"))]
            {
                let mut command = Command::new("sh");
                command.arg("-lc").arg(command_line);
                command.stdout(Stdio::inherit());
                command.stderr(Stdio::inherit());
                return Ok(command);
            }
        }

        let ws_url = format!("ws://{}", self.bind_addr);
        let current_exe = env::current_exe()
            .map_err(|err| format!("failed to resolve current exe for helper spawn: {err}"))?;
        let mut command = Command::new(current_exe);
        command
            .arg("output-helper")
            .arg("--mode")
            .arg(output_name)
            .arg("--ws")
            .arg(ws_url)
            .arg("--parent-pid")
            .arg(std::process::id().to_string());
        if output_name == "spout" && self.spout_fastpath_forced_off {
            command.env("BROWSER_PORT_SPOUT_TEXTURE_FASTPATH", "0");
        }
        command.stdout(Stdio::inherit());
        command.stderr(Stdio::inherit());
        Ok(command)
    }

    fn force_spout_cpu_fallback_if_needed(&mut self) {
        if self.spout_fastpath_forced_off {
            return;
        }
        if self.spout_fastpath_retry_in_progress {
            self.spout_fastpath_retry_failures =
                self.spout_fastpath_retry_failures.saturating_add(1);
            self.spout_fastpath_retry_in_progress = false;
        }
        self.spout_fastpath_forced_off = true;
        self.spout_fastpath_switched_at = Some(StdInstant::now());
        if let Some(mut process) = self.processes.remove("spout") {
            let _ = process.kill();
            let _ = process.wait();
        }
        if let Err(err) = self.apply("spout", true) {
            eprintln!("BrowserPort: failed to restart spout helper in fallback mode: {err}");
        } else {
            eprintln!("BrowserPort: forcing spout helper bgra fallback due fastpath stall");
        }
    }

    fn retry_spout_fastpath_if_due(&mut self) {
        if !self.spout_fastpath_forced_off {
            return;
        }
        if self.spout_fastpath_retry_failures > 0 {
            return;
        }
        let Some(switched_at) = self.spout_fastpath_switched_at else {
            return;
        };
        if switched_at.elapsed() < SPOUT_FASTPATH_RETRY_COOLDOWN {
            return;
        }
        self.spout_fastpath_forced_off = false;
        self.spout_fastpath_switched_at = Some(StdInstant::now());
        self.spout_fastpath_retry_in_progress = true;
        if let Some(mut process) = self.processes.remove("spout") {
            let _ = process.kill();
            let _ = process.wait();
        }
        if let Err(err) = self.apply("spout", true) {
            self.spout_fastpath_forced_off = true;
            self.spout_fastpath_retry_in_progress = false;
            eprintln!("BrowserPort: failed to retry spout fastpath, keeping fallback: {err}");
        } else {
            eprintln!("BrowserPort: retrying spout texture fastpath after cooldown");
        }
    }
}

impl Drop for OutputProcessManager {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn main() -> anyhow::Result<()> {
    #[cfg(target_os = "windows")]
    maybe_hide_console_window_for_direct_launch();

    if let Some(helper_args) = output_helper::parse_from_env()? {
        let rt = tokio::runtime::Runtime::new().context("failed to create tokio runtime")?;
        return rt.block_on(output_helper::run(helper_args));
    }

    if should_launch_menu_bar_app() {
        return run_menu_bar_app();
    }

    run_headless_browser_port()
}

#[cfg(target_os = "windows")]
fn maybe_hide_console_window_for_direct_launch() {
    let mut process_ids = [0_u32; 8];
    let process_count = unsafe { GetConsoleProcessList(&mut process_ids) };
    // When launched from cmd/powershell, multiple processes share the same console.
    if process_count <= 1 {
        let hwnd = unsafe { GetConsoleWindow() };
        if !hwnd.0.is_null() {
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
        }
    }
}

fn should_launch_menu_bar_app() -> bool {
    if env::var("BROWSER_PORT_HEADLESS")
        .ok()
        .as_deref()
        .and_then(parse_env_bool)
        .unwrap_or(false)
    {
        return false;
    }

    if env::var("BROWSER_PORT_TRAY")
        .ok()
        .as_deref()
        .and_then(parse_env_bool)
        .unwrap_or(false)
    {
        return true;
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        return true;
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        false
    }
}

fn run_headless_browser_port() -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new().context("failed to create tokio runtime")?;
    let state = Arc::new(RwLock::new(SharedState::default()));
    rt.block_on(run_browser_port(
        Arc::clone(&state),
        Arc::new(AtomicBool::new(false)),
    ))
}

#[cfg(target_os = "macos")]
fn run_menu_bar_app() -> anyhow::Result<()> {
    if let Err(err) = macos_login_item::register_main_app_login_item() {
        eprintln!("BrowserPort: failed to register login item via SMAppService: {err}");
    } else {
        eprintln!("BrowserPort: login item registration is enabled");
    }

    let stop = Arc::new(AtomicBool::new(false));
    let state = Arc::new(RwLock::new(SharedState::default()));
    let bind_addr =
        env::var("BROWSER_PORT_AGENT_BIND").unwrap_or_else(|_| "127.0.0.1:1844".to_string());
    eprintln!(
        "BrowserPort starting macOS menu bar app on ws://{}",
        bind_addr
    );
    let runtime = tokio::runtime::Runtime::new().context("failed to create tokio runtime")?;
    let handle = runtime.handle().clone();
    let rt_stop = Arc::clone(&stop);
    let rt_state = Arc::clone(&state);
    let runtime_thread = thread::spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            runtime.block_on(run_browser_port(rt_state, Arc::clone(&rt_stop)))
        }));
        match result {
            Ok(Ok(())) => {}
            Ok(Err(err)) => eprintln!("browser-port server exited with error: {err}"),
            Err(_) => eprintln!("browser-port server thread panicked"),
        }
        rt_stop.store(true, Ordering::Relaxed);
    });
    let result = macos_menu::run_menu_bar_app(state, bind_addr, stop.clone(), handle);
    stop.store(true, Ordering::Relaxed);
    let _ = runtime_thread.join();
    result
}

#[cfg(target_os = "windows")]
fn run_menu_bar_app() -> anyhow::Result<()> {
    let Some(_single_instance_guard) =
        windows_single_instance::SingleInstanceGuard::acquire("Local\\BrowserPortTrayApp")?
    else {
        eprintln!("BrowserPort: another tray instance is already running");
        return Ok(());
    };

    let stop = Arc::new(AtomicBool::new(false));
    let state = Arc::new(RwLock::new(SharedState::default()));
    let bind_addr =
        env::var("BROWSER_PORT_AGENT_BIND").unwrap_or_else(|_| "127.0.0.1:1844".to_string());
    eprintln!(
        "BrowserPort starting Windows tray app on ws://{}",
        bind_addr
    );
    let runtime = tokio::runtime::Runtime::new().context("failed to create tokio runtime")?;
    let handle = runtime.handle().clone();
    let rt_stop = Arc::clone(&stop);
    let rt_state = Arc::clone(&state);
    let runtime_thread = thread::spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            runtime.block_on(run_browser_port(rt_state, Arc::clone(&rt_stop)))
        }));
        match result {
            Ok(Ok(())) => {}
            Ok(Err(err)) => eprintln!("browser-port server exited with error: {err}"),
            Err(_) => eprintln!("browser-port server thread panicked"),
        }
        rt_stop.store(true, Ordering::Relaxed);
    });
    let result = windows_tray::run_tray_app(state, bind_addr, stop.clone(), handle);
    stop.store(true, Ordering::Relaxed);
    let _ = runtime_thread.join();
    result
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn run_menu_bar_app() -> anyhow::Result<()> {
    run_headless_browser_port()
}

async fn run_browser_port(
    state: Arc<RwLock<SharedState>>,
    stop: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let bind_addr =
        env::var("BROWSER_PORT_AGENT_BIND").unwrap_or_else(|_| "127.0.0.1:1844".to_string());
    let spout_fastpath_watchdog_enabled = env::var("BROWSER_PORT_SPOUT_FASTPATH_WATCHDOG")
        .ok()
        .as_deref()
        .and_then(parse_env_bool)
        .unwrap_or(false);
    let listener = TcpListener::bind(&bind_addr)
        .await
        .with_context(|| format!("failed to bind {}", bind_addr))?;
    println!("BrowserPort listening on ws://{}", bind_addr);

    let output_manager = Arc::new(Mutex::new(OutputProcessManager::new(&bind_addr)));
    let next_id = Arc::new(AtomicU64::new(1));
    {
        let stop_ref = Arc::clone(&stop);
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                stop_ref.store(true, Ordering::Relaxed);
            }
        });
    }
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};

        let stop_ref = Arc::clone(&stop);
        tokio::spawn(async move {
            if let Ok(mut sigterm) = signal(SignalKind::terminate()) {
                let _ = sigterm.recv().await;
                stop_ref.store(true, Ordering::Relaxed);
            }
        });
    }
    {
        let outputs = {
            let lock = state.read().await;
            vec![
                ("spout".to_string(), lock.outputs.spout_enabled),
                ("syphon".to_string(), lock.outputs.syphon_enabled),
                ("ndi".to_string(), lock.outputs.ndi_enabled),
            ]
        };
        if let Ok(mut manager) = output_manager.lock() {
            for (output_name, enabled) in outputs {
                if enabled {
                    if let Err(err) = manager.apply(&output_name, true) {
                        eprintln!("{err}");
                    }
                }
            }
        }
    }
    let mut helper_reconcile_at = StdInstant::now();

    while !stop.load(Ordering::Relaxed) {
        if helper_reconcile_at.elapsed() >= StdDuration::from_secs(2) {
            let now = StdInstant::now();
            let (outputs, helper_stalled, helper_healthy) = {
                let lock = state.read().await;
                (
                    lock.outputs.clone(),
                    lock.spout_helper_stalled(now),
                    lock.spout_helper_healthy(now),
                )
            };
            if let Ok(mut manager) = output_manager.lock() {
                manager.reconcile_enabled(&outputs);
                if spout_fastpath_watchdog_enabled && outputs.spout_enabled {
                    if helper_stalled {
                        manager.force_spout_cpu_fallback_if_needed();
                    } else if helper_healthy {
                        manager.retry_spout_fastpath_if_due();
                    }
                }
            }
            helper_reconcile_at = StdInstant::now();
        }

        let accepted = timeout(Duration::from_millis(250), listener.accept()).await;
        let (stream, _) = match accepted {
            Ok(Ok(pair)) => pair,
            Ok(Err(err)) => {
                eprintln!("listener accept failed: {err}");
                continue;
            }
            Err(_) => continue,
        };
        let state_ref = Arc::clone(&state);
        let next_id_ref = Arc::clone(&next_id);
        let stop_ref = Arc::clone(&stop);
        let output_manager_ref = Arc::clone(&output_manager);
        let bind_for_task = bind_addr.clone();
        tokio::spawn(async move {
            if let Err(err) = handle_connection(
                stream,
                state_ref,
                next_id_ref,
                &bind_for_task,
                stop_ref,
                output_manager_ref,
            )
            .await
            {
                eprintln!("connection error: {err}");
            }
        });
    }

    if let Ok(mut manager) = output_manager.lock() {
        manager.shutdown();
    }
    println!("BrowserPort shutting down");
    Ok(())
}

async fn handle_connection(
    stream: TcpStream,
    state: Arc<RwLock<SharedState>>,
    next_id: Arc<AtomicU64>,
    bind_addr: &str,
    stop: Arc<AtomicBool>,
    output_manager: Arc<Mutex<OutputProcessManager>>,
) -> anyhow::Result<()> {
    let ws_stream = tokio_tungstenite::accept_async(stream)
        .await
        .context("ws handshake failed")?;
    let conn_id = next_id.fetch_add(1, Ordering::SeqCst);
    let (mut ws_tx, mut ws_rx) = ws_stream.split();
    let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel::<Message>();

    let writer = tokio::spawn(async move {
        while let Some(message) = outgoing_rx.recv().await {
            if ws_tx.send(message).await.is_err() {
                break;
            }
        }
    });

    let first_message = timeout(Duration::from_secs(5), ws_rx.next()).await;
    let handshake = match parse_hello(first_message) {
        Ok(info) => info,
        Err((code, message)) => {
            send_json_message(&outgoing_tx, error_message(&code, &message, None));
            let _ = outgoing_tx.send(Message::Close(None));
            let _ = writer.await;
            return Ok(());
        }
    };
    let role = handshake.role;
    let source = handshake.source.as_deref().unwrap_or("-");

    {
        let mut lock = state.write().await;
        lock.add_connection(conn_id, role, outgoing_tx.clone());
    }
    eprintln!(
        "connection opened conn={} role={} source={}",
        conn_id,
        role_name(role),
        source
    );

    send_json_message(
        &outgoing_tx,
        json!({
            "type": "hello-ack",
            "protocolVersion": PROTOCOL_VERSION,
            "role": role_name(role),
            "connectionId": conn_id,
            "server": "browser-port",
        }),
    );
    if role == Role::BrowserPortExtension {
        send_json_message(
            &outgoing_tx,
            json!({
                "type": "tier-info",
                "maxPlayers": 4,
            }),
        );
    } else {
        let config_messages = {
            let lock = state.read().await;
            lock.player_config_messages()
        };
        for config in config_messages {
            send_json_message(&outgoing_tx, config);
        }
    }
    broadcast_browser_port_stats(&state, bind_addr).await;

    while !stop.load(Ordering::Relaxed) {
        let next_message = timeout(Duration::from_millis(250), ws_rx.next()).await;
        let message = match next_message {
            Ok(Some(Ok(message))) => message,
            Ok(Some(Err(err))) => {
                eprintln!(
                    "read failed conn={} role={} source={}: {}",
                    conn_id,
                    role_name(role),
                    source,
                    err
                );
                break;
            }
            Ok(None) => break,
            Err(_) => continue,
        };
        match message {
            Message::Text(text) => {
                handle_text_message(
                    conn_id,
                    role,
                    &text,
                    &state,
                    &outgoing_tx,
                    bind_addr,
                    &output_manager,
                )
                .await;
            }
            Message::Binary(payload) => {
                handle_binary_message(conn_id, role, &payload, &state, &outgoing_tx).await;
            }
            Message::Ping(payload) => {
                let _ = outgoing_tx.send(Message::Pong(payload));
            }
            Message::Close(_) => break,
            _ => {}
        }
    }

    let disconnected_players = {
        let mut lock = state.write().await;
        lock.remove_connection(conn_id)
    };
    eprintln!(
        "connection closed conn={} role={} source={} players_disconnected={}",
        conn_id,
        role_name(role),
        source,
        disconnected_players.len()
    );
    for player_id in disconnected_players {
        broadcast_status(
            &state,
            json!({
                "type": "status",
                "event": "player-disconnected",
                "playerId": player_id,
            }),
        )
        .await;
    }
    broadcast_browser_port_stats(&state, bind_addr).await;

    let _ = outgoing_tx.send(Message::Close(None));
    let _ = writer.await;
    Ok(())
}

fn parse_hello(
    first_message: Result<
        Option<Result<Message, tokio_tungstenite::tungstenite::Error>>,
        tokio::time::error::Elapsed,
    >,
) -> Result<HandshakeInfo, (String, String)> {
    let message = match first_message {
        Ok(Some(Ok(message))) => message,
        Ok(Some(Err(_))) => {
            return Err((
                "E_HANDSHAKE_READ".to_string(),
                "failed to read hello message".to_string(),
            ))
        }
        Ok(None) => {
            return Err((
                "E_HANDSHAKE_CLOSED".to_string(),
                "connection closed before hello".to_string(),
            ))
        }
        Err(_) => {
            return Err((
                "E_HANDSHAKE_TIMEOUT".to_string(),
                "hello message timeout".to_string(),
            ))
        }
    };
    let text = match message {
        Message::Text(text) => text,
        _ => {
            return Err((
                "E_HANDSHAKE_TYPE".to_string(),
                "first message must be hello JSON".to_string(),
            ))
        }
    };
    let value: Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(_) => {
            return Err((
                "E_HANDSHAKE_JSON".to_string(),
                "invalid hello json".to_string(),
            ))
        }
    };
    if value.get("type").and_then(Value::as_str) != Some("hello") {
        return Err((
            "E_HANDSHAKE_MISSING".to_string(),
            "missing hello message".to_string(),
        ));
    }
    if value
        .get("protocolVersion")
        .and_then(Value::as_i64)
        .unwrap_or(0)
        != PROTOCOL_VERSION
    {
        return Err((
            "E_PROTOCOL_VERSION".to_string(),
            "unsupported protocolVersion".to_string(),
        ));
    }
    let source = value
        .get("capabilities")
        .and_then(Value::as_object)
        .and_then(|cap| cap.get("source"))
        .and_then(Value::as_str)
        .map(std::string::ToString::to_string);
    match value.get("role").and_then(Value::as_str) {
        Some("browser-port-extension") => Ok(HandshakeInfo {
            role: Role::BrowserPortExtension,
            source,
        }),
        Some("client") => Ok(HandshakeInfo {
            role: Role::Client,
            source,
        }),
        _ => Err((
            "E_HANDSHAKE_ROLE".to_string(),
            "role must be browser-port-extension or client".to_string(),
        )),
    }
}

async fn handle_text_message(
    conn_id: ConnId,
    role: Role,
    text: &str,
    state: &Arc<RwLock<SharedState>>,
    conn_tx: &mpsc::UnboundedSender<Message>,
    bind_addr: &str,
    output_manager: &Arc<Mutex<OutputProcessManager>>,
) {
    let value: Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(_) => {
            send_json_message(
                conn_tx,
                error_message("E_INVALID_JSON", "invalid JSON payload", None),
            );
            return;
        }
    };
    let msg_type = value.get("type").and_then(Value::as_str).unwrap_or("");
    if msg_type == "ping" {
        send_json_message(conn_tx, json!({"type": "pong"}));
        return;
    }

    match role {
        Role::BrowserPortExtension => {
            if !matches!(
                msg_type,
                "register"
                    | "config"
                    | "playback"
                    | "search-results"
                    | "search-selected"
                    | "status"
                    | "error"
                    | "pong"
            ) {
                send_json_message(
                    conn_tx,
                    error_message(
                        "E_UNSUPPORTED_MESSAGE",
                        "unsupported browser-port-extension message type",
                        None,
                    ),
                );
                return;
            }
            let player_scoped = matches!(
                msg_type,
                "register" | "config" | "playback" | "search-results" | "search-selected"
            );
            let player_id = value
                .get("playerId")
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok());
            if player_scoped && player_id.is_none() {
                send_json_message(
                    conn_tx,
                    error_message("E_PLAYER_REQUIRED", "playerId is required", None),
                );
                return;
            }
            if let Some(player_id) = player_id {
                let mut lock = state.write().await;
                lock.route_player(player_id, conn_id);
                if msg_type == "config" {
                    lock.update_stream_config(player_id, &value);
                }
                if msg_type == "status" {
                    lock.update_stream_status(player_id, &value);
                }
                if msg_type == "register" {
                    drop(lock);
                    broadcast_status(
                        state,
                        json!({
                            "type": "status",
                            "event": "player-connected",
                            "playerId": player_id,
                        }),
                    )
                    .await;
                    broadcast_browser_port_stats(state, bind_addr).await;
                }
            }
            broadcast_to_clients(state, Message::Text(text.to_string().into())).await;
        }
        Role::Client => {
            if msg_type == "browser-port-control" {
                handle_browser_port_control(state, conn_tx, &value, bind_addr, output_manager)
                    .await;
                return;
            }
            if msg_type == "helper-stats" {
                handle_helper_stats(state, &value, bind_addr).await;
                return;
            }
            if !matches!(msg_type, "control" | "search" | "pong") {
                send_json_message(
                    conn_tx,
                    error_message(
                        "E_UNSUPPORTED_MESSAGE",
                        "unsupported client message type",
                        None,
                    ),
                );
                return;
            }
            if msg_type == "pong" {
                return;
            }
            let player_id = value
                .get("playerId")
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok());
            let Some(player_id) = player_id else {
                send_json_message(
                    conn_tx,
                    error_message("E_PLAYER_REQUIRED", "playerId is required", None),
                );
                return;
            };
            let sender = {
                let lock = state.read().await;
                lock.sender_for_player(player_id)
            };
            if let Some(sender) = sender {
                let _ = sender.send(Message::Text(text.to_string().into()));
            } else {
                send_json_message(
                    conn_tx,
                    error_message(
                        "E_PLAYER_NOT_REGISTERED",
                        "player route is not available",
                        Some(player_id),
                    ),
                );
            }
        }
    }
}

async fn handle_browser_port_control(
    state: &Arc<RwLock<SharedState>>,
    conn_tx: &mpsc::UnboundedSender<Message>,
    value: &Value,
    bind_addr: &str,
    output_manager: &Arc<Mutex<OutputProcessManager>>,
) {
    let command = value
        .get("command")
        .and_then(Value::as_str)
        .or_else(|| value.get("action").and_then(Value::as_str))
        .unwrap_or("");
    if command == "get-status" {
        send_browser_port_stats(state, bind_addr, conn_tx).await;
        return;
    }

    if command == "set-output" || command == "toggle-output" {
        let Some(output_name) = value.get("output").and_then(Value::as_str) else {
            send_json_message(
                conn_tx,
                error_message(
                    "E_INVALID_JSON",
                    "output is required for browser-port-control",
                    None,
                ),
            );
            return;
        };

        #[cfg(target_os = "macos")]
        if output_name == "spout" {
            send_json_message(
                conn_tx,
                error_message(
                    "E_UNSUPPORTED_MESSAGE",
                    "spout is not supported on macOS; use syphon output",
                    None,
                ),
            );
            return;
        }

        let (enabled, available) = {
            let mut lock = state.write().await;
            let available = lock.output_available(output_name);
            let Some(available) = available else {
                send_json_message(
                    conn_tx,
                    error_message("E_UNSUPPORTED_MESSAGE", "unknown output target", None),
                );
                return;
            };
            if !available {
                send_json_message(
                    conn_tx,
                    error_message(
                        "E_UNSUPPORTED_MESSAGE",
                        "output is not available on this host",
                        None,
                    ),
                );
                return;
            }
            let enabled = if command == "toggle-output" {
                let current = lock.output_enabled(output_name).unwrap_or(false);
                !current
            } else if let Some(value_bool) = value.get("enabled").and_then(Value::as_bool) {
                value_bool
            } else if let Some(value_bool) = value.get("value").and_then(Value::as_bool) {
                value_bool
            } else {
                send_json_message(
                    conn_tx,
                    error_message("E_INVALID_JSON", "enabled=true/false is required", None),
                );
                return;
            };
            let _ = lock.set_output(output_name, enabled);
            (enabled, available)
        };
        if let Ok(mut manager) = output_manager.lock() {
            if let Err(err) = manager.apply(output_name, enabled) {
                send_json_message(conn_tx, error_message("E_OUTPUT_RUNTIME", &err, None));
            }
        }

        send_json_message(
            conn_tx,
            json!({
                "type": "status",
                "event": "browser-port-output-changed",
                "output": output_name,
                "enabled": enabled,
                "available": available,
            }),
        );
        broadcast_browser_port_stats(state, bind_addr).await;
        return;
    }

    send_json_message(
        conn_tx,
        error_message(
            "E_UNSUPPORTED_MESSAGE",
            "unsupported browser-port-control command",
            None,
        ),
    );
}

async fn handle_helper_stats(state: &Arc<RwLock<SharedState>>, value: &Value, bind_addr: &str) {
    let syphon_client_count = value
        .get("syphonClientCount")
        .and_then(Value::as_u64)
        .and_then(|count| usize::try_from(count).ok());
    let Some(players) = value.get("players").and_then(Value::as_array) else {
        if let Some(count) = syphon_client_count {
            let mut lock = state.write().await;
            lock.set_syphon_client_count(count);
        }
        broadcast_browser_port_stats(state, bind_addr).await;
        return;
    };
    let perf_list = players
        .iter()
        .filter_map(parse_helper_player_perf)
        .collect::<Vec<_>>();
    {
        let mut lock = state.write().await;
        if let Some(count) = syphon_client_count {
            lock.set_syphon_client_count(count);
        }
        for perf in &perf_list {
            lock.update_helper_perf(perf);
        }
    }
    broadcast_browser_port_stats(state, bind_addr).await;
}

fn parse_helper_player_perf(value: &Value) -> Option<HelperPlayerPerf> {
    let player_id = value
        .get("playerId")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())?;
    let queue_depth = value.get("queueDepth").and_then(Value::as_u64).or_else(|| {
        value
            .get("queueDepth")
            .and_then(Value::as_f64)
            .map(|v| if v < 0.0 { 0 } else { v as u64 })
    });
    let ndi_receivers = value
        .get("ndiReceivers")
        .and_then(Value::as_u64)
        .or_else(|| {
            value.get("ndiReceivers").and_then(Value::as_f64).map(|v| {
                if v < 0.0 {
                    0
                } else {
                    v as u64
                }
            })
        });
    let syphon_clients = value
        .get("syphonClients")
        .and_then(Value::as_u64)
        .or_else(|| {
            value.get("syphonClients").and_then(Value::as_f64).map(|v| {
                if v < 0.0 {
                    0
                } else {
                    v as u64
                }
            })
        });
    Some(HelperPlayerPerf {
        player_id,
        decode_backend: value
            .get("decodeBackend")
            .and_then(Value::as_str)
            .map(std::string::ToString::to_string),
        decode_ms: value.get("decodeMs").and_then(Value::as_f64),
        convert_ms: value.get("convertMs").and_then(Value::as_f64),
        spout_send_ms: value.get("spoutSendMs").and_then(Value::as_f64),
        send_path: value
            .get("sendPath")
            .and_then(Value::as_str)
            .map(std::string::ToString::to_string),
        mf_process_output_ms: value.get("mfProcessOutputMs").and_then(Value::as_f64),
        cpu_color_convert_ms: value.get("cpuColorConvertMs").and_then(Value::as_f64),
        frame_stats_ms: value.get("frameStatsMs").and_then(Value::as_f64),
        spout_bridge_ms: value.get("spoutBridgeMs").and_then(Value::as_f64),
        spout_swap_ms: value.get("spoutSwapMs").and_then(Value::as_f64),
        spout_upload_ms: value.get("spoutUploadMs").and_then(Value::as_f64),
        spout_send_texture_ms: value.get("spoutSendTextureMs").and_then(Value::as_f64),
        texture_path_ratio: value.get("texturePathRatio").and_then(Value::as_f64),
        stream_latency_ms: value.get("streamLatencyMs").and_then(Value::as_f64),
        fastpath_state: value
            .get("fastpathState")
            .and_then(Value::as_str)
            .map(std::string::ToString::to_string),
        fastpath_fallback_count: value.get("fastpathFallbackCount").and_then(Value::as_u64),
        fastpath_recover_count: value.get("fastpathRecoverCount").and_then(Value::as_u64),
        crop_applied: value.get("cropApplied").and_then(Value::as_bool),
        effective_width: value.get("effectiveWidth").and_then(Value::as_u64),
        effective_height: value.get("effectiveHeight").and_then(Value::as_u64),
        queue_depth,
        frame_mean_luma: value.get("frameMeanLuma").and_then(Value::as_f64),
        frame_non_black_ratio: value.get("frameNonBlackRatio").and_then(Value::as_f64),
        ndi_connected: value.get("ndiConnected").and_then(Value::as_bool),
        ndi_receivers,
        syphon_connected: value.get("syphonConnected").and_then(Value::as_bool),
        syphon_clients,
    })
}

async fn handle_binary_message(
    conn_id: ConnId,
    role: Role,
    payload: &[u8],
    state: &Arc<RwLock<SharedState>>,
    conn_tx: &mpsc::UnboundedSender<Message>,
) {
    if role != Role::BrowserPortExtension {
        send_json_message(
            conn_tx,
            error_message(
                "E_BINARY_FORBIDDEN",
                "binary is only for browser-port-extension role",
                None,
            ),
        );
        return;
    }
    let Some(binary_info) = parse_binary_info(payload) else {
        send_json_message(
            conn_tx,
            error_message("E_BINARY_HEADER", "invalid binary header", None),
        );
        return;
    };
    if binary_info.msg_type != MSG_TYPE_VIDEO && binary_info.msg_type != MSG_TYPE_AUDIO {
        send_json_message(
            conn_tx,
            error_message(
                "E_UNSUPPORTED_CHUNK",
                "unsupported binary message type",
                None,
            ),
        );
        return;
    }
    {
        let mut lock = state.write().await;
        lock.route_player(binary_info.player_id, conn_id);
        lock.record_binary_chunk(
            binary_info.player_id,
            binary_info.msg_type,
            binary_info.timestamp_us,
            binary_info.payload_len,
        );
    }
    broadcast_to_clients(state, Message::Binary(payload.to_vec().into())).await;
}

fn parse_binary_info(payload: &[u8]) -> Option<BinaryInfo> {
    if payload.len() < 16 {
        return None;
    }
    let msg_type = payload[0];
    let version = payload[1];
    if version == 1 {
        if payload.len() < 20 {
            return None;
        }
        let player_id = u32::from_le_bytes([payload[4], payload[5], payload[6], payload[7]]);
        let timestamp_us = u64::from_le_bytes([
            payload[8],
            payload[9],
            payload[10],
            payload[11],
            payload[12],
            payload[13],
            payload[14],
            payload[15],
        ]);
        let declared_payload_len =
            u32::from_le_bytes([payload[16], payload[17], payload[18], payload[19]]) as usize;
        if payload.len() < 20 + declared_payload_len {
            return None;
        }
        return Some(BinaryInfo {
            msg_type,
            player_id,
            timestamp_us,
            payload_len: declared_payload_len,
        });
    }

    let header_size = u16::from_le_bytes([payload[2], payload[3]]) as usize;
    if header_size < 16 || payload.len() < header_size {
        return None;
    }
    let player_id = u32::from_le_bytes([payload[4], payload[5], payload[6], payload[7]]);
    let timestamp_us = u64::from_le_bytes([
        payload[8],
        payload[9],
        payload[10],
        payload[11],
        payload[12],
        payload[13],
        payload[14],
        payload[15],
    ]);
    Some(BinaryInfo {
        msg_type,
        player_id,
        timestamp_us,
        payload_len: payload.len().saturating_sub(header_size),
    })
}

async fn broadcast_to_clients(state: &Arc<RwLock<SharedState>>, message: Message) {
    let senders = {
        let lock = state.read().await;
        lock.senders_by_role(Role::Client)
    };
    for sender in senders {
        let _ = sender.send(message.clone());
    }
}

async fn broadcast_status(state: &Arc<RwLock<SharedState>>, payload: Value) {
    let senders = {
        let lock = state.read().await;
        lock.senders_by_role(Role::Client)
    };
    for sender in senders {
        send_json_message(&sender, payload.clone());
    }
}

async fn send_browser_port_stats(
    state: &Arc<RwLock<SharedState>>,
    bind_addr: &str,
    sender: &mpsc::UnboundedSender<Message>,
) {
    let payload = build_browser_port_stats_payload(state, bind_addr).await;
    send_json_message(sender, payload);
}

async fn broadcast_browser_port_stats(state: &Arc<RwLock<SharedState>>, bind_addr: &str) {
    let payload = build_browser_port_stats_payload(state, bind_addr).await;
    let senders = {
        let lock = state.read().await;
        lock.all_senders()
    };
    for sender in senders {
        send_json_message(&sender, payload.clone());
    }
}

async fn build_browser_port_stats_payload(
    state: &Arc<RwLock<SharedState>>,
    bind_addr: &str,
) -> Value {
    let (
        ext_count,
        client_count,
        output_flags,
        output_availability,
        syphon_client_count,
        mut player_streams,
        players_registered,
    ) = {
        let lock = state.read().await;
        let (ext_count, client_count) = lock.counts();
        let player_streams = lock
            .player_streams
            .iter()
            .map(|(player_id, stream)| (*player_id, stream.clone()))
            .collect::<Vec<_>>();
        (
            ext_count,
            client_count,
            lock.outputs.clone(),
            lock.output_availability.clone(),
            lock.syphon_client_count,
            player_streams,
            lock.player_routes.len(),
        )
    };
    player_streams.sort_by_key(|(player_id, _)| *player_id);
    let players = player_streams
        .into_iter()
        .map(|(player_id, stream)| {
            json!({
                "playerId": player_id,
                "connected": stream.connected,
                "codec": stream.codec,
                "codedWidth": stream.coded_width,
                "codedHeight": stream.coded_height,
                "fps": stream.fps,
                "bitrate": stream.bitrate,
                "tabTitle": stream.tab_title,
                "ndiConnected": stream.ndi_connected,
                "ndiReceivers": stream.ndi_receivers,
                "syphonConnected": stream.syphon_connected,
                "syphonClients": stream.syphon_clients,
                "videoChunks": stream.video_chunks,
                "audioChunks": stream.audio_chunks,
                "lastTimestampUs": stream.last_timestamp_us,
                "lastPayloadLen": stream.last_payload_len,
                "decodeBackend": stream.decode_backend,
                "decodeMs": stream.decode_ms,
                "convertMs": stream.convert_ms,
                "spoutSendMs": stream.spout_send_ms,
                "sendPath": stream.send_path,
                "mfProcessOutputMs": stream.mf_process_output_ms,
                "cpuColorConvertMs": stream.cpu_color_convert_ms,
                "frameStatsMs": stream.frame_stats_ms,
                "spoutBridgeMs": stream.spout_bridge_ms,
                "spoutSwapMs": stream.spout_swap_ms,
                "spoutUploadMs": stream.spout_upload_ms,
                "spoutSendTextureMs": stream.spout_send_texture_ms,
                "texturePathRatio": stream.texture_path_ratio,
                "streamLatencyMs": stream.stream_latency_ms,
                "fastpathState": stream.fastpath_state,
                "fastpathFallbackCount": stream.fastpath_fallback_count,
                "fastpathRecoverCount": stream.fastpath_recover_count,
                "cropApplied": stream.crop_applied,
                "effectiveWidth": stream.effective_width,
                "effectiveHeight": stream.effective_height,
                "queueDepth": stream.queue_depth,
                "frameMeanLuma": stream.frame_mean_luma,
                "frameNonBlackRatio": stream.frame_non_black_ratio,
            })
        })
        .collect::<Vec<_>>();

    let mut outputs = serde_json::Map::new();
    outputs.insert(
        "ndi".to_string(),
        json!({
            "enabled": output_flags.ndi_enabled,
            "available": output_availability.ndi_available,
        }),
    );
    #[cfg(not(target_os = "macos"))]
    outputs.insert(
        "spout".to_string(),
        json!({
            "enabled": output_flags.spout_enabled,
            "available": output_availability.spout_available,
        }),
    );
    outputs.insert(
        "syphon".to_string(),
        json!({
            "enabled": output_flags.syphon_enabled,
            "available": output_availability.syphon_available,
        }),
    );

    json!({
        "type": "status",
        "event": "browser-port-stats",
        "bind": bind_addr,
        "extensions": ext_count,
        "clients": client_count,
        "playersRegistered": players_registered,
        "syphonClientCount": syphon_client_count,
        "outputs": outputs,
        "players": players,
    })
}

fn error_message(code: &str, message: &str, player_id: Option<u32>) -> Value {
    let mut payload = json!({
        "type": "error",
        "code": code,
        "message": message,
    });
    if let Some(player_id) = player_id {
        payload["playerId"] = json!(player_id);
    }
    payload
}

fn send_json_message(sender: &mpsc::UnboundedSender<Message>, payload: Value) {
    let _ = sender.send(Message::Text(payload.to_string().into()));
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::BrowserPortExtension => "browser-port-extension",
        Role::Client => "client",
    }
}

fn detect_ndi_runtime() -> bool {
    if let Ok(raw) = env::var("BROWSER_PORT_NDI_AVAILABLE") {
        let normalized = raw.trim().to_ascii_lowercase();
        return matches!(normalized.as_str(), "1" | "true" | "yes" | "on");
    }

    #[cfg(target_os = "windows")]
    {
        for candidate in ndi_library_candidates_windows() {
            if Path::new(&candidate).exists() {
                return true;
            }
        }
        return false;
    }

    #[cfg(target_os = "macos")]
    {
        let mut reasons = Vec::new();
        for candidate in ndi_library_candidates_macos() {
            match try_load_ndi_runtime_macos(&candidate) {
                Ok(()) => {
                    eprintln!("BrowserPort: detected NDI runtime path={candidate}");
                    return true;
                }
                Err(reason) => reasons.push(format!("{candidate}: {reason}")),
            }
        }
        if !reasons.is_empty() {
            eprintln!(
                "BrowserPort: NDI runtime unavailable on macOS {}",
                reasons.join(" | ")
            );
        }
        return false;
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        false
    }
}

#[cfg(target_os = "windows")]
fn ndi_library_candidates_windows() -> Vec<String> {
    let mut candidates = Vec::new();
    if let Ok(raw) = env::var("BROWSER_PORT_NDI_LIBRARY_PATH") {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            candidates.push(trimmed.to_string());
        }
    }
    candidates.push(r"C:\Program Files\NDI\NDI 6 Runtime\Processing.NDI.Lib.x64.dll".to_string());
    candidates
        .push(r"C:\Program Files\NDI\NDI 6 Tools\Runtime\Processing.NDI.Lib.x64.dll".to_string());
    candidates
        .push(r"C:\Program Files\NDI\NDI 6 Tools\Router\Processing.NDI.Lib.x64.dll".to_string());
    candidates
        .push(r"C:\Program Files\NDI\NDI 6 Tools\Remote\Processing.NDI.Lib.x64.dll".to_string());
    candidates
        .push(r"C:\Program Files\NDI\NDI 5 Runtime\v5\Processing.NDI.Lib.x64.dll".to_string());
    if let Some(path_value) = env::var_os("PATH") {
        for path_entry in env::split_paths(&path_value) {
            let candidate = path_entry.join("Processing.NDI.Lib.x64.dll");
            candidates.push(candidate.to_string_lossy().into_owned());
        }
    }
    candidates
}

#[cfg(target_os = "macos")]
fn ndi_library_candidates_macos() -> Vec<String> {
    let mut candidates = Vec::new();
    if let Ok(raw) = env::var("BROWSER_PORT_NDI_LIBRARY_PATH") {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            candidates.push(trimmed.to_string());
        }
    }
    candidates.push("/Library/NDI SDK for Apple/lib/macOS/libndi.dylib".to_string());
    candidates.push("/usr/local/lib/libndi.dylib".to_string());
    candidates.push("/opt/homebrew/lib/libndi.dylib".to_string());
    candidates.push("libndi.dylib".to_string());
    candidates.push("libndi.6.dylib".to_string());
    candidates.push("libndi.5.dylib".to_string());
    candidates.push("libndi.4.dylib".to_string());
    candidates
}

#[cfg(target_os = "macos")]
fn try_load_ndi_runtime_macos(path: &str) -> Result<(), String> {
    let c_path = CString::new(path).map_err(|_| "invalid dylib path".to_string())?;
    unsafe {
        libc::dlerror();
        let handle = libc::dlopen(c_path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
        if handle.is_null() {
            let err = libc::dlerror();
            if err.is_null() {
                return Err("dlopen failed with null error".to_string());
            }
            return Err(CStr::from_ptr(err).to_string_lossy().into_owned());
        }
        libc::dlclose(handle);
    }
    Ok(())
}

fn parse_env_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

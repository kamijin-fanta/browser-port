use crate::SharedState;
use anyhow::{anyhow, Context};
use image::imageops::FilterType;
use std::ffi::c_void;
use std::mem::size_of;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tokio::runtime::Handle;
use tokio::sync::RwLock;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_SETVERSION,
    NOTIFYICONDATAW, NOTIFYICONDATAW_0, NOTIFYICON_VERSION_4,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIcon, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon,
    DestroyMenu, DestroyWindow, DispatchMessageW, GetCursorPos, GetWindowLongPtrW, PeekMessageW,
    PostQuitMessage, RegisterClassW, SetForegroundWindow, SetWindowLongPtrW, TrackPopupMenu,
    TranslateMessage, CREATESTRUCTW, GWLP_USERDATA, HICON, HMENU, MENU_ITEM_FLAGS, MF_GRAYED,
    MF_SEPARATOR, MF_STRING, MSG, PM_REMOVE, TPM_RIGHTBUTTON, WINDOW_EX_STYLE, WINDOW_STYLE,
    WM_APP, WM_COMMAND, WM_CONTEXTMENU, WM_DESTROY, WM_LBUTTONUP, WM_NCCREATE, WM_QUIT,
    WM_RBUTTONUP, WNDCLASSW,
};

const TRAY_ICON_PNG: &[u8] = include_bytes!("../assets/tray_icon.png");
const TRAY_ICON_SIZE: u32 = 16;
const WM_TRAYICON: u32 = WM_APP + 1;
const TRAY_ICON_ID: u32 = 1;
const MENU_ID_QUIT: usize = 1001;
const PLAYER_COUNT: u32 = 4;

struct TrayContext {
    state: Arc<RwLock<SharedState>>,
    handle: Handle,
    bind_addr: String,
    stop: Arc<AtomicBool>,
    hicon: HICON,
}

#[derive(Clone, Default)]
struct PlayerMenuSnapshot {
    connected: bool,
    codec: Option<String>,
    coded_width: Option<u32>,
    coded_height: Option<u32>,
    fps: Option<f64>,
    bitrate: Option<f64>,
    tab_title: Option<String>,
    ndi_connected: Option<bool>,
    syphon_connected: Option<bool>,
}

pub fn run_tray_app(
    state: Arc<RwLock<SharedState>>,
    bind_addr: String,
    stop: Arc<AtomicBool>,
    handle: Handle,
) -> anyhow::Result<()> {
    unsafe {
        let module = GetModuleHandleW(None).context("failed to get module handle")?;
        let instance = HINSTANCE(module.0);
        let class_name = wide_null("BrowserPortTrayWindow");
        let window_name = wide_null("BrowserPortTrayWindow");

        let wc = WNDCLASSW {
            lpfnWndProc: Some(tray_wnd_proc),
            hInstance: instance,
            lpszClassName: PCWSTR(class_name.as_ptr()),
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            return Err(anyhow!(
                "RegisterClassW failed: {}",
                windows::core::Error::from_win32()
            ));
        }

        let hicon = load_tray_icon().context("failed to build tray icon from PNG")?;
        let tray_context = Box::new(TrayContext {
            state,
            handle,
            bind_addr,
            stop,
            hicon,
        });
        let tray_context_ptr = Box::into_raw(tray_context);

        let hwnd = match CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            PCWSTR(class_name.as_ptr()),
            PCWSTR(window_name.as_ptr()),
            WINDOW_STYLE::default(),
            0,
            0,
            0,
            0,
            HWND(null_mut()),
            HMENU(null_mut()),
            instance,
            Some(tray_context_ptr as *const c_void),
        ) {
            Ok(hwnd) => hwnd,
            Err(err) => {
                let _ = Box::from_raw(tray_context_ptr);
                return Err(anyhow!("CreateWindowExW failed: {err}"));
            }
        };

        if let Err(err) = add_tray_icon(hwnd, hicon) {
            let _ = DestroyWindow(hwnd);
            let _ = Box::from_raw(tray_context_ptr);
            return Err(err);
        }
        eprintln!("BrowserPort: Windows tray icon initialized");

        let mut quit_requested = false;
        let mut msg = MSG::default();
        loop {
            while PeekMessageW(&mut msg, HWND(null_mut()), 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    let _ = Box::from_raw(tray_context_ptr);
                    return Ok(());
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }

            if !quit_requested {
                if let Some(ctx) = tray_context_from_hwnd(hwnd) {
                    if ctx.stop.load(Ordering::Relaxed) {
                        let _ = DestroyWindow(hwnd);
                        quit_requested = true;
                    }
                } else {
                    quit_requested = true;
                }
            }

            thread::sleep(Duration::from_millis(50));
        }
    }
}

extern "system" fn tray_wnd_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        match message {
            WM_NCCREATE => {
                let create_struct = lparam.0 as *const CREATESTRUCTW;
                if create_struct.is_null() {
                    return LRESULT(0);
                }
                let ptr = (*create_struct).lpCreateParams as *mut TrayContext;
                if ptr.is_null() {
                    return LRESULT(0);
                }
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, ptr as isize);
                return LRESULT(1);
            }
            WM_COMMAND => {
                let command_id = (wparam.0 & 0xffff) as usize;
                if command_id == MENU_ID_QUIT {
                    if let Some(ctx) = tray_context_from_hwnd(hwnd) {
                        ctx.stop.store(true, Ordering::Relaxed);
                    }
                    let _ = DestroyWindow(hwnd);
                    return LRESULT(0);
                }
            }
            WM_TRAYICON => {
                // With NOTIFYICON_VERSION_4, lParam packs additional data; the
                // low word carries the mouse/window event id we need to match.
                let event = (lparam.0 as u32) & 0xFFFF;
                if matches!(event, WM_CONTEXTMENU | WM_RBUTTONUP | WM_LBUTTONUP) {
                    if let Some(ctx) = tray_context_from_hwnd(hwnd) {
                        if let Err(err) = show_tray_menu(hwnd, ctx) {
                            eprintln!("BrowserPort: failed to show tray menu: {err}");
                        }
                    }
                    return LRESULT(0);
                }
            }
            WM_DESTROY => {
                if let Some(ctx) = tray_context_from_hwnd(hwnd) {
                    let _ = remove_tray_icon(hwnd);
                    let _ = DestroyIcon(ctx.hicon);
                }
                PostQuitMessage(0);
                return LRESULT(0);
            }
            _ => {}
        }
        DefWindowProcW(hwnd, message, wparam, lparam)
    }
}

unsafe fn show_tray_menu(hwnd: HWND, ctx: &TrayContext) -> anyhow::Result<()> {
    let snapshot = ctx.handle.block_on(async {
        let lock = ctx.state.read().await;
        let mut players = Vec::with_capacity(PLAYER_COUNT as usize);
        for player_id in 1..=PLAYER_COUNT {
            let stream = lock.player_streams.get(&player_id);
            players.push((
                player_id,
                PlayerMenuSnapshot {
                    connected: stream.map(|s| s.connected).unwrap_or(false),
                    codec: stream.and_then(|s| s.codec.clone()),
                    coded_width: stream.and_then(|s| s.coded_width),
                    coded_height: stream.and_then(|s| s.coded_height),
                    fps: stream.and_then(|s| s.fps),
                    bitrate: stream.and_then(|s| s.bitrate),
                    tab_title: stream.and_then(|s| s.tab_title.clone()),
                    ndi_connected: stream.and_then(|s| s.ndi_connected),
                    syphon_connected: stream.and_then(|s| s.syphon_connected),
                },
            ));
        }
        (
            lock.player_routes.len(),
            format!("ws://{}", ctx.bind_addr),
            lock.outputs.ndi_enabled,
            lock.outputs.syphon_enabled,
            players,
        )
    });
    let players_title = format!("Players: {}", snapshot.0);
    let server_title = format!("Server: {}", snapshot.1);

    let menu = CreatePopupMenu().context("CreatePopupMenu failed")?;
    append_menu_text(menu, MF_STRING | MF_GRAYED, 0, &players_title)?;
    append_menu_text(menu, MF_STRING | MF_GRAYED, 0, &server_title)?;
    append_menu_text(menu, MF_STRING, MENU_ID_QUIT, "Quit BrowserPort")?;
    AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null()).context("AppendMenuW separator failed")?;

    for (player_id, player) in &snapshot.4 {
        if !player.connected {
            append_menu_text(
                menu,
                MF_STRING | MF_GRAYED,
                0,
                &format!("Player{player_id} Idle"),
            )?;
        } else {
            let outputs = format_player_outputs(player, snapshot.2, snapshot.3);
            let header = if outputs.is_empty() {
                format!("Player{player_id} Connected")
            } else {
                format!("Player{player_id} Connected        {outputs}")
            };
            let stream_line = format_stream_line(player);
            let tab_title = player.tab_title.as_deref().unwrap_or("N/A");

            append_menu_text(menu, MF_STRING | MF_GRAYED, 0, &header)?;
            append_menu_text(menu, MF_STRING | MF_GRAYED, 0, &stream_line)?;
            append_menu_text(menu, MF_STRING | MF_GRAYED, 0, tab_title)?;
        }

        if *player_id < PLAYER_COUNT {
            append_menu_text(menu, MF_STRING | MF_GRAYED, 0, " ")?;
        }
    }

    let mut point = POINT::default();
    GetCursorPos(&mut point).context("GetCursorPos failed")?;
    let _ = SetForegroundWindow(hwnd);
    let _ = TrackPopupMenu(menu, TPM_RIGHTBUTTON, point.x, point.y, 0, hwnd, None);
    DestroyMenu(menu).context("DestroyMenu failed")?;
    Ok(())
}

unsafe fn append_menu_text(
    menu: HMENU,
    flags: MENU_ITEM_FLAGS,
    id: usize,
    text: &str,
) -> anyhow::Result<()> {
    let wide = wide_null(text);
    AppendMenuW(menu, flags, id, PCWSTR(wide.as_ptr())).context("AppendMenuW failed")
}

unsafe fn tray_context_from_hwnd(hwnd: HWND) -> Option<&'static mut TrayContext> {
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut TrayContext;
    ptr.as_mut()
}

unsafe fn add_tray_icon(hwnd: HWND, hicon: HICON) -> anyhow::Result<()> {
    let mut notify_data = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ICON_ID,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: WM_TRAYICON,
        hIcon: hicon,
        ..Default::default()
    };
    copy_wide_truncated(&mut notify_data.szTip, "BrowserPort");
    if !Shell_NotifyIconW(NIM_ADD, &notify_data).as_bool() {
        return Err(anyhow!(
            "Shell_NotifyIconW(NIM_ADD) failed: {}",
            windows::core::Error::from_win32()
        ));
    }

    notify_data.Anonymous = NOTIFYICONDATAW_0 {
        uVersion: NOTIFYICON_VERSION_4,
    };
    let _ = Shell_NotifyIconW(NIM_SETVERSION, &notify_data);
    Ok(())
}

unsafe fn remove_tray_icon(hwnd: HWND) -> anyhow::Result<()> {
    let notify_data = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ICON_ID,
        ..Default::default()
    };
    if !Shell_NotifyIconW(NIM_DELETE, &notify_data).as_bool() {
        return Err(anyhow!(
            "Shell_NotifyIconW(NIM_DELETE) failed: {}",
            windows::core::Error::from_win32()
        ));
    }
    Ok(())
}

unsafe fn load_tray_icon() -> anyhow::Result<HICON> {
    let image = image::load_from_memory(TRAY_ICON_PNG).context("failed to decode tray_icon.png")?;
    let rgba = image
        .resize_exact(TRAY_ICON_SIZE, TRAY_ICON_SIZE, FilterType::Lanczos3)
        .into_rgba8();
    let (width, height) = rgba.dimensions();
    let mut xor_mask = Vec::with_capacity((width * height * 4) as usize);
    for pixel in rgba.pixels() {
        xor_mask.push(pixel[2]);
        xor_mask.push(pixel[1]);
        xor_mask.push(pixel[0]);
        xor_mask.push(pixel[3]);
    }
    let and_mask_len = (((width + 31) / 32) * 4 * height) as usize;
    let and_mask = vec![0u8; and_mask_len];
    let icon = CreateIcon(
        HINSTANCE(null_mut()),
        width as i32,
        height as i32,
        1,
        32,
        and_mask.as_ptr(),
        xor_mask.as_ptr(),
    )
    .context("CreateIcon failed")?;
    Ok(icon)
}

fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn copy_wide_truncated(buffer: &mut [u16], value: &str) {
    if buffer.is_empty() {
        return;
    }
    let max = buffer.len() - 1;
    for (index, unit) in value.encode_utf16().take(max).enumerate() {
        buffer[index] = unit;
    }
    buffer[max] = 0;
}

fn format_stream_line(player: &PlayerMenuSnapshot) -> String {
    let codec = player.codec.as_deref().unwrap_or("N/A");
    let resolution = match (player.coded_width, player.coded_height) {
        (Some(width), Some(height)) => format!("{width}x{height}"),
        _ => "N/A".to_string(),
    };
    let fps = player
        .fps
        .filter(|value| value.is_finite() && *value > 0.0)
        .map(|value| format!("{value:.1} fps"))
        .unwrap_or_else(|| "N/A fps".to_string());
    let bitrate = player
        .bitrate
        .filter(|value| value.is_finite() && *value >= 0.0)
        .map(|value| format!("{:.2} Mbps", value / 1_000_000.0))
        .unwrap_or_else(|| "N/A Mbps".to_string());
    format!("{codec} | {resolution} | {fps} | {bitrate}")
}

fn format_player_outputs(
    player: &PlayerMenuSnapshot,
    ndi_enabled: bool,
    syphon_enabled: bool,
) -> String {
    let ndi_connected = player.ndi_connected.unwrap_or(ndi_enabled);
    let syphon_connected = player.syphon_connected.unwrap_or(syphon_enabled);
    let mut outputs = Vec::new();
    if ndi_connected {
        outputs.push("NDI");
    }
    if syphon_connected {
        outputs.push("Syphon");
    }
    outputs.join(" ")
}

#![allow(deprecated)]

use crate::{app_version, SharedState};
use cocoa::appkit::{
    NSApp, NSApplication, NSApplicationActivationPolicyAccessory, NSVariableStatusItemLength,
};
use cocoa::base::{id, nil, NO, YES};
use cocoa::foundation::{NSSize, NSString};
use objc::declare::ClassDecl;
use objc::runtime::{Class, Object, Sel};
use objc::{class, msg_send, sel, sel_impl};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::runtime::Handle;
use tokio::sync::RwLock;

const REFRESH_INTERVAL_SECONDS: f64 = 1.0;
const TRAY_ICON_PNG: &[u8] = include_bytes!("../../icons/mac-tray.png");
const TRAY_ICON_WIDTH: f64 = 24.0;
const TRAY_ICON_HEIGHT: f64 = 18.0;
const PLAYER_COUNT: u32 = 4;

struct PlayerMenuItems {
    header: id,
    stream: id,
    tab_title: id,
    spacer: id,
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

struct TrayState {
    state: Arc<RwLock<SharedState>>,
    handle: Handle,
    bind_addr: String,
    stop: Arc<AtomicBool>,
    _status_item: id,
    players_item: id,
    server_item: id,
    version_item: id,
    player_items: Vec<PlayerMenuItems>,
}

impl TrayState {
    unsafe fn refresh_titles(&self) {
        let state = Arc::clone(&self.state);
        let bind_addr = self.bind_addr.clone();
        let snapshot = self.handle.block_on(async move {
            let lock = state.read().await;
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
                format!("ws://{}", bind_addr),
                lock.outputs.ndi_enabled,
                lock.outputs.syphon_enabled,
                players,
            )
        });

        let player_title = ns_string(&format!("Players: {}", snapshot.0));
        let server_title = ns_string(&format!("Server: {}", snapshot.1));
        let version_title = ns_string(&format!("Version: v{}", app_version()));
        let _: () = msg_send![self.players_item, setTitle: player_title];
        let _: () = msg_send![self.server_item, setTitle: server_title];
        let _: () = msg_send![self.version_item, setTitle: version_title];

        for (index, (player_id, player)) in snapshot.4.iter().enumerate() {
            let Some(items) = self.player_items.get(index) else {
                continue;
            };
            if !player.connected {
                let header = ns_string(&format!("Player{player_id} Idle"));
                let empty = ns_string("");
                let _: () = msg_send![items.header, setTitle: header];
                let _: () = msg_send![items.stream, setTitle: empty];
                let _: () = msg_send![items.tab_title, setTitle: empty];
                let _: () = msg_send![items.stream, setHidden: YES];
                let _: () = msg_send![items.tab_title, setHidden: YES];
                let spacer_hidden = if *player_id < PLAYER_COUNT { NO } else { YES };
                let _: () = msg_send![items.spacer, setHidden: spacer_hidden];
                continue;
            }

            let outputs = format_player_outputs(player, snapshot.2, snapshot.3);
            let header = if outputs.is_empty() {
                format!("Player{player_id} Connected")
            } else {
                format!("Player{player_id} Connected        {outputs}")
            };
            let stream_line = format_stream_line(player);
            let tab_title = player.tab_title.as_deref().unwrap_or("N/A");
            let _: () = msg_send![items.header, setTitle: ns_string(&header)];
            let _: () = msg_send![items.stream, setTitle: ns_string(&stream_line)];
            let _: () = msg_send![items.tab_title, setTitle: ns_string(tab_title)];
            let _: () = msg_send![items.stream, setHidden: NO];
            let _: () = msg_send![items.tab_title, setHidden: NO];
            let spacer_hidden = if *player_id < PLAYER_COUNT { NO } else { YES };
            let _: () = msg_send![items.spacer, setHidden: spacer_hidden];
        }
    }
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
    format!("{codec} ・ {resolution} ・ {fps} ・ {bitrate}")
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

unsafe fn set_hidden(item: id, hidden: bool) {
    if hidden {
        let _: () = msg_send![item, setHidden: YES];
    } else {
        let _: () = msg_send![item, setHidden: NO];
    }
}

unsafe fn ns_string(value: &str) -> id {
    NSString::alloc(nil).init_str(value)
}

unsafe fn menu_controller_class() -> &'static Class {
    static mut CLASS: *const Class = std::ptr::null();
    static ONCE: std::sync::Once = std::sync::Once::new();

    ONCE.call_once(|| {
        let superclass = Class::get("NSObject").expect("NSObject unavailable");
        let mut decl = ClassDecl::new("BrowserPortMenuController", superclass)
            .expect("failed to declare BrowserPortMenuController");
        decl.add_ivar::<*mut c_void>("state_ptr");
        decl.add_method(
            sel!(refreshMenu:),
            refresh_menu as extern "C" fn(&Object, Sel, id),
        );
        decl.add_method(sel!(quit:), quit_menu as extern "C" fn(&Object, Sel, id));
        CLASS = decl.register();
    });

    &*CLASS
}

extern "C" fn refresh_menu(this: &Object, _cmd: Sel, _sender: id) {
    unsafe {
        let state_ptr: *mut c_void = *this.get_ivar("state_ptr");
        if state_ptr.is_null() {
            return;
        }
        let tray_state = &*(state_ptr as *mut TrayState);
        if tray_state.stop.load(Ordering::Relaxed) {
            let app: id = NSApp();
            let _: () = msg_send![app, terminate: nil];
            return;
        }
        tray_state.refresh_titles();
    }
}

extern "C" fn quit_menu(this: &Object, _cmd: Sel, _sender: id) {
    unsafe {
        let state_ptr: *mut c_void = *this.get_ivar("state_ptr");
        if !state_ptr.is_null() {
            let tray_state = &*(state_ptr as *mut TrayState);
            tray_state.stop.store(true, Ordering::Relaxed);
        }
        let app: id = NSApp();
        let _: () = msg_send![app, terminate: nil];
    }
}

unsafe fn make_menu_item(title: &str, enabled: bool, target: id, action: Sel) -> id {
    let title = ns_string(title);
    let empty = ns_string("");
    let item: id = msg_send![class!(NSMenuItem), alloc];
    let item: id = msg_send![item, initWithTitle: title action: action keyEquivalent: empty];
    let _: () = msg_send![item, setTarget: target];
    let _: () = msg_send![item, setEnabled: if enabled { YES } else { NO }];
    item
}

unsafe fn load_tray_icon() -> id {
    let data: id = msg_send![
        class!(NSData),
        dataWithBytes: TRAY_ICON_PNG.as_ptr()
        length: TRAY_ICON_PNG.len()
    ];
    if data == nil {
        return nil;
    }

    let image: id = msg_send![class!(NSImage), alloc];
    let image: id = msg_send![image, initWithData: data];
    if image == nil {
        return nil;
    }

    let size = NSSize {
        width: TRAY_ICON_WIDTH,
        height: TRAY_ICON_HEIGHT,
    };
    let _: () = msg_send![image, setSize: size];
    let _: () = msg_send![image, setTemplate: NO];
    image
}

unsafe fn build_menu(controller: id) -> (id, id, id, id, id, Vec<PlayerMenuItems>) {
    let menu: id = msg_send![class!(NSMenu), new];
    let status_item: id = msg_send![class!(NSStatusBar), systemStatusBar];
    let status_item: id = msg_send![status_item, statusItemWithLength: NSVariableStatusItemLength];
    let button: id = msg_send![status_item, button];
    if button != nil {
        let tooltip = ns_string(&format!("BrowserPort v{}", app_version()));
        let _: () = msg_send![button, setToolTip: tooltip];
        let icon = load_tray_icon();
        if icon != nil {
            let _: () = msg_send![button, setImage: icon];
            let empty_title = ns_string("");
            let _: () = msg_send![button, setTitle: empty_title];
        } else {
            let fallback_title = ns_string("BrowserPort");
            let _: () = msg_send![button, setTitle: fallback_title];
            let _: () = msg_send![status_item, setTitle: fallback_title];
        }
    }

    let players_item = make_menu_item("Players: 0", false, nil, sel!(refreshMenu:));
    let server_item = make_menu_item("Server: ws://127.0.0.1:1844", false, nil, sel!(refreshMenu:));
    let version_item = make_menu_item(&format!("Version: v{}", app_version()), false, nil, sel!(refreshMenu:));
    let quit_item = make_menu_item("Quit BrowserPort", true, controller, sel!(quit:));
    let separator: id = msg_send![class!(NSMenuItem), separatorItem];

    let _: () = msg_send![menu, addItem: players_item];
    let _: () = msg_send![menu, addItem: server_item];
    let _: () = msg_send![menu, addItem: version_item];
    let _: () = msg_send![menu, addItem: quit_item];
    let _: () = msg_send![menu, addItem: separator];

    let mut player_items = Vec::with_capacity(PLAYER_COUNT as usize);
    for player_id in 1..=PLAYER_COUNT {
        let header = make_menu_item(
            &format!("Player{player_id} Idle"),
            false,
            nil,
            sel!(refreshMenu:),
        );
        let stream = make_menu_item("", false, nil, sel!(refreshMenu:));
        let tab_title = make_menu_item("", false, nil, sel!(refreshMenu:));
        let spacer = make_menu_item(" ", false, nil, sel!(refreshMenu:));
        set_hidden(stream, true);
        set_hidden(tab_title, true);
        if player_id == PLAYER_COUNT {
            set_hidden(spacer, true);
        }
        let _: () = msg_send![menu, addItem: header];
        let _: () = msg_send![menu, addItem: stream];
        let _: () = msg_send![menu, addItem: tab_title];
        let _: () = msg_send![menu, addItem: spacer];
        player_items.push(PlayerMenuItems {
            header,
            stream,
            tab_title,
            spacer,
        });
    }

    let _: () = msg_send![status_item, setMenu: menu];
    let _: () = msg_send![status_item, setHighlightMode: YES];
    (menu, status_item, players_item, server_item, version_item, player_items)
}

pub fn run_menu_bar_app(
    state: Arc<RwLock<SharedState>>,
    bind_addr: String,
    stop: Arc<AtomicBool>,
    handle: Handle,
) -> anyhow::Result<()> {
    unsafe {
        let app: id = NSApplication::sharedApplication(nil);
        if app == nil {
            return Err(anyhow::anyhow!("failed to create shared NSApplication"));
        }
        eprintln!("BrowserPort: initializing macOS status item");
        let _: () = msg_send![app, setActivationPolicy: NSApplicationActivationPolicyAccessory];
        let _: () = msg_send![app, finishLaunching];

        let controller_class = menu_controller_class();
        let controller: id = msg_send![controller_class, new];
        let (_menu, status_item, players_item, server_item, version_item, player_items) =
            build_menu(controller);
        eprintln!("BrowserPort: status item created");

        let tray_state = Box::new(TrayState {
            state,
            handle,
            bind_addr,
            stop,
            _status_item: status_item,
            players_item,
            server_item,
            version_item,
            player_items,
        });
        let tray_state_ptr = Box::into_raw(tray_state) as *mut c_void;
        (*controller).set_ivar("state_ptr", tray_state_ptr);

        let _: () = msg_send![controller, refreshMenu: nil];
        let timer: id = msg_send![
            class!(NSTimer),
            scheduledTimerWithTimeInterval: REFRESH_INTERVAL_SECONDS
            target: controller
            selector: sel!(refreshMenu:)
            userInfo: nil
            repeats: YES
        ];
        let _: () = msg_send![timer, fire];

        let _: () = msg_send![app, run];
    }
    Ok(())
}

use crate::SharedState;
use cocoa::appkit::{
    NSApp, NSApplication, NSApplicationActivationPolicyAccessory, NSVariableStatusItemLength,
};
use cocoa::base::{id, nil, NO, YES};
use cocoa::foundation::NSString;
use objc::declare::ClassDecl;
use objc::runtime::{Class, Object, Sel};
use objc::{class, msg_send, sel, sel_impl};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::runtime::Handle;
use tokio::sync::RwLock;

const REFRESH_INTERVAL_SECONDS: f64 = 1.0;

struct TrayState {
    state: Arc<RwLock<SharedState>>,
    handle: Handle,
    bind_addr: String,
    stop: Arc<AtomicBool>,
    status_item: id,
    player_item: id,
    syphon_item: id,
    ws_item: id,
}

impl TrayState {
    unsafe fn refresh_titles(&self) {
        let state = Arc::clone(&self.state);
        let bind_addr = self.bind_addr.clone();
        let snapshot = self.handle.block_on(async move {
            let lock = state.read().await;
            (lock.player_routes.len(), lock.syphon_client_count, bind_addr)
        });

        let status_title = ns_string("BrowserPort");
        let player_title = ns_string(&format!("Players: {}", snapshot.0));
        let syphon_title = ns_string(&format!(
            "Syphon: {}",
            format_connection_count(snapshot.1)
        ));
        let ws_title = ns_string(&format!("WS: {}", snapshot.2));

        let _: () = msg_send![self.status_item, setTitle: status_title];
        let _: () = msg_send![self.player_item, setTitle: player_title];
        let _: () = msg_send![self.syphon_item, setTitle: syphon_title];
        let _: () = msg_send![self.ws_item, setTitle: ws_title];
    }
}

fn format_connection_count(count: usize) -> String {
    if count == 0 {
        "0".to_string()
    } else {
        count.to_string()
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
        decl.add_method(sel!(refreshMenu:), refresh_menu as extern "C" fn(&Object, Sel, id));
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

unsafe fn build_menu(controller: id) -> (id, id, id, id, id) {
    let menu: id = msg_send![class!(NSMenu), new];
    let status_item: id = msg_send![class!(NSStatusBar), systemStatusBar];
    let status_item: id = msg_send![status_item, statusItemWithLength: NSVariableStatusItemLength];
    let status_title = ns_string("BrowserPort");
    let _: () = msg_send![status_item, setTitle: status_title];
    let button: id = msg_send![status_item, button];
    if button != nil {
        let _: () = msg_send![button, setTitle: status_title];
    }

    let player_item = make_menu_item("Players: 0", false, nil, sel!(refreshMenu:));
    let syphon_item = make_menu_item("Syphon: 0", false, nil, sel!(refreshMenu:));
    let ws_item = make_menu_item("WS: 127.0.0.1:9876", false, nil, sel!(refreshMenu:));
    let separator: id = msg_send![class!(NSMenuItem), separatorItem];
    let quit_item = make_menu_item("Quit BrowserPort", true, controller, sel!(quit:));

    let _: () = msg_send![menu, addItem: player_item];
    let _: () = msg_send![menu, addItem: syphon_item];
    let _: () = msg_send![menu, addItem: ws_item];
    let _: () = msg_send![menu, addItem: separator];
    let _: () = msg_send![menu, addItem: quit_item];
    let _: () = msg_send![status_item, setMenu: menu];
    let _: () = msg_send![status_item, setHighlightMode: YES];
    (menu, status_item, player_item, syphon_item, ws_item)
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
        let (_menu, status_item, player_item, syphon_item, ws_item) = build_menu(controller);
        eprintln!("BrowserPort: status item created");

        let tray_state = Box::new(TrayState {
            state,
            handle,
            bind_addr,
            stop,
            status_item,
            player_item,
            syphon_item,
            ws_item,
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

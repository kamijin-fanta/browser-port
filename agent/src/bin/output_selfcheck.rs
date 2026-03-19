#[cfg(target_os = "macos")]
mod macos_selfcheck {
    use std::ffi::{c_char, CStr, CString};
    use std::thread;
    use std::time::{Duration, Instant};

    #[repr(C)]
    struct BrowserPortSyphonSender {
        _private: [u8; 0],
    }

    #[repr(C)]
    struct BrowserPortSyphonClient {
        _private: [u8; 0],
    }

    unsafe extern "C" {
        fn browser_port_syphon_create_sender(name: *const c_char) -> *mut BrowserPortSyphonSender;
        fn browser_port_syphon_send_bgra(
            sender: *mut BrowserPortSyphonSender,
            bgra: *const u8,
            width: u32,
            height: u32,
        ) -> bool;
        fn browser_port_syphon_destroy_sender(sender: *mut BrowserPortSyphonSender);

        fn browser_port_syphon_create_client(name: *const c_char) -> *mut BrowserPortSyphonClient;
        fn browser_port_syphon_client_width(client: *mut BrowserPortSyphonClient) -> u32;
        fn browser_port_syphon_client_height(client: *mut BrowserPortSyphonClient) -> u32;
        fn browser_port_syphon_client_receive_bgra(
            client: *mut BrowserPortSyphonClient,
            bgra: *mut u8,
            width: u32,
            height: u32,
        ) -> bool;
        fn browser_port_syphon_destroy_client(client: *mut BrowserPortSyphonClient);

        fn browser_port_syphon_last_error() -> *const c_char;
    }

    pub fn main() {
        if let Err(err) = run() {
            eprintln!("output_selfcheck: {err}");
            std::process::exit(1);
        }
    }

    fn run() -> Result<(), String> {
        println!("output_selfcheck: running macOS Syphon roundtrip");

        let name = CString::new("codex-output-selfcheck-syphon").map_err(|err| err.to_string())?;
        let sender = unsafe { browser_port_syphon_create_sender(name.as_ptr()) };
        if sender.is_null() {
            return Err(format!("create_sender failed: {}", last_error()));
        }
        let _sender_guard = SenderGuard(sender);

        let client = unsafe { browser_port_syphon_create_client(name.as_ptr()) };
        if client.is_null() {
            return Err(format!("create_client failed: {}", last_error()));
        }
        let _client_guard = ClientGuard(client);

        let width = 32_u32;
        let height = 32_u32;
        let mut frame = vec![0_u8; width as usize * height as usize * 4];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let i = (y * width as usize + x) * 4;
                frame[i] = (x as u8).wrapping_mul(7);
                frame[i + 1] = (y as u8).wrapping_mul(9);
                frame[i + 2] = 0xC0;
                frame[i + 3] = 0xFF;
            }
        }

        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            let sent =
                unsafe { browser_port_syphon_send_bgra(sender, frame.as_ptr(), width, height) };
            if !sent {
                return Err(format!("send_bgra failed: {}", last_error()));
            }
            thread::sleep(Duration::from_millis(20));

            let rw = unsafe { browser_port_syphon_client_width(client) };
            let rh = unsafe { browser_port_syphon_client_height(client) };
            if rw == 0 || rh == 0 {
                continue;
            }
            let mut recv = vec![0_u8; rw as usize * rh as usize * 4];
            let ok = unsafe {
                browser_port_syphon_client_receive_bgra(client, recv.as_mut_ptr(), rw, rh)
            };
            if !ok {
                continue;
            }
            let non_black = recv
                .chunks_exact(4)
                .filter(|px| px[0] != 0 || px[1] != 0 || px[2] != 0)
                .count();
            let ratio = non_black as f64 / (recv.len() / 4).max(1) as f64;
            println!(
                "output_selfcheck: syphon receive ratio={ratio:.4} size={}x{}",
                rw, rh
            );
            if ratio > 0.01 {
                println!("output_selfcheck: ok");
                return Ok(());
            }
        }

        Err(format!(
            "timed out waiting for non-black Syphon frame: {}",
            last_error()
        ))
    }

    fn last_error() -> String {
        unsafe {
            let ptr = browser_port_syphon_last_error();
            if ptr.is_null() {
                return "(null)".to_string();
            }
            CStr::from_ptr(ptr).to_string_lossy().into_owned()
        }
    }

    struct SenderGuard(*mut BrowserPortSyphonSender);

    impl Drop for SenderGuard {
        fn drop(&mut self) {
            unsafe { browser_port_syphon_destroy_sender(self.0) };
        }
    }

    struct ClientGuard(*mut BrowserPortSyphonClient);

    impl Drop for ClientGuard {
        fn drop(&mut self) {
            unsafe { browser_port_syphon_destroy_client(self.0) };
        }
    }
}

#[cfg(target_os = "windows")]
fn main() {
    use std::path::PathBuf;
    use std::process::Command;

    let current_exe = match std::env::current_exe() {
        Ok(path) => path,
        Err(err) => {
            eprintln!("output_selfcheck: failed to locate current executable: {err}");
            std::process::exit(1);
        }
    };
    let helper = current_exe
        .parent()
        .map(|dir| dir.join("spout_selftest.exe"))
        .unwrap_or_else(|| PathBuf::from("spout_selftest.exe"));

    if !helper.exists() {
        eprintln!(
            "output_selfcheck: spout_selftest.exe not found at {}. build it with `cargo build --bin spout_selftest`.",
            helper.display()
        );
        std::process::exit(1);
    }

    let status = match Command::new(&helper).status() {
        Ok(status) => status,
        Err(err) => {
            eprintln!(
                "output_selfcheck: failed to execute {}: {err}",
                helper.display()
            );
            std::process::exit(1);
        }
    };

    if !status.success() {
        eprintln!("output_selfcheck: spout_selftest failed with status {status}");
        std::process::exit(status.code().unwrap_or(1));
    }
    println!("output_selfcheck: ok");
}

#[cfg(target_os = "macos")]
fn main() {
    macos_selfcheck::main();
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn main() {
    eprintln!("output_selfcheck is currently supported on macOS/Windows only");
    std::process::exit(1);
}

#[cfg(target_os = "macos")]
mod macos_selftest {
    use std::ffi::{c_char, CStr, CString};
    use std::env;
    use std::thread;
    use std::time::{Duration, Instant};

    struct SelftestConfig {
        sender_name: String,
        duration: Duration,
    }

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
            eprintln!("syphon_selftest: {err}");
            std::process::exit(1);
        }
    }

    fn run() -> Result<(), String> {
        let config = SelftestConfig::from_args();
        println!(
            "syphon_selftest sender={} duration_ms={}",
            config.sender_name,
            config.duration.as_millis()
        );
        let name = CString::new(config.sender_name.clone()).map_err(|err| err.to_string())?;
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

        let width = 64_u32;
        let height = 64_u32;
        let mut frame = vec![0_u8; width as usize * height as usize * 4];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let i = (y * width as usize + x) * 4;
                let on = ((x / 8) + (y / 8)) % 2 == 0;
                frame[i] = if on { 0x20 } else { 0xD0 };
                frame[i + 1] = if on { 0x90 } else { 0x30 };
                frame[i + 2] = if on { 0xF0 } else { 0x20 };
                frame[i + 3] = 0xFF;
            }
        }

        let deadline = Instant::now() + config.duration;
        let mut receive_attempt = 0_u32;
        let mut last_ok = Instant::now();
        while Instant::now() < deadline {
            let sent =
                unsafe { browser_port_syphon_send_bgra(sender, frame.as_ptr(), width, height) };
            println!("send_ok={sent}");
            if !sent {
                return Err(format!("send_bgra failed: {}", last_error()));
            }

            thread::sleep(Duration::from_millis(20));

            let recv_width = unsafe { browser_port_syphon_client_width(client) };
            let recv_height = unsafe { browser_port_syphon_client_height(client) };
            println!("client_size={}x{}", recv_width, recv_height);
            if recv_width == 0 || recv_height == 0 {
                thread::sleep(Duration::from_millis(20));
                continue;
            }

            let mut recv = vec![0_u8; recv_width as usize * recv_height as usize * 4];
            let received = unsafe {
                browser_port_syphon_client_receive_bgra(
                    client,
                    recv.as_mut_ptr(),
                    recv_width,
                    recv_height,
                )
            };
            receive_attempt = receive_attempt.saturating_add(1);
            println!("receive_ok[{receive_attempt}]={received}");
            if !received {
                thread::sleep(Duration::from_millis(20));
                continue;
            }

            if let Some((mean_luma, non_black_ratio, sample)) = analyze_frame(&recv) {
                println!(
                    "stats mean_luma={:.2} non_black_ratio={:.4} sample_bgra={:02X}{:02X}{:02X}{:02X}",
                    mean_luma,
                    non_black_ratio,
                    sample[0],
                    sample[1],
                    sample[2],
                    sample[3],
                );
                if non_black_ratio > 0.01 {
                    last_ok = Instant::now();
                }
            }

            thread::sleep(Duration::from_millis(20));
        }

        if last_ok.elapsed() <= config.duration {
            return Ok(());
        }
        Err(format!("timed out without non-black frame (last_error={})", last_error()))
    }

    impl SelftestConfig {
        fn from_args() -> Self {
            let mut args = env::args().skip(1);
            let sender_name = args
                .next()
                .unwrap_or_else(|| "codex-syphon-selftest".to_string());
            let duration_secs = args
                .next()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(8);
            Self {
                sender_name,
                duration: Duration::from_secs(duration_secs),
            }
        }
    }

    fn analyze_frame(bgra: &[u8]) -> Option<(f64, f64, [u8; 4])> {
        if bgra.len() < 4 {
            return None;
        }
        let mut sum_luma = 0.0_f64;
        let mut non_black = 0_u64;
        let mut sample = [0_u8; 4];
        for chunk in bgra.chunks_exact(4) {
            let b = f64::from(chunk[0]);
            let g = f64::from(chunk[1]);
            let r = f64::from(chunk[2]);
            sum_luma += 0.114 * b + 0.587 * g + 0.299 * r;
            if chunk[0] != 0 || chunk[1] != 0 || chunk[2] != 0 {
                non_black = non_black.saturating_add(1);
                if sample == [0, 0, 0, 0] {
                    sample.copy_from_slice(chunk);
                }
            }
        }
        let pixel_count = (bgra.len() / 4).max(1) as f64;
        Some((
            sum_luma / pixel_count,
            non_black as f64 / pixel_count,
            sample,
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

#[cfg(target_os = "macos")]
fn main() {
    macos_selftest::main();
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("syphon_selftest is only available on macOS");
    std::process::exit(1);
}

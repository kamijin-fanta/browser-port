#[cfg(target_os = "macos")]
mod macos_probe {
    use std::env;
    use std::ffi::{c_char, CStr, CString};
    use std::fs;
    use std::path::PathBuf;
    use std::thread;
    use std::time::{Duration, Instant};

    struct ProbeConfig {
        sender_name: String,
        timeout: Duration,
        output: PathBuf,
    }

    struct FrameStats {
        mean_luma: f64,
        non_black_ratio: f64,
        center_bgra: [u8; 4],
        first_non_black: Option<[u8; 4]>,
    }

    impl FrameStats {
        fn is_effectively_black(&self) -> bool {
            self.mean_luma < 1.0 && self.non_black_ratio < 0.001
        }
    }

    #[repr(C)]
    struct BrowserPortSyphonClient {
        _private: [u8; 0],
    }

    unsafe extern "C" {
        fn browser_port_syphon_create_client(name: *const c_char) -> *mut BrowserPortSyphonClient;
        fn browser_port_syphon_client_has_frame(client: *mut BrowserPortSyphonClient) -> bool;
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
            eprintln!("syphon_probe: {err:#}");
            std::process::exit(1);
        }
    }

    fn run() -> anyhow::Result<()> {
        let config = ProbeConfig::from_args();
        println!("sender={}", config.sender_name);
        println!("timeout_ms={}", config.timeout.as_millis());
        println!("output={}", config.output.display());

        let sender_name = CString::new(config.sender_name.clone())?;
        let client = unsafe { browser_port_syphon_create_client(sender_name.as_ptr()) };
        if client.is_null() {
            anyhow::bail!("failed to create syphon client: {}", last_error());
        }
        let _client_guard = ClientGuard(client);

        let started = Instant::now();
        let mut width = 0_u32;
        let mut height = 0_u32;
        let mut buffer = Vec::<u8>::new();
        let mut saved_frame = false;
        let mut received_frames = 0_u64;
        let mut last_report = Instant::now() - Duration::from_secs(1);

        while started.elapsed() < config.timeout {
            let has_frame = unsafe { browser_port_syphon_client_has_frame(client) };
            let recv_width = unsafe { browser_port_syphon_client_width(client) };
            let recv_height = unsafe { browser_port_syphon_client_height(client) };

            if recv_width == 0 || recv_height == 0 {
                if last_report.elapsed() >= Duration::from_millis(500) {
                    println!(
                        "waiting_for_sender has_frame={has_frame} error={}",
                        last_error()
                    );
                    last_report = Instant::now();
                }
                thread::sleep(Duration::from_millis(16));
                continue;
            }

            if recv_width != width || recv_height != height {
                width = recv_width;
                height = recv_height;
                let size = width as usize * height as usize * 4;
                buffer.resize(size, 0);
                println!("client_size={}x{}", width, height);
            }

            let received = unsafe {
                browser_port_syphon_client_receive_bgra(client, buffer.as_mut_ptr(), width, height)
            };
            if !received {
                if last_report.elapsed() >= Duration::from_millis(500) {
                    println!("receive_failed error={}", last_error());
                    last_report = Instant::now();
                }
                thread::sleep(Duration::from_millis(16));
                continue;
            }

            received_frames = received_frames.saturating_add(1);
            let stats = analyze_frame(&buffer, width as usize, height as usize);
            println!(
                "frame={} size={}x{} mean_luma={:.2} non_black_ratio={:.4} center_bgra={:02X}{:02X}{:02X}{:02X}",
                received_frames,
                width,
                height,
                stats.mean_luma,
                stats.non_black_ratio,
                stats.center_bgra[0],
                stats.center_bgra[1],
                stats.center_bgra[2],
                stats.center_bgra[3]
            );

            if !saved_frame {
                write_bmp(&config.output, &buffer, width as usize, height as usize)?;
                println!("saved_frame={}", config.output.display());
                saved_frame = true;
            }

            if !stats.is_effectively_black() {
                if let Some(sample) = stats.first_non_black {
                    println!(
                        "non_black_detected sample_bgra={:02X}{:02X}{:02X}{:02X}",
                        sample[0], sample[1], sample[2], sample[3]
                    );
                }
                return Ok(());
            }

            thread::sleep(Duration::from_millis(8));
        }

        anyhow::bail!(
            "timed out after {} ms without observing a non-black frame",
            config.timeout.as_millis()
        );
    }

    impl ProbeConfig {
        fn from_args() -> Self {
            let mut args = env::args().skip(1);
            let sender_name = args
                .next()
                .unwrap_or_else(|| "browser-port-syphon-1".to_string());
            let timeout_secs = args
                .next()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(10);
            let output = args
                .next()
                .map(PathBuf::from)
                .unwrap_or_else(|| env::temp_dir().join("syphon_probe.bmp"));
            Self {
                sender_name,
                timeout: Duration::from_secs(timeout_secs),
                output,
            }
        }
    }

    fn analyze_frame(bgra: &[u8], width: usize, height: usize) -> FrameStats {
        let mut sum_luma = 0.0_f64;
        let mut non_black = 0_u64;
        let mut first_non_black = None;

        for chunk in bgra.chunks_exact(4) {
            let b = chunk[0] as f64;
            let g = chunk[1] as f64;
            let r = chunk[2] as f64;
            let luma = 0.114 * b + 0.587 * g + 0.299 * r;
            sum_luma += luma;
            if chunk[0] != 0 || chunk[1] != 0 || chunk[2] != 0 {
                non_black = non_black.saturating_add(1);
                if first_non_black.is_none() {
                    first_non_black = Some([chunk[0], chunk[1], chunk[2], chunk[3]]);
                }
            }
        }

        let center_index = ((height / 2) * width + (width / 2)) * 4;
        let center_bgra = if center_index + 4 <= bgra.len() {
            [
                bgra[center_index],
                bgra[center_index + 1],
                bgra[center_index + 2],
                bgra[center_index + 3],
            ]
        } else {
            [0, 0, 0, 0]
        };

        let pixel_count = (width * height).max(1) as f64;
        FrameStats {
            mean_luma: sum_luma / pixel_count,
            non_black_ratio: non_black as f64 / pixel_count,
            center_bgra,
            first_non_black,
        }
    }

    fn write_bmp(path: &PathBuf, bgra: &[u8], width: usize, height: usize) -> anyhow::Result<()> {
        let row_stride = (width * 3 + 3) & !3;
        let image_size = row_stride * height;
        let file_size = 14 + 40 + image_size;
        let mut bytes = Vec::with_capacity(file_size);

        bytes.extend_from_slice(b"BM");
        bytes.extend_from_slice(&(file_size as u32).to_le_bytes());
        bytes.extend_from_slice(&0_u16.to_le_bytes());
        bytes.extend_from_slice(&0_u16.to_le_bytes());
        bytes.extend_from_slice(&(14_u32 + 40_u32).to_le_bytes());

        bytes.extend_from_slice(&40_u32.to_le_bytes());
        bytes.extend_from_slice(&(width as i32).to_le_bytes());
        bytes.extend_from_slice(&(height as i32).to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&24_u16.to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        bytes.extend_from_slice(&(image_size as u32).to_le_bytes());
        bytes.extend_from_slice(&2835_u32.to_le_bytes());
        bytes.extend_from_slice(&2835_u32.to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes());

        let padding = row_stride - width * 3;
        let zero_padding = vec![0_u8; padding];
        for row in (0..height).rev() {
            let row_start = row * width * 4;
            for pixel in bgra[row_start..row_start + width * 4].chunks_exact(4) {
                bytes.push(pixel[0]);
                bytes.push(pixel[1]);
                bytes.push(pixel[2]);
            }
            bytes.extend_from_slice(&zero_padding);
        }

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, bytes)?;
        Ok(())
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

    struct ClientGuard(*mut BrowserPortSyphonClient);

    impl Drop for ClientGuard {
        fn drop(&mut self) {
            unsafe {
                browser_port_syphon_destroy_client(self.0);
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn main() {
    macos_probe::main();
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("syphon_probe is only available on macOS");
    std::process::exit(1);
}

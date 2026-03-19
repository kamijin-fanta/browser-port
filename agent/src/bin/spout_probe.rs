#[cfg(target_os = "windows")]
mod windows_probe {
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

    fn run() -> anyhow::Result<()> {
        let config = ProbeConfig::from_args();
        println!("sender={}", config.sender_name);
        println!("timeout_ms={}", config.timeout.as_millis());
        println!("output={}", config.output.display());
        print_sender_list();
        match lookup_sender_info(&config.sender_name) {
            Some((width, height, share_handle, format)) => {
                println!(
                    "sender_info={}x{} share_handle=0x{:X} format={}",
                    width, height, share_handle, format
                );
            }
            None => {
                println!("sender_info=missing error={}", last_error());
            }
        }

        let sender_name = CString::new(config.sender_name.clone())?;
        let receiver = unsafe { browser_port_spout_create_receiver(sender_name.as_ptr()) };
        if receiver.is_null() {
            anyhow::bail!("failed to create receiver: {}", last_error());
        }

        let _receiver_guard = ReceiverGuard(receiver);
        let started = Instant::now();
        let mut width = 0_u32;
        let mut height = 0_u32;
        let mut buffer = Vec::<u8>::new();
        let mut saved_frame = false;
        let mut received_new_frames = 0_u64;
        let mut last_report = Instant::now() - Duration::from_secs(1);

        while started.elapsed() < config.timeout {
            let fallback_info = lookup_sender_info(&config.sender_name);
            let sender_width = unsafe { browser_port_spout_receiver_width(receiver) };
            let sender_height = unsafe { browser_port_spout_receiver_height(receiver) };
            let connected = unsafe { browser_port_spout_receiver_is_connected(receiver) };
            let effective_width = if sender_width > 0 {
                sender_width
            } else {
                fallback_info.map(|info| info.0).unwrap_or(0)
            };
            let effective_height = if sender_height > 0 {
                sender_height
            } else {
                fallback_info.map(|info| info.1).unwrap_or(0)
            };

            if effective_width == 0 || effective_height == 0 {
                if last_report.elapsed() >= Duration::from_millis(500) {
                    println!("waiting_for_sender connected={connected}");
                    last_report = Instant::now();
                }
                thread::sleep(Duration::from_millis(16));
                continue;
            }

            if effective_width != width || effective_height != height {
                width = effective_width;
                height = effective_height;
                let size = width as usize * height as usize * 4;
                buffer.resize(size, 0);
                println!("receiver_size={}x{} connected={connected}", width, height);
            }

            let received = unsafe {
                browser_port_spout_receiver_receive_bgra(
                    receiver,
                    buffer.as_mut_ptr(),
                    width,
                    height,
                )
            };
            let frame_new = unsafe { browser_port_spout_receiver_is_frame_new(receiver) };

            if !received {
                if last_report.elapsed() >= Duration::from_millis(500) {
                    println!(
                        "receive_failed connected={connected} error={}",
                        last_error()
                    );
                    last_report = Instant::now();
                }
                thread::sleep(Duration::from_millis(16));
                continue;
            }

            if !frame_new {
                thread::sleep(Duration::from_millis(8));
                continue;
            }

            received_new_frames += 1;
            let stats = analyze_frame(&buffer, width as usize, height as usize);
            println!(
                "frame={} size={}x{} mean_luma={:.2} non_black_ratio={:.4} center_bgra={:02X}{:02X}{:02X}{:02X}",
                received_new_frames,
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
                .unwrap_or_else(|| "browser-port-spout-1".to_string());
            let timeout_secs = args
                .next()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(10);
            let output = args
                .next()
                .map(PathBuf::from)
                .unwrap_or_else(|| env::temp_dir().join("spout_probe.bmp"));
            Self {
                sender_name,
                timeout: Duration::from_secs(timeout_secs),
                output,
            }
        }
    }

    fn print_sender_list() {
        let count = unsafe { browser_port_spout_sender_count() };
        println!("sender_count={count}");
        for index in 0..count {
            let mut buf = vec![0_i8; 256];
            let ok = unsafe {
                browser_port_spout_sender_name(index, buf.as_mut_ptr(), buf.len() as i32)
            };
            if ok {
                let name = unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy();
                println!("sender[{index}]={name}");
            }
        }
    }

    fn lookup_sender_info(name: &str) -> Option<(u32, u32, u64, u32)> {
        let sender_name = CString::new(name).ok()?;
        let mut width = 0_u32;
        let mut height = 0_u32;
        let mut share_handle = 0_u64;
        let mut format = 0_u32;
        let ok = unsafe {
            browser_port_spout_sender_info(
                sender_name.as_ptr(),
                &mut width,
                &mut height,
                &mut share_handle,
                &mut format,
            )
        };
        if ok {
            Some((width, height, share_handle, format))
        } else {
            None
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
                non_black += 1;
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
            let ptr = browser_port_spout_last_error();
            if ptr.is_null() {
                return "(null)".to_string();
            }
            CStr::from_ptr(ptr).to_string_lossy().into_owned()
        }
    }

    struct ReceiverGuard(*mut BrowserPortSpoutReceiver);

    impl Drop for ReceiverGuard {
        fn drop(&mut self) {
            unsafe {
                browser_port_spout_destroy_receiver(self.0);
            }
        }
    }

    #[repr(C)]
    struct BrowserPortSpoutReceiver {
        _private: [u8; 0],
    }

    unsafe extern "C" {
        fn browser_port_spout_last_error() -> *const c_char;
        fn browser_port_spout_sender_count() -> i32;
        fn browser_port_spout_sender_name(index: i32, name: *mut c_char, max_len: i32) -> bool;
        fn browser_port_spout_sender_info(
            name: *const c_char,
            width: *mut u32,
            height: *mut u32,
            share_handle: *mut u64,
            format: *mut u32,
        ) -> bool;
        fn browser_port_spout_create_receiver(name: *const c_char)
            -> *mut BrowserPortSpoutReceiver;
        fn browser_port_spout_receiver_is_connected(
            receiver: *mut BrowserPortSpoutReceiver,
        ) -> bool;
        fn browser_port_spout_receiver_is_frame_new(
            receiver: *mut BrowserPortSpoutReceiver,
        ) -> bool;
        fn browser_port_spout_receiver_width(receiver: *mut BrowserPortSpoutReceiver) -> u32;
        fn browser_port_spout_receiver_height(receiver: *mut BrowserPortSpoutReceiver) -> u32;
        fn browser_port_spout_receiver_receive_bgra(
            receiver: *mut BrowserPortSpoutReceiver,
            bgra: *mut u8,
            width: u32,
            height: u32,
        ) -> bool;
        fn browser_port_spout_destroy_receiver(receiver: *mut BrowserPortSpoutReceiver);
    }

    pub fn main() {
        if let Err(err) = run() {
            eprintln!("spout_probe: {err:#}");
            std::process::exit(1);
        }
    }
}

#[cfg(target_os = "windows")]
fn main() {
    windows_probe::main();
}

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("spout_probe is only available on Windows");
    std::process::exit(1);
}

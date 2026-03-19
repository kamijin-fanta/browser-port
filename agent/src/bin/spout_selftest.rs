#[cfg(target_os = "windows")]
mod windows_selftest {
    use std::ffi::{c_char, CStr, CString};
    use std::thread;
    use std::time::Duration;

    const DXGI_FORMAT_B8G8R8A8_UNORM: u32 = 87;

    #[repr(C)]
    struct BrowserPortSpoutSender {
        _private: [u8; 0],
    }

    #[repr(C)]
    struct BrowserPortSpoutReceiver {
        _private: [u8; 0],
    }

    unsafe extern "C" {
        fn browser_port_spout_create_sender(name: *const c_char) -> *mut BrowserPortSpoutSender;
        fn browser_port_spout_send_bgra(
            sender: *mut BrowserPortSpoutSender,
            bgra: *const u8,
            width: u32,
            height: u32,
        ) -> bool;
        fn browser_port_spout_debug_read_sender_bgra(
            sender: *mut BrowserPortSpoutSender,
            bgra: *mut u8,
            width: u32,
            height: u32,
        ) -> bool;
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
        fn browser_port_spout_create_receiver(name: *const c_char) -> *mut BrowserPortSpoutReceiver;
        fn browser_port_spout_receiver_width(receiver: *mut BrowserPortSpoutReceiver) -> u32;
        fn browser_port_spout_receiver_height(receiver: *mut BrowserPortSpoutReceiver) -> u32;
        fn browser_port_spout_receiver_receive_bgra(
            receiver: *mut BrowserPortSpoutReceiver,
            bgra: *mut u8,
            width: u32,
            height: u32,
        ) -> bool;
        fn browser_port_spout_destroy_sender(sender: *mut BrowserPortSpoutSender);
        fn browser_port_spout_destroy_receiver(receiver: *mut BrowserPortSpoutReceiver);
    }

    pub fn main() {
        if let Err(err) = run() {
            eprintln!("spout_selftest: {err}");
            std::process::exit(1);
        }
    }

    fn run() -> Result<(), String> {
        let name = CString::new("codex-spout-selftest").map_err(|err| err.to_string())?;
        let sender = unsafe { browser_port_spout_create_sender(name.as_ptr()) };
        if sender.is_null() {
            return Err(format!("create_sender failed: {}", last_error()));
        }
        let _sender_guard = SenderGuard(sender);

        let width = 64_u32;
        let height = 64_u32;
        let mut frame = vec![0_u8; width as usize * height as usize * 4];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let i = (y * width as usize + x) * 4;
                let on = ((x / 8) + (y / 8)) % 2 == 0;
                frame[i] = if on { 0x10 } else { 0xF0 };
                frame[i + 1] = if on { 0x80 } else { 0x20 };
                frame[i + 2] = if on { 0xF0 } else { 0x10 };
                frame[i + 3] = 0xFF;
            }
        }

        let warmup_sent = unsafe { browser_port_spout_send_bgra(sender, frame.as_ptr(), width, height) };
        println!("send_ok[warmup]={warmup_sent}");
        if !warmup_sent {
            return Err(format!("send_bgra failed: {}", last_error()));
        }
        thread::sleep(Duration::from_millis(20));
        let mut sender_readback = vec![0_u8; width as usize * height as usize * 4];
        let sender_read_ok = unsafe {
            browser_port_spout_debug_read_sender_bgra(sender, sender_readback.as_mut_ptr(), width, height)
        };
        println!("sender_read_ok={sender_read_ok}");
        if !sender_read_ok {
            return Err(format!("sender readback failed: {}", last_error()));
        }
        println!(
            "sender_sample_bgra={:02X}{:02X}{:02X}{:02X}",
            sender_readback[0], sender_readback[1], sender_readback[2], sender_readback[3]
        );

        let sender_count = unsafe { browser_port_spout_sender_count() };
        println!("sender_count={sender_count}");
        for i in 0..sender_count {
            let mut buf = vec![0_i8; 256];
            let ok = unsafe { browser_port_spout_sender_name(i, buf.as_mut_ptr(), buf.len() as i32) };
            println!("sender_name_ok[{i}]={ok}");
            if ok {
                let value = unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy();
                println!("sender[{i}]={value}");
            }
        }
        let mut info_width = 0_u32;
        let mut info_height = 0_u32;
        let mut share_handle = 0_u64;
        let mut format = 0_u32;
        let info_ok = unsafe {
            browser_port_spout_sender_info(
                name.as_ptr(),
                &mut info_width,
                &mut info_height,
                &mut share_handle,
                &mut format,
            )
        };
        println!(
            "sender_info_ok={} size={}x{} share_handle=0x{:X} format={}",
            info_ok, info_width, info_height, share_handle, format
        );
        if !info_ok {
            return Err(format!("sender_info failed: {}", last_error()));
        }
        if format != DXGI_FORMAT_B8G8R8A8_UNORM {
            return Err(format!(
                "unexpected sender format: {} (expected {})",
                format, DXGI_FORMAT_B8G8R8A8_UNORM
            ));
        }

        let receiver = unsafe { browser_port_spout_create_receiver(name.as_ptr()) };
        if receiver.is_null() {
            return Err(format!("create_receiver failed: {}", last_error()));
        }
        let _receiver_guard = ReceiverGuard(receiver);

        let recv_width = unsafe { browser_port_spout_receiver_width(receiver) }.max(info_width);
        let recv_height = unsafe { browser_port_spout_receiver_height(receiver) }.max(info_height);
        println!("receiver_size={}x{}", recv_width, recv_height);
        if recv_width == 0 || recv_height == 0 {
            return Err("receiver did not observe sender dimensions".to_string());
        }

        let mut recv = vec![0_u8; recv_width as usize * recv_height as usize * 4];
        for frame_index in 0..4 {
            let sent = unsafe { browser_port_spout_send_bgra(sender, frame.as_ptr(), width, height) };
            println!("send_ok[{frame_index}]={sent}");
            if !sent {
                return Err(format!("send_bgra failed: {}", last_error()));
            }
            thread::sleep(Duration::from_millis(20));
            let received = unsafe {
                browser_port_spout_receiver_receive_bgra(
                    receiver,
                    recv.as_mut_ptr(),
                    recv_width,
                    recv_height,
                )
            };
            println!("receive_ok[{frame_index}]={received}");
            if !received {
                return Err(format!("receive_bgra failed: {}", last_error()));
            }
            thread::sleep(Duration::from_millis(20));
        }

        let sample = &recv[..4];
        println!(
            "sample_bgra={:02X}{:02X}{:02X}{:02X}",
            sample[0], sample[1], sample[2], sample[3]
        );

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

    struct SenderGuard(*mut BrowserPortSpoutSender);

    impl Drop for SenderGuard {
        fn drop(&mut self) {
            unsafe { browser_port_spout_destroy_sender(self.0) };
        }
    }

    struct ReceiverGuard(*mut BrowserPortSpoutReceiver);

    impl Drop for ReceiverGuard {
        fn drop(&mut self) {
            unsafe { browser_port_spout_destroy_receiver(self.0) };
        }
    }
}

#[cfg(target_os = "windows")]
fn main() {
    windows_selftest::main();
}

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("spout_selftest is only available on Windows");
    std::process::exit(1);
}

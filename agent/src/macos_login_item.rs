#![allow(deprecated)]

use anyhow::{anyhow, bail};
use cocoa::base::{id, nil, BOOL, YES};
use objc::runtime::Class;
use objc::{msg_send, sel, sel_impl};
use std::ffi::CStr;
use std::os::raw::c_char;

const STATUS_ENABLED: i64 = 1;

pub fn register_main_app_login_item() -> anyhow::Result<()> {
    unsafe {
        let service_class = Class::get("SMAppService")
            .ok_or_else(|| anyhow!("SMAppService is unavailable (requires macOS 13+)"))?;
        let service: id = msg_send![service_class, mainAppService];
        if service == nil {
            bail!("SMAppService.mainAppService returned nil");
        }

        let status: i64 = msg_send![service, status];
        if status == STATUS_ENABLED {
            return Ok(());
        }

        let mut error: id = nil;
        let registered: BOOL = msg_send![service, registerAndReturnError: &mut error];
        if registered == YES {
            return Ok(());
        }

        bail!("SMAppService registration failed: {}", describe_error(error));
    }
}

unsafe fn describe_error(error: id) -> String {
    if error == nil {
        return "unknown error".to_string();
    }

    let domain: id = msg_send![error, domain];
    let code: isize = msg_send![error, code];
    let localized_description: id = msg_send![error, localizedDescription];
    format!(
        "domain={}, code={}, message={}",
        ns_string(domain),
        code,
        ns_string(localized_description)
    )
}

unsafe fn ns_string(value: id) -> String {
    if value == nil {
        return "<nil>".to_string();
    }
    let utf8: *const c_char = msg_send![value, UTF8String];
    if utf8.is_null() {
        return "<null-utf8>".to_string();
    }
    CStr::from_ptr(utf8).to_string_lossy().into_owned()
}

//! A last-resort way to tell the user startup failed. In a release build there's no
//! console window (see `main.rs`), so without this a startup error would otherwise be
//! silent apart from the log file.

#[link(name = "user32")]
extern "system" {
    fn MessageBoxW(hwnd: *mut std::ffi::c_void, text: *const u16, caption: *const u16, utype: u32) -> i32;
}

const MB_ICONERROR: u32 = 0x10;

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Show a blocking native "Tea Telemetry" message box with `message`.
pub fn show_fatal_error(message: &str) {
    let text = to_wide(message);
    let caption = to_wide("Tea Telemetry");
    unsafe {
        MessageBoxW(std::ptr::null_mut(), text.as_ptr(), caption.as_ptr(), MB_ICONERROR);
    }
}

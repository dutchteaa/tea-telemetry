//! Real shared-memory access to iRacing via Win32 (kernel32).

use super::source::SharedMem;
use std::ffi::c_void;

type Handle = *mut c_void;

const FILE_MAP_READ: u32 = 0x0004;
const SYNCHRONIZE: u32 = 0x0010_0000;

#[repr(C)]
struct MemoryBasicInformation {
    base_address: *mut c_void,
    allocation_base: *mut c_void,
    allocation_protect: u32,
    partition_id: u16,
    region_size: usize,
    state: u32,
    protect: u32,
    kind: u32,
}

#[link(name = "kernel32")]
extern "system" {
    fn OpenFileMappingW(desired_access: u32, inherit: i32, name: *const u16) -> Handle;
    fn MapViewOfFile(mapping: Handle, access: u32, off_high: u32, off_low: u32, bytes: usize) -> *mut c_void;
    fn UnmapViewOfFile(base: *const c_void) -> i32;
    fn VirtualQuery(addr: *const c_void, info: *mut MemoryBasicInformation, len: usize) -> usize;
    fn OpenEventW(desired_access: u32, inherit: i32, name: *const u16) -> Handle;
    fn WaitForSingleObject(handle: Handle, millis: u32) -> u32;
    fn CloseHandle(handle: Handle) -> i32;
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

pub struct WinSharedMem {
    mapping: Handle,
    view: *const u8,
    size: usize,
    event: Handle,
}

/// Open iRacing's telemetry memory map, or None if the sim isn't running.
pub fn connect() -> Option<WinSharedMem> {
    unsafe {
        let mapping = OpenFileMappingW(FILE_MAP_READ, 0, wide("Local\\IRSDKMemMapFileName").as_ptr());
        if mapping.is_null() {
            return None;
        }
        let view = MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, 0);
        if view.is_null() {
            CloseHandle(mapping);
            return None;
        }
        let mut info: MemoryBasicInformation = std::mem::zeroed();
        let ok = VirtualQuery(view, &mut info, std::mem::size_of::<MemoryBasicInformation>());
        if ok == 0 {
            UnmapViewOfFile(view);
            CloseHandle(mapping);
            return None;
        }
        let event = OpenEventW(SYNCHRONIZE, 0, wide("Local\\IRSDKDataValidEvent").as_ptr());
        Some(WinSharedMem { mapping, view: view as *const u8, size: info.region_size, event })
    }
}

impl SharedMem for WinSharedMem {
    fn read(&self, offset: usize, len: usize) -> Option<Vec<u8>> {
        let end = offset.checked_add(len)?;
        if end > self.size {
            return None;
        }
        let mut out = vec![0u8; len];
        // The sim writes this memory concurrently; torn reads are detected by the caller
        // re-checking the buffer's tick count after copying.
        unsafe { std::ptr::copy_nonoverlapping(self.view.add(offset), out.as_mut_ptr(), len) };
        Some(out)
    }

    fn wait_for_data(&self, timeout_ms: u32) {
        if self.event.is_null() {
            std::thread::sleep(std::time::Duration::from_millis(16));
        } else {
            unsafe { WaitForSingleObject(self.event, timeout_ms) };
        }
    }
}

impl Drop for WinSharedMem {
    fn drop(&mut self) {
        unsafe {
            UnmapViewOfFile(self.view as *const c_void);
            CloseHandle(self.mapping);
            if !self.event.is_null() {
                CloseHandle(self.event);
            }
        }
    }
}

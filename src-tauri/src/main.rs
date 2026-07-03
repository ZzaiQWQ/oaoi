// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(|s| s.as_str()) == Some("--apply-update") {
        apply_update(&args);
        return;
    }

    let Some(_single_instance_guard) = acquire_single_instance() else {
        return;
    };

    oaoi_lib::run();
}

#[cfg(windows)]
struct SingleInstanceGuard {
    handle: Option<*mut std::ffi::c_void>,
}

#[cfg(windows)]
impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            unsafe {
                let _ = CloseHandle(handle);
            }
        }
    }
}

#[cfg(windows)]
fn acquire_single_instance() -> Option<SingleInstanceGuard> {
    const ERROR_ALREADY_EXISTS: u32 = 183;

    let mutex_name = wide_null("Local\\OAOI_LAUNCHER_SINGLE_INSTANCE");
    let handle = unsafe { CreateMutexW(std::ptr::null_mut(), 1, mutex_name.as_ptr()) };
    if handle.is_null() {
        return Some(SingleInstanceGuard { handle: None });
    }

    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        bring_existing_window_to_front();
        unsafe {
            let _ = CloseHandle(handle);
        }
        return None;
    }

    Some(SingleInstanceGuard {
        handle: Some(handle),
    })
}

#[cfg(not(windows))]
struct SingleInstanceGuard;

#[cfg(not(windows))]
fn acquire_single_instance() -> Option<SingleInstanceGuard> {
    Some(SingleInstanceGuard)
}

#[cfg(windows)]
fn bring_existing_window_to_front() {
    let title = wide_null("oaoi - Minecraft启动器");
    unsafe {
        let hwnd = FindWindowW(std::ptr::null(), title.as_ptr());
        if hwnd.is_null() {
            return;
        }
        // 已有窗口可能被最小化，先恢复窗口，再把它放到前台。
        let _ = ShowWindow(hwnd, 9);
        let _ = SetForegroundWindow(hwnd);
    }
}

#[cfg(windows)]
fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
extern "system" {
    fn CreateMutexW(
        lpMutexAttributes: *mut std::ffi::c_void,
        bInitialOwner: i32,
        lpName: *const u16,
    ) -> *mut std::ffi::c_void;
    fn GetLastError() -> u32;
    fn CloseHandle(hObject: *mut std::ffi::c_void) -> i32;
    fn FindWindowW(lpClassName: *const u16, lpWindowName: *const u16) -> *mut std::ffi::c_void;
    fn ShowWindow(hWnd: *mut std::ffi::c_void, nCmdShow: i32) -> i32;
    fn SetForegroundWindow(hWnd: *mut std::ffi::c_void) -> i32;
}

fn apply_update(args: &[String]) {
    let Some(target) = args.get(2).map(std::path::PathBuf::from) else {
        return;
    };
    let Some(new_exe) = args.get(3).map(std::path::PathBuf::from) else {
        return;
    };
    let pid = args.get(4).and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);

    wait_for_process_exit(pid);

    for _ in 0..60 {
        if std::fs::copy(&new_exe, &target).is_ok() {
            let _ = std::process::Command::new(&target).spawn();
            let _ = std::fs::remove_file(&new_exe);
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

#[cfg(windows)]
fn wait_for_process_exit(pid: u32) {
    if pid == 0 {
        return;
    }

    const SYNCHRONIZE: u32 = 0x00100000;
    const INFINITE: u32 = 0xFFFFFFFF;

    extern "system" {
        fn OpenProcess(
            dwDesiredAccess: u32,
            bInheritHandle: i32,
            dwProcessId: u32,
        ) -> *mut std::ffi::c_void;
        fn WaitForSingleObject(hHandle: *mut std::ffi::c_void, dwMilliseconds: u32) -> u32;
        fn CloseHandle(hObject: *mut std::ffi::c_void) -> i32;
    }

    unsafe {
        let handle = OpenProcess(SYNCHRONIZE, 0, pid);
        if handle.is_null() {
            return;
        }
        let _ = WaitForSingleObject(handle, INFINITE);
        let _ = CloseHandle(handle);
    }
}

#[cfg(not(windows))]
fn wait_for_process_exit(_pid: u32) {
    std::thread::sleep(std::time::Duration::from_secs(1));
}

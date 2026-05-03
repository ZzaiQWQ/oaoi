// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(|s| s.as_str()) == Some("--apply-update") {
        apply_update(&args);
        return;
    }

    oaoi_lib::run();
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

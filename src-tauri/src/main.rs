// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(|s| s.as_str()) == Some("--apply-update") {
        if let Err(error) = apply_update(&args) {
            if let Some(target) = args.get(2) {
                let _ = std::process::Command::new(target).spawn();
            }
            show_update_error(&error);
        }
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

fn apply_update(args: &[String]) -> Result<(), String> {
    let target = args
        .get(2)
        .map(std::path::PathBuf::from)
        .ok_or_else(|| "更新参数缺少目标程序路径".to_string())?;
    let new_exe = args
        .get(3)
        .map(std::path::PathBuf::from)
        .ok_or_else(|| "更新参数缺少新版本路径".to_string())?;
    let pid = args.get(4).and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);

    wait_for_process_exit(pid)?;

    if !new_exe.is_file() {
        return Err(format!("找不到已下载的新版本: {}", new_exe.display()));
    }
    let file_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "目标程序文件名无效".to_string())?;
    let parent = target
        .parent()
        .ok_or_else(|| "目标程序目录无效".to_string())?;
    let staged = parent.join(format!("{}.oaoi_update_staged", file_name));
    let backup = parent.join(format!("{}.oaoi_update_backup", file_name));

    // 上次更新中断时优先恢复旧程序，再开始新的替换。
    if !target.exists() && backup.exists() {
        rename_with_retry(&backup, &target).map_err(|e| format!("恢复上次更新备份失败: {}", e))?;
    }
    let _ = std::fs::remove_file(&staged);
    if target.exists() {
        let _ = std::fs::remove_file(&backup);
    }

    copy_with_retry(&new_exe, &staged).map_err(|e| format!("准备新版本文件失败: {}", e))?;
    if let Err(error) = replace_with_backup(&target, &staged, &backup) {
        let _ = std::fs::remove_file(&staged);
        if !target.exists() && backup.exists() {
            let restore = rename_with_retry(&backup, &target);
            return match restore {
                Ok(()) => Err(format!("替换新版本失败，已恢复旧版本: {}", error)),
                Err(restore_error) => Err(format!(
                    "替换新版本失败: {}；恢复旧版本也失败: {}",
                    error, restore_error
                )),
            };
        }
        return Err(format!("替换新版本失败，旧版本未改动: {}", error));
    }

    let health_marker = new_exe.with_file_name("oaoi-update-ready");
    let _ = std::fs::remove_file(&health_marker);
    let mut child = match std::process::Command::new(&target)
        .env("OAOI_UPDATE_HEALTH_MARKER", &health_marker)
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            let restore = restore_update_backup(&target, &backup);
            return match restore {
                Ok(()) => Err(format!("启动新版本失败，已恢复旧版本: {}", error)),
                Err(restore_error) => Err(format!(
                    "启动新版本失败: {}；恢复旧版本也失败: {}",
                    error, restore_error
                )),
            };
        }
    };

    match wait_for_update_health(&mut child, &health_marker) {
        Ok(()) => {}
        Err(error) => {
            // 进程提前退出时也统一等待一次，确保 Windows 已释放程序文件。
            let _ = child.wait();
            let restore = restore_update_backup(&target, &backup);
            return match restore {
                Ok(()) => Err(format!("新版本未能正常启动，已恢复旧版本: {}", error)),
                Err(restore_error) => Err(format!(
                    "新版本未能正常启动: {}；恢复旧版本也失败: {}",
                    error, restore_error
                )),
            };
        }
    }

    let _ = std::fs::remove_file(&backup);
    let _ = std::fs::remove_file(&health_marker);
    let _ = std::fs::remove_file(&new_exe);
    Ok(())
}

fn wait_for_update_health(
    child: &mut std::process::Child,
    marker: &std::path::Path,
) -> Result<(), String> {
    let started = std::time::Instant::now();
    loop {
        if std::fs::read(marker).is_ok_and(|content| content == b"ready") {
            return Ok(());
        }
        match child.try_wait() {
            Ok(Some(status)) => return Err(format!("进程提前退出: {}", status)),
            Ok(None) => {}
            Err(error) => {
                terminate_update_child(child);
                return Err(format!("检查新版本进程失败: {}", error));
            }
        }
        if started.elapsed() >= std::time::Duration::from_secs(120) {
            // 没有健康确认就不能提交更新，先终止新进程，再由上层恢复旧版本。
            terminate_update_child(child);
            return Err("等待新版本健康确认超时".to_string());
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

fn terminate_update_child(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn restore_update_backup(target: &std::path::Path, backup: &std::path::Path) -> Result<(), String> {
    if !backup.is_file() {
        return Err(format!("找不到旧版本备份: {}", backup.display()));
    }
    remove_file_with_retry(target)?;
    rename_with_retry(backup, target)
}

fn remove_file_with_retry(path: &std::path::Path) -> Result<(), String> {
    let mut last_error = None;
    for _ in 0..60 {
        match std::fs::remove_file(path) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => last_error = Some(error),
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    Err(last_error
        .map(|error| error.to_string())
        .unwrap_or_else(|| "未知删除错误".to_string()))
}

fn copy_with_retry(source: &std::path::Path, target: &std::path::Path) -> Result<(), String> {
    let mut last_error = None;
    for _ in 0..60 {
        match std::fs::copy(source, target) {
            Ok(_) => {
                let file = std::fs::OpenOptions::new()
                    .write(true)
                    .open(target)
                    .map_err(|e| e.to_string())?;
                file.sync_all().map_err(|e| e.to_string())?;
                return Ok(());
            }
            Err(error) => last_error = Some(error),
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    Err(last_error
        .map(|error| error.to_string())
        .unwrap_or_else(|| "未知复制错误".to_string()))
}

fn rename_with_retry(source: &std::path::Path, target: &std::path::Path) -> Result<(), String> {
    let mut last_error = None;
    for _ in 0..60 {
        match std::fs::rename(source, target) {
            Ok(()) => return Ok(()),
            Err(error) => last_error = Some(error),
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    Err(last_error
        .map(|error| error.to_string())
        .unwrap_or_else(|| "未知替换错误".to_string()))
}

#[cfg(windows)]
fn replace_with_backup(
    target: &std::path::Path,
    replacement: &std::path::Path,
    backup: &std::path::Path,
) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;

    extern "system" {
        fn ReplaceFileW(
            replaced_file_name: *const u16,
            replacement_file_name: *const u16,
            backup_file_name: *const u16,
            replace_flags: u32,
            exclude: *mut std::ffi::c_void,
            reserved: *mut std::ffi::c_void,
        ) -> i32;
    }

    let wide = |path: &std::path::Path| {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>()
    };
    let target = wide(target);
    let replacement = wide(replacement);
    let backup = wide(backup);
    let result = unsafe {
        ReplaceFileW(
            target.as_ptr(),
            replacement.as_ptr(),
            backup.as_ptr(),
            1,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if result == 0 {
        Err(std::io::Error::last_os_error().to_string())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_with_backup(
    target: &std::path::Path,
    replacement: &std::path::Path,
    backup: &std::path::Path,
) -> Result<(), String> {
    std::fs::rename(target, backup).map_err(|e| e.to_string())?;
    if let Err(error) = std::fs::rename(replacement, target) {
        let _ = std::fs::rename(backup, target);
        return Err(error.to_string());
    }
    Ok(())
}

#[cfg(windows)]
fn show_update_error(message: &str) {
    extern "system" {
        fn MessageBoxW(
            window: *mut std::ffi::c_void,
            text: *const u16,
            caption: *const u16,
            kind: u32,
        ) -> i32;
    }

    let text = wide_null(&format!("更新失败，已尝试恢复旧版本。\n\n{}", message));
    let caption = wide_null("oaoi 更新失败");
    unsafe {
        let _ = MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            caption.as_ptr(),
            0x10 | 0x40000,
        );
    }
}

#[cfg(not(windows))]
fn show_update_error(message: &str) {
    eprintln!("更新失败，已尝试恢复旧版本: {}", message);
}

#[cfg(windows)]
fn wait_for_process_exit(pid: u32) -> Result<(), String> {
    if pid == 0 {
        return Ok(());
    }

    const SYNCHRONIZE: u32 = 0x00100000;
    const WAIT_OBJECT_0: u32 = 0;
    const WAIT_TIMEOUT: u32 = 0x102;

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
            return Ok(());
        }
        let wait_result = WaitForSingleObject(handle, 120_000);
        let _ = CloseHandle(handle);
        match wait_result {
            WAIT_OBJECT_0 => Ok(()),
            WAIT_TIMEOUT => Err("等待旧启动器退出超时".to_string()),
            other => Err(format!("等待旧启动器退出失败: {}", other)),
        }
    }
}

#[cfg(not(windows))]
fn wait_for_process_exit(_pid: u32) -> Result<(), String> {
    std::thread::sleep(std::time::Duration::from_secs(1));
    Ok(())
}

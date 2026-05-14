mod proxy;
mod punch;
mod tunnel;

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::str::FromStr;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter};
use tokio_util::sync::CancellationToken;

/// 向前端发日志，永远不会 panic
fn emit_log(app: &AppHandle, msg: impl Into<String>) {
    let _ = app.emit("log", msg.into());
}

pub struct AppState {
    /// 每次 step1 创建的 socket，按公网地址索引（支持多访客并发）
    pending_sockets: Mutex<HashMap<String, UdpSocket>>,
    /// LAN 广播取消令牌
    lan_broadcast_cancel: Mutex<Option<CancellationToken>>,
    /// 所有活跃的代理任务取消令牌（支持多条隧道并存）
    proxy_cancels: Mutex<Vec<CancellationToken>>,
}

#[tauri::command]
pub async fn step1_get_ip(state: tauri::State<'_, AppState>) -> Result<String, String> {
    let (socket, addr) = punch::get_public_address().await?;
    let key = addr.to_string();
    state
        .pending_sockets
        .lock()
        .unwrap()
        .insert(key.clone(), socket);
    Ok(key)
}

/// 自动侦测本机 MC 局域网广播端口（房主用）
#[tauri::command]
pub async fn detect_mc_port(app: AppHandle) -> Result<u16, String> {
    emit_log(&app, "正在扫描本机 MC 局域网广播...");

    tokio::task::spawn_blocking(|| {
        let socket = std::net::UdpSocket::bind("0.0.0.0:4445")
            .map_err(|e| format!("绑定 4445 端口失败（可能被占用）: {}", e))?;

        socket
            .join_multicast_v4(&Ipv4Addr::new(224, 0, 2, 60), &Ipv4Addr::UNSPECIFIED)
            .map_err(|e| format!("加入组播组失败: {}", e))?;

        socket
            .set_read_timeout(Some(std::time::Duration::from_millis(800)))
            .map_err(|e| e.to_string())?;

        let mut buf = [0u8; 1024];
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);

        while std::time::Instant::now() < deadline {
            match socket.recv_from(&mut buf) {
                Ok((len, _)) => {
                    let msg = String::from_utf8_lossy(&buf[..len]);

                    // 忽略本程序访客代理发出的 LAN 广播，避免切换到房主时侦测到旧代理端口。
                    if msg.contains("MC Quantum Link (P2P)") {
                        continue;
                    }

                    if let Some(start) = msg.find("[AD]") {
                        if let Some(end) = msg.find("[/AD]") {
                            let port_str = &msg[start + 4..end];
                            if let Ok(port) = port_str.parse::<u16>() {
                                println!("[MC侦测] 发现 MC 局域网端口: {}", port);
                                return Ok(port);
                            }
                        }
                    }
                }
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut => {}
                Err(e) => return Err(format!("读取 MC 局域网广播失败: {}", e)),
            }
        }

        Err("10 秒内未检测到真实 MC 局域网广播，请先在游戏中打开「对局域网开放」".to_string())
    })
    .await
    .map_err(|e| format!("spawn_blocking 失败: {}", e))?
}

#[tauri::command]
pub async fn host_step2_connect(
    guest_ip: String,
    mc_port: u16,
    stun_addr: String,
    state: tauri::State<'_, AppState>,
    app: AppHandle,
) -> Result<(), String> {
    // 房主模式：用 stun_addr 取出对应的 socket（每个访客独立）
    emit_log(&app, format!("房主开始连接访客: {}", guest_ip));

    let socket = {
        state
            .pending_sockets
            .lock()
            .unwrap()
            .remove(&stun_addr)
            .ok_or(format!("socket {} 已过期，请重试", stun_addr))?
    };

    let guest_addr = SocketAddr::from_str(&guest_ip).map_err(|e| e.to_string())?;
    emit_log(&app, "开始用同一端口向对方盲发打洞包...");
    let (punched_socket, actual_guest_addr) = punch::hole_punch(socket, guest_addr).await?;
    emit_log(&app, format!("实际访客打洞地址: {}", actual_guest_addr));
    emit_log(&app, "NAT 洞口突破成功！");
    let std_socket = punched_socket.into_std().map_err(|e| e.to_string())?;
    let endpoint = tunnel::build_quic_server(std_socket)?;
    emit_log(&app, "QUIC 加密隧道建立！等待访客 TLS 握手...");

    let cancel_token = {
        let token = CancellationToken::new();
        let child = token.clone();
        state.proxy_cancels.lock().unwrap().push(token);
        child
    };
    let app_clone = app.clone();
    tokio::spawn(async move {
        let proxy_cancel = cancel_token.clone();
        let proxy_fut = proxy::start_host_proxy(endpoint, mc_port, app_clone.clone(), proxy_cancel);
        tokio::pin!(proxy_fut);
        tokio::select! {
            _ = cancel_token.cancelled() => {
                println!("[TCP代理] 房主代理已被取消");
            }
            result = &mut proxy_fut => {
                if let Err(e) = result {
                    let _ = app_clone.emit("log", format!("[错误] 房主代理崩溃: {}", e));
                    println!("[错误] 房主代理崩溃: {}", e);
                }
            }
        }
    });
    Ok(())
}

#[tauri::command]
pub async fn guest_step2_connect(
    host_ip: String,
    local_port: u16,
    stun_addr: String,
    state: tauri::State<'_, AppState>,
    app: AppHandle,
) -> Result<(), String> {
    // 访客模式：取消旧代理（访客同一时间只连一个房主）
    cancel_all_proxies(&state);

    emit_log(&app, format!("访客开始连接房主: {}", host_ip));
    let socket = {
        state
            .pending_sockets
            .lock()
            .unwrap()
            .remove(&stun_addr)
            .ok_or(format!("socket {} 已过期，请重试", stun_addr))?
    };
    let host_addr = SocketAddr::from_str(&host_ip).map_err(|e| e.to_string())?;
    emit_log(&app, "开始用同一端口向对方盲发打洞包...");
    let (punched_socket, actual_host_addr) = punch::hole_punch(socket, host_addr).await?;
    emit_log(&app, format!("实际房主打洞地址: {}", actual_host_addr));
    emit_log(&app, "NAT 洞口突破成功！");
    let std_socket = punched_socket.into_std().map_err(|e| e.to_string())?;
    let endpoint = tunnel::build_quic_client(std_socket)?;
    emit_log(&app, "正在进行 TLS 1.3 极速握手...");
    let connection = endpoint
        .connect(actual_host_addr, "minecraft.p2p")
        .map_err(|e| e.to_string())?
        .await
        .map_err(|e| e.to_string())?;
    emit_log(&app, "TLS 握手成功！QUIC 隧道已就绪。");

    // 先绑定端口，拿到实际端口号
    // local_port=0 表示自动查找，否则用用户指定的端口
    let (listener, actual_port) = proxy::bind_guest_listener(local_port).await?;
    emit_log(&app, format!("本地代理端口: {}", actual_port));

    let cancel_token = {
        let token = CancellationToken::new();
        let child = token.clone();
        state.proxy_cancels.lock().unwrap().push(token);
        child
    };
    let app_clone = app.clone();
    tokio::spawn(async move {
        let proxy_cancel = cancel_token.clone();
        let proxy_fut =
            proxy::run_guest_proxy(listener, connection, app_clone.clone(), proxy_cancel);
        tokio::pin!(proxy_fut);
        tokio::select! {
            _ = cancel_token.cancelled() => {
                println!("[TCP代理] 访客代理已被取消");
            }
            _ = &mut proxy_fut => {
                let _ = app_clone.emit("log", "[TCP代理] 访客代理已退出".to_string());
            }
        }
    });

    // 取消旧的 LAN 广播任务，启动新的（使用实际端口）
    {
        let mut cancel_guard = state.lan_broadcast_cancel.lock().unwrap();
        if let Some(old_token) = cancel_guard.take() {
            old_token.cancel();
        }
        let new_token = CancellationToken::new();
        let child_token = new_token.clone();
        *cancel_guard = Some(new_token);

        tokio::spawn(async move {
            let multicast_socket = match tokio::net::UdpSocket::bind("0.0.0.0:0").await {
                Ok(s) => s,
                Err(e) => {
                    println!("[LAN广播] 绑定失败: {}", e);
                    return;
                }
            };
            // 用实际绑定的端口，不再写死 25565
            let payload = format!("[MOTD]MC Quantum Link (P2P)[/MOTD][AD]{}[/AD]", actual_port);
            loop {
                tokio::select! {
                    _ = child_token.cancelled() => {
                        println!("[LAN广播] 已停止");
                        return;
                    }
                    _ = async {
                        let _ = multicast_socket.send_to(payload.as_bytes(), "224.0.2.60:4445").await;
                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    } => {}
                }
            }
        });
    }

    emit_log(&app, "======== 联机准备完毕 ========");
    emit_log(
        &app,
        format!(
            ">> 游戏内局域网列表将自动刷出房间（端口 {}）！ <<",
            actual_port
        ),
    );
    Ok(())
}

/// 取消所有代理任务（用于重置/退出时）
#[tauri::command]
pub fn reset_connections(state: tauri::State<'_, AppState>) {
    cancel_all_proxies(&state);
    // 清理未使用的 pending sockets（防止内存泄漏）
    state.pending_sockets.lock().unwrap().clear();
    // 取消 LAN 广播
    let mut lan_guard = state.lan_broadcast_cancel.lock().unwrap();
    if let Some(token) = lan_guard.take() {
        token.cancel();
    }
}

fn cancel_all_proxies(state: &AppState) {
    let mut guard = state.proxy_cancels.lock().unwrap();
    for token in guard.drain(..) {
        token.cancel();
    }
}

pub fn init_state() -> AppState {
    AppState {
        pending_sockets: Mutex::new(HashMap::new()),
        lan_broadcast_cancel: Mutex::new(None),
        proxy_cancels: Mutex::new(Vec::new()),
    }
}

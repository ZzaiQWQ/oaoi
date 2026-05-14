use quinn::{Connection, Endpoint};
use tauri::{AppHandle, Emitter};
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

fn emit_log(app: &AppHandle, msg: impl Into<String>) {
    let _ = app.emit("log", msg.into());
}

/// 双向搬运字节：TCP ↔ QUIC，任一方向结束即正确关闭另一方向
async fn bridge_streams(
    mut quic_send: quinn::SendStream,
    mut quic_recv: quinn::RecvStream,
    tcp_stream: TcpStream,
    cancel_token: CancellationToken,
) {
    let (mut tcp_read, mut tcp_write) = tcp_stream.into_split();

    tokio::select! {
        _ = cancel_token.cancelled() => {
            println!("[TCP代理] 数据流收到取消信号，正在关闭");
        }
        // TCP → QUIC 方向先结束（玩家断开 TCP）
        result = tokio::io::copy(&mut tcp_read, &mut quic_send) => {
            match result {
                Ok(bytes) => println!("[TCP代理] TCP→QUIC 传输完成 ({} bytes)", bytes),
                Err(e) => println!("[TCP代理] TCP→QUIC 传输错误: {}", e),
            }
            let _ = quic_send.finish();
            let drain = tokio::io::copy(&mut quic_recv, &mut tcp_write);
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), drain).await;
            let _ = tcp_write.shutdown().await;
        }
        // QUIC → TCP 方向先结束（远端关闭了流）
        result = tokio::io::copy(&mut quic_recv, &mut tcp_write) => {
            match result {
                Ok(bytes) => println!("[TCP代理] QUIC→TCP 传输完成 ({} bytes)", bytes),
                Err(e) => println!("[TCP代理] QUIC→TCP 传输错误: {}", e),
            }
            let _ = tcp_write.shutdown().await;
            let drain = tokio::io::copy(&mut tcp_read, &mut quic_send);
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), drain).await;
            let _ = quic_send.finish();
        }
    }
}

// ============== 房主端：接收 QUIC 连接，转发至本地真正的 MC 端口 ==============
pub async fn start_host_proxy(
    endpoint: Endpoint,
    mc_port: u16,
    app: AppHandle,
    cancel_token: CancellationToken,
) -> Result<(), String> {
    println!("[TCP代理] 房主端等待远端访客的 QUIC 连接...");
    emit_log(&app, "[TCP代理] 房主端已启动，等待访客数据流");

    loop {
        let accept_cancel = cancel_token.clone();
        tokio::select! {
            _ = accept_cancel.cancelled() => {
                endpoint.close(0u32.into(), b"cancelled");
                println!("[TCP代理] 房主端代理收到取消信号");
                break;
            }
            incoming = endpoint.accept() => {
                let Some(incoming) = incoming else {
                    break;
                };

                let app_for_connection = app.clone();
                let connection_cancel = cancel_token.clone();
                tokio::spawn(async move {
                    match incoming.await {
                        Ok(connection) => {
                            println!("[TCP代理] 新访客已连入 QUIC 隧道，客户端地址: {}", connection.remote_address());
                            println!("[TCP代理] 开始监听该访客的数据流，目标本地 MC 端口: {}", mc_port);
                            emit_log(&app_for_connection, format!("[TCP代理] 访客 QUIC 已连入: {}", connection.remote_address()));

                            loop {
                                let wait_cancel = connection_cancel.clone();
                                tokio::select! {
                                    _ = wait_cancel.cancelled() => {
                                        connection.close(0u32.into(), b"cancelled");
                                        println!("[TCP代理] 访客 QUIC 连接收到取消信号");
                                        break;
                                    }
                                    stream = connection.accept_bi() => {
                                        match stream {
                                            Ok((quic_send, quic_recv)) => {
                                                println!("[TCP代理] 收到访客发来的 TCP 握手映射请求");
                                                emit_log(&app_for_connection, format!("[TCP代理] 收到访客游戏连接，转发到 127.0.0.1:{}", mc_port));

                                                match tokio::time::timeout(
                                                    std::time::Duration::from_secs(5),
                                                    TcpStream::connect(format!("127.0.0.1:{}", mc_port)),
                                                ).await {
                                                    Ok(Ok(tcp_stream)) => {
                                                        emit_log(&app_for_connection, format!("[TCP代理] 已连接房主本地 MC 端口: {}", mc_port));
                                                        let app_for_stream = app_for_connection.clone();
                                                        let stream_cancel = connection_cancel.clone();
                                                        tokio::spawn(async move {
                                                            bridge_streams(quic_send, quic_recv, tcp_stream, stream_cancel).await;
                                                            emit_log(&app_for_stream, "[TCP代理] 一条玩家数据流已结束");
                                                            println!("[TCP代理] 一条玩家数据流已结束 (Player Disconnected)");
                                                        });
                                                    }
                                                    Ok(Err(e)) => {
                                                        emit_log(&app_for_connection, format!("[TCP代理] 无法连接房主本地 MC 端口 {}: {}", mc_port, e));
                                                        println!("[TCP代理] 拒绝访客流量：无法连接到本地内网 MC 端口: {}", e);
                                                    }
                                                    Err(_) => {
                                                        emit_log(&app_for_connection, format!("[TCP代理] 连接房主本地 MC 端口 {} 超时", mc_port));
                                                        println!("[TCP代理] 连接本地内网 MC 端口超时: {}", mc_port);
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                println!("[TCP代理] 接受双向流失败 (该访客可能已退出游戏或掉线): {:?}", e);
                                                emit_log(&app_for_connection, format!("[TCP代理] 访客数据流已断开: {:?}", e));
                                                break;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            println!("[TCP代理] QUIC 连接握手建立失败: {}", e);
                            emit_log(&app_for_connection, format!("[TCP代理] QUIC 连接握手失败: {}", e));
                        }
                    }
                });
            }
        }
    }

    Ok(())
}

// ============== 访客端：绑定本地端口 ==============
// preferred_port: 用户自定义端口，0 表示自动查找
pub async fn bind_guest_listener(preferred_port: u16) -> Result<(TcpListener, u16), String> {
    // 如果前端给了端口，优先尝试；失败后再退回常用端口，避免一次占用就直接失败。
    if preferred_port > 0 {
        match TcpListener::bind(format!("0.0.0.0:{}", preferred_port)).await {
            Ok(listener) => {
                println!("[TCP代理] 访客端绑定用户指定端口: {}", preferred_port);
                return Ok((listener, preferred_port));
            }
            Err(e) => {
                println!(
                    "[TCP代理] 无法绑定优先端口 {}，将尝试备用端口: {}",
                    preferred_port, e
                );
            }
        }
    }

    // 自动/备用模式：优先 25565，再扫描临近端口。
    for port in 25565..=25575 {
        if port == preferred_port {
            continue;
        }
        match TcpListener::bind(format!("0.0.0.0:{}", port)).await {
            Ok(listener) => {
                println!("[TCP代理] 访客端自动绑定端口: {}", port);
                return Ok((listener, port));
            }
            Err(_) => continue,
        }
    }
    Err("无法绑定本地端口 25565-25575（全部被占用）".to_string())
}

// ============== 访客端：运行代理循环 ==============
pub async fn run_guest_proxy(
    listener: TcpListener,
    connection: Connection,
    app: AppHandle,
    cancel_token: CancellationToken,
) {
    println!("[TCP代理] 访客端代理已启动！");
    emit_log(&app, "[TCP代理] 访客本地代理已启动，等待 Minecraft 连接");

    loop {
        let accept_cancel = cancel_token.clone();
        tokio::select! {
            _ = accept_cancel.cancelled() => {
                connection.close(0u32.into(), b"cancelled");
                println!("[TCP代理] 访客端代理收到取消信号");
                break;
            }
            accepted = listener.accept() => {
                match accepted {
                    Ok((tcp_stream, addr)) => {
                        println!("[TCP代理] 检测到游戏客户端连入，正在打通 QUIC 隧道...");
                        emit_log(&app, format!("[TCP代理] Minecraft 已连接本地代理: {}，正在打开 QUIC 数据流", addr));

                        let connection_clone = connection.clone();
                        let app_for_stream = app.clone();
                        let stream_cancel = cancel_token.clone();
                        tokio::spawn(async move {
                            let wait_cancel = stream_cancel.clone();
                            tokio::select! {
                                _ = wait_cancel.cancelled() => {
                                    println!("[TCP代理] 游戏连接在打开 QUIC 流前被取消");
                                }
                                opened = connection_clone.open_bi() => {
                                    match opened {
                                        Ok((quic_send, quic_recv)) => {
                                            emit_log(&app_for_stream, "[TCP代理] QUIC 数据流已打开，开始转发游戏数据");
                                            bridge_streams(quic_send, quic_recv, tcp_stream, stream_cancel).await;
                                            emit_log(&app_for_stream, "[TCP代理] 一条游戏连接已结束");
                                            println!("[TCP代理] 一条游戏连接已结束");
                                        }
                                        Err(e) => {
                                            emit_log(&app_for_stream, format!("[TCP代理] 无法打开 QUIC 双向流: {}", e));
                                            println!("[TCP代理] 无法打开 QUIC 双向流: {}", e);
                                        }
                                    }
                                }
                            }
                        });
                    }
                    Err(e) => {
                        emit_log(&app, format!("[TCP代理] 接受游戏本地 TCP 连接失败: {}", e));
                        println!("[TCP代理] 接受游戏本地 TCP 连接失败: {}", e);
                    }
                }
            }
        }
    }
}

use rand::Rng;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::UdpSocket as TokioUdpSocket;

// 构建简单的 STUN Binding Request (RFC 5389)
fn build_stun_request() -> ([u8; 12], Vec<u8>) {
    let mut pkt = vec![0; 20];
    pkt[0] = 0x00;
    pkt[1] = 0x01; // Binding Request
                   // length (2 bytes) = 0
                   // magic cookie (4 bytes)
    pkt[4] = 0x21;
    pkt[5] = 0x12;
    pkt[6] = 0xA4;
    pkt[7] = 0x42;
    // transaction ID (12 bytes)
    let mut txn_id = [0u8; 12];
    rand::thread_rng().fill(&mut txn_id);
    pkt[8..20].copy_from_slice(&txn_id);
    (txn_id, pkt)
}

// 解析 STUN Binding Response，同时支持 XOR-MAPPED-ADDRESS 和 MAPPED-ADDRESS
// 增加 transaction ID 校验，防止误接受无关响应
fn parse_stun_response(resp: &[u8], expected_txn_id: &[u8; 12]) -> Option<SocketAddr> {
    if resp.len() < 20 {
        return None;
    }
    if resp[0] != 0x01 || resp[1] != 0x01 {
        return None;
    } // Not a Binding Response

    // 校验 magic cookie
    if resp[4..8] != [0x21, 0x12, 0xA4, 0x42] {
        return None;
    }

    // 校验 transaction ID
    if resp[8..20] != expected_txn_id[..] {
        return None;
    }

    let mut offset = 20;
    let mut fallback_addr: Option<SocketAddr> = None; // 用来存 MAPPED-ADDRESS（优先级低）

    while offset + 4 <= resp.len() {
        let attr_type = u16::from_be_bytes([resp[offset], resp[offset + 1]]);
        let attr_len = u16::from_be_bytes([resp[offset + 2], resp[offset + 3]]) as usize;
        offset += 4;

        if offset + attr_len > resp.len() {
            break; // 防止越界
        }

        match attr_type {
            // XOR-MAPPED-ADDRESS (优先使用)
            0x0020 => {
                let family = resp[offset + 1];
                let port = u16::from_be_bytes([resp[offset + 2], resp[offset + 3]]) ^ 0x2112;
                if family == 0x01 && attr_len >= 8 {
                    // IPv4
                    let ip0 = resp[offset + 4] ^ 0x21;
                    let ip1 = resp[offset + 5] ^ 0x12;
                    let ip2 = resp[offset + 6] ^ 0xA4;
                    let ip3 = resp[offset + 7] ^ 0x42;
                    return Some(SocketAddr::new(
                        std::net::IpAddr::V4(std::net::Ipv4Addr::new(ip0, ip1, ip2, ip3)),
                        port,
                    ));
                }
            }
            // MAPPED-ADDRESS (兼容旧 RFC 3489 服务器)
            0x0001 => {
                let family = resp[offset + 1];
                let port = u16::from_be_bytes([resp[offset + 2], resp[offset + 3]]);
                if family == 0x01 && attr_len >= 8 {
                    // IPv4
                    let ip0 = resp[offset + 4];
                    let ip1 = resp[offset + 5];
                    let ip2 = resp[offset + 6];
                    let ip3 = resp[offset + 7];
                    fallback_addr = Some(SocketAddr::new(
                        std::net::IpAddr::V4(std::net::Ipv4Addr::new(ip0, ip1, ip2, ip3)),
                        port,
                    ));
                }
            }
            _ => {}
        }

        // RFC 5389 要求属性按 4 字节边界对齐
        let padded_len = (attr_len + 3) & !3;
        offset += padded_len;
    }

    // 如果没找到 XOR-MAPPED-ADDRESS，退而求其次用 MAPPED-ADDRESS
    fallback_addr
}

// 步骤 2.1: 获取自身的公网 IP 和端口（连接到 STUN）
// 用 spawn_blocking 包裹阻塞 I/O，避免冻结 tokio 线程
pub async fn get_public_address() -> Result<(std::net::UdpSocket, SocketAddr), String> {
    tokio::task::spawn_blocking(|| {
        let socket = std::net::UdpSocket::bind("0.0.0.0:0").map_err(|e| e.to_string())?;
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        socket.set_nonblocking(false).unwrap();

        // 国内优先，国外备用（各组内部随机打乱分散负载）
        let mut cn_servers = [
            "stun.miwifi.com:3478",        // 小米
            "stun.qq.com:3478",            // 腾讯
            "stun.chat.bilibili.com:3478", // B站
            "stun.hitv.com:3478",          // 华数TV
            "stun.cdnbye.com:3478",        // CDNBye
        ];
        let mut intl_servers = [
            "stun.l.google.com:19302",       // Google
            "stun1.l.google.com:19302",      // Google 2
            "stun2.l.google.com:19302",      // Google 3
            "stun.cloudflare.com:3478",      // Cloudflare
            "stun.stunprotocol.org:3478",    // 开源社区
            "stun.voip.blackberry.com:3478", // BlackBerry
            "stun.sipnet.ru:3478",           // SipNet
        ];

        // 各组内部随机打乱
        for i in (1..cn_servers.len()).rev() {
            let j = rand::thread_rng().gen_range(0..=i);
            cn_servers.swap(i, j);
        }
        for i in (1..intl_servers.len()).rev() {
            let j = rand::thread_rng().gen_range(0..=i);
            intl_servers.swap(i, j);
        }

        // 国内在前，海外在后
        let stun_servers: Vec<&str> = cn_servers
            .iter()
            .chain(intl_servers.iter())
            .copied()
            .collect();

        for stun_server in stun_servers.iter() {
            // 每个服务器用独立的 transaction ID
            let (txn_id, req) = build_stun_request();

            if socket.send_to(&req, stun_server).is_ok() {
                let mut buf = [0; 1024];
                if let Ok((len, _)) = socket.recv_from(&mut buf) {
                    if let Some(addr) = parse_stun_response(&buf[..len], &txn_id) {
                        println!("[STUN] 探测成功: {}", addr);
                        return Ok((socket, addr));
                    }
                }
            }
        }

        Err("无法通过 STUN 获取公网 IP (可能是对称 NAT，建议回退到中继)".to_string())
    })
    .await
    .map_err(|e| format!("spawn_blocking 失败: {}", e))?
}

// 步骤 2.2: 使用获取的 UdpSocket 向对端疯狂发送握手包，实现打洞
// 当收到对方的包时，认为打洞成功，将 Socket 交给 Quinn 使用
pub async fn hole_punch(
    socket: std::net::UdpSocket,
    peer_addr: SocketAddr,
) -> Result<(TokioUdpSocket, SocketAddr), String> {
    socket.set_nonblocking(true).unwrap();
    let async_socket = TokioUdpSocket::from_std(socket).map_err(|e| e.to_string())?;

    println!("[UDP 打洞] 开始向 {} 尝试盲发打洞...", peer_addr);
    let punch_msg = b"PUNCH_HOLE_MAGIC";
    let mut buf = [0; 1024];

    // 启动接收循环的超时（30 秒，给慢速 NAT 更多时间）
    let start_time = std::time::Instant::now();
    let timeout = Duration::from_secs(30);

    loop {
        if start_time.elapsed() > timeout {
            return Err("打洞超时失败（30秒内未收到对方回包，可能双方 NAT 不兼容）".to_string());
        }

        // 持续发送
        let _ = async_socket.send_to(punch_msg, peer_addr).await;

        // 尝试清空并检查接收缓冲区的所有数据包
        while let Ok((len, src)) = async_socket.try_recv_from(&mut buf) {
            let same_peer = src == peer_addr || src.ip() == peer_addr.ip();
            if same_peer && &buf[..len] == punch_msg {
                println!(
                    "[UDP 打洞] 成功！收到 {} 的打洞包（信令地址: {}）",
                    src, peer_addr
                );
                // 再给对方连发几包，确保对方跳出循环
                for _ in 0..5 {
                    let _ = async_socket.send_to(punch_msg, src).await;
                }
                return Ok((async_socket, src));
            }
        }

        // 发包间隔降低到 100ms，提高打洞成功率
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

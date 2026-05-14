use quinn::{ClientConfig, Endpoint, ServerConfig, TransportConfig};
use rcgen::generate_simple_self_signed;
use rustls::{client::ServerCertVerified, Certificate, PrivateKey};
use std::sync::Arc;
use std::time::Duration;

// 盲信证书验证器
struct SkipServerVerification;

impl rustls::client::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &Certificate,
        _intermediates: &[Certificate],
        _server_name: &rustls::ServerName,
        _scts: &mut dyn Iterator<Item = &[u8]>,
        _ocsp_response: &[u8],
        _now: std::time::SystemTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }
}

/// 创建面向 Minecraft 长连接优化的传输配置
fn make_transport_config() -> TransportConfig {
    let mut transport = TransportConfig::default();
    // 每 5 秒发送 Keep-Alive，防止 NAT 映射过期 + 防止空闲超时
    transport.keep_alive_interval(Some(Duration::from_secs(5)));
    // 最大空闲超时设为 5 分钟（MC 挂机场景）
    if let Ok(idle) = quinn::IdleTimeout::try_from(Duration::from_secs(300)) {
        transport.max_idle_timeout(Some(idle));
    }
    transport
}

// ============== 房主端：启动 QUIC 服务端 ==============
pub fn build_quic_server(udp_socket: std::net::UdpSocket) -> Result<Endpoint, String> {
    println!("[QUIC] 正在生成自签名证书...");
    let subject_alt_names = vec!["minecraft.p2p".to_string()];
    let cert = generate_simple_self_signed(subject_alt_names).map_err(|e| e.to_string())?;

    let cert_der = cert.serialize_der().map_err(|e| e.to_string())?;
    let priv_key_der = cert.serialize_private_key_der();

    let rustls_cert = vec![Certificate(cert_der)];
    let rustls_key = PrivateKey(priv_key_der);

    let mut server_config =
        ServerConfig::with_single_cert(rustls_cert, rustls_key).map_err(|e| e.to_string())?;
    server_config.transport_config(Arc::new(make_transport_config()));

    println!("[QUIC] 服务端正在启动...");
    let endpoint = Endpoint::new(
        Default::default(),
        Some(server_config),
        udp_socket,
        Arc::new(quinn::TokioRuntime),
    )
    .map_err(|e| e.to_string())?;

    println!("[QUIC] 服务端启动成功！");
    Ok(endpoint)
}

// ============== 访客端：启动 QUIC 客户端 ==============
pub fn build_quic_client(udp_socket: std::net::UdpSocket) -> Result<Endpoint, String> {
    println!("[QUIC] 正在配置客户端（盲信模式）...");
    let crypto = rustls::ClientConfig::builder()
        .with_safe_defaults()
        .with_custom_certificate_verifier(Arc::new(SkipServerVerification))
        .with_no_client_auth();

    let mut client_config = ClientConfig::new(Arc::new(crypto));
    client_config.transport_config(Arc::new(make_transport_config()));

    let mut endpoint = Endpoint::new(
        Default::default(),
        None,
        udp_socket,
        Arc::new(quinn::TokioRuntime),
    )
    .map_err(|e| e.to_string())?;

    endpoint.set_default_client_config(client_config);
    println!("[QUIC] 客户端启动成功！");

    Ok(endpoint)
}

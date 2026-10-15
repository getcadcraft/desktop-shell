// SPDX-License-Identifier: Apache-2.0
//! Studio Live's outgoing WebSockets (a LAN host, the relay over wss://), one implementation for the shell and for
//! `tools/live-test/` (which builds this module on its own): the OS's own TLS everywhere but Android; Android (TB.1) has
//! no system OpenSSL, so rustls there, verified by the system's own trust manager (`crate::mobile::ws_connector`).

pub type WsConnected = (
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    tokio_tungstenite::tungstenite::handshake::client::Response,
);

#[cfg(not(target_os = "android"))]
pub async fn ws_connect(url: &str) -> Result<WsConnected, tokio_tungstenite::tungstenite::Error> {
    tokio_tungstenite::connect_async(url).await
}

#[cfg(target_os = "android")]
pub async fn ws_connect(url: &str) -> Result<WsConnected, tokio_tungstenite::tungstenite::Error> {
    tokio_tungstenite::connect_async_tls_with_config(url, None, false, crate::mobile::ws_connector().ok()).await
}

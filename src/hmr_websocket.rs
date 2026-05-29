use futures::FutureExt;
use futures::future::BoxFuture;
use futures_util::stream::{FuturesUnordered, SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::RwLock;
use tokio::task::yield_now;
use tokio::time::sleep;
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio_tungstenite::{WebSocketStream, accept_async};
use tracing::{info, trace};

type WsSender = SplitSink<WebSocketStream<TcpStream>, Message>;
type WsReceiver = SplitStream<WebSocketStream<TcpStream>>;

pub struct HmrWebSocketClient {
  pub addr: SocketAddr,
  pub sender: Arc<RwLock<WsSender>>,
  pub name: String,
}

impl HmrWebSocketClient {
  pub fn new(addr: SocketAddr, sender: WsSender, name: &str) -> Self {
    Self {
      addr,
      sender: Arc::new(RwLock::new(sender)),
      name: name.to_string(),
    }
  }

  pub async fn send_message(&mut self, msg: &str) {
    let mut sender = self.sender.write().await;
    if let Err(e) = sender.send(Message::Text(msg.into())).await {
      info!("❌ Failed to send message to {}: {}", self.addr, e);
    }
  }
}

pub struct HmrWebSocketClientManager {
  clients: Arc<RwLock<HashMap<SocketAddr, HmrWebSocketClient>>>,
  name_to_addr: Arc<RwLock<HashMap<String, SocketAddr>>>,
}

impl HmrWebSocketClientManager {
  pub fn new() -> Self {
    Self {
      clients: Arc::new(RwLock::new(HashMap::new())),
      name_to_addr: Arc::new(RwLock::new(HashMap::new())),
    }
  }

  pub async fn add_client(&self, addr: SocketAddr, sender: WsSender, name: &str) {
    let client = HmrWebSocketClient::new(addr, sender, name);
    let mut clients = self.clients.write().await;
    clients.insert(addr, client);
  }

  pub async fn remove_client(&self, addr: &SocketAddr) {
    self.clients.write().await.remove(addr);
  }

  pub async fn send_message(&self, name: &str, msg: &str) {
    let mut clients = self.clients.write().await;
    if let Some(addr) = self.name_to_addr.write().await.get(name) {
      if let Some(client) = clients.get_mut(addr) {
        client.send_message(msg).await;
      }
    }
  }

  pub async fn send_message_to_all(&self, msg: &str) {
    let mut clients = self.clients.write().await;
    for client in clients.values_mut() {
      client.send_message(msg).await;
    }
  }
}

pub struct HmrWebSocket {
  pub port: AtomicU16,
  client_manager: HmrWebSocketClientManager,
  _is_running: Arc<AtomicBool>,
}

impl HmrWebSocket {
  pub fn new(port: u16, is_running: Arc<AtomicBool>) -> Self {
    Self {
      port: AtomicU16::new(port),
      client_manager: HmrWebSocketClientManager::new(),
      _is_running: is_running,
    }
  }

  pub async fn main_loop(&self) {
    let addr = SocketAddr::from(([127, 0, 0, 1], self.port.load(Ordering::SeqCst)));
    let listener = TcpListener::bind(&addr).await.unwrap();
    let local_addr = listener
      .local_addr()
      .unwrap_or_else(|_| panic!("Failed to bind to address: {}", addr));
    if local_addr.port() != self.port.load(Ordering::SeqCst) {
      self.port.store(local_addr.port(), Ordering::SeqCst);
      info!(
        "Port {} was unavailable, using dynamically assigned port {}",
        self.port.load(Ordering::SeqCst),
        local_addr.port()
      );
    }
    info!("HMR WebSocket server listening on: {}", addr);
    let mut active_receivers: FuturesUnordered<BoxFuture<'static, (SocketAddr, WsReceiver, WsSender)>> =
      FuturesUnordered::new();

    loop {
      if !self._is_running.load(Ordering::Relaxed) {
        info!("HMR WebSocket server is shutting down...");
        break;
      }
      tokio_select!(
        biased,
        match .. {
          .. if let Ok((stream, addr)) = listener.accept() => {
            info!("🔌 New client connection from: {}", addr);
            if let Ok(ws_stream) = accept_async(stream).await {
              info!("🤝 WebSocket handshake successful with {}", addr);
              let (ws_sender, ws_receiver) = ws_stream.split();
              active_receivers.push(async move { (addr, ws_receiver, ws_sender) }.boxed());
            } else {
              info!("❌ Failed WebSocket handshake with {}", addr);
            }
          }
          .. if let Some((addr, mut ws_receiver, ws_sender)) = active_receivers.next() => {
            if !active_receivers.is_empty() {
              match ws_receiver.next().await {
                Some(Ok(Message::Text(msg))) => {
                  trace!("📥 Received message from {}: {:?}", addr, msg);
                  let json = serde_json::from_str::<serde_json::Value>(&msg);
                  if let Ok(json) = json {
                    if let Some(name) = json.get("name").and_then(|v| v.as_str()) {
                      self.client_manager.add_client(addr, ws_sender, name).await;
                      info!("✅ Registered client {} with name '{}'", addr, name);
                    } else {
                      active_receivers.push(async move { (addr, ws_receiver, ws_sender) }.boxed());
                      info!("⚠️ Received message from {} without 'name' field", addr);
                    }
                  } else {
                    info!("⚠️ Received non-JSON message from {}: {}", addr, msg);
                  }
                }
                Some(Ok(Message::Close(_))) => {
                  info!("👋 Client {} disconnected", addr);
                  self.client_manager.remove_client(&addr).await;
                }
                Some(Ok(_)) => {
                  // Handle other message types (Binary, Ping/Pong) if necessary
                  // For now, we just ignore them and keep the connection alive
                  // If you want to handle them, you can add more match arms here
                  // e.g., log binary data size, respond to pings, etc.
                  active_receivers.push(async move { (addr, ws_receiver, ws_sender) }.boxed());
                }
                Some(Err(e)) => {
                  info!("❌ Error receiving message from {}: {}", addr, e);
                  self.client_manager.remove_client(&addr).await;
                }
                None => {
                  info!("👋 Client {} disconnected (stream ended)", addr);
                  self.client_manager.remove_client(&addr).await;
                }
              }
            }
          }
          .. if let _ = sleep(Duration::from_millis(100)) => {
            yield_now().await; // Allow other tasks to run
          }
        }
      );
    }
  }
}

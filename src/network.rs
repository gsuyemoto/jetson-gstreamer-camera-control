use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use socket2::{Domain, Protocol, Socket, Type};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::net::UdpSocket;

pub const MULTICAST_ADDR: &str = "239.255.42.99";
pub const DISCOVERY_PORT: u16 = 9942;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Message {
    /// Discovery beacon from a node
    Discovery {
        node_id: String,
        role: NodeRole,
        timestamp: i64,
    },
    /// Time sync request (NTP-style)
    TimeSyncRequest {
        node_id: String,
        t1: i64, // Client timestamp when request sent
    },
    /// Time sync response
    TimeSyncResponse {
        node_id: String,
        t1: i64, // Original client timestamp
        t2: i64, // Server timestamp when request received
        t3: i64, // Server timestamp when response sent
    },
    /// Recording control command
    RecordingCommand {
        command: RecordingCmd,
        target_timestamp: i64, // When to execute (microseconds since UNIX epoch)
    },
    /// Status report from a node
    Status {
        node_id: String,
        state: NodeState,
        timestamp: i64,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum NodeRole {
    Leader,
    Follower,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum RecordingCmd {
    Start,
    Stop,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum NodeState {
    Idle,
    Ready,
    Recording,
    Error,
}

pub struct NetworkManager {
    socket: UdpSocket,
    node_id: String,
    role: NodeRole,
}

impl NetworkManager {
    pub async fn new(node_id: String, role: NodeRole, interface_ip: Option<IpAddr>) -> Result<Self> {
        let socket = Self::create_multicast_socket(interface_ip).await?;
        
        Ok(Self {
            socket,
            node_id,
            role,
        })
    }

    async fn create_multicast_socket(interface_ip: Option<IpAddr>) -> Result<UdpSocket> {
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        socket.set_reuse_address(true)?;

        let bind_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), DISCOVERY_PORT);
        socket.bind(&bind_addr.into())?;

        // Join multicast group
        let multicast_addr: Ipv4Addr = MULTICAST_ADDR.parse()?;
        let interface_addr = match interface_ip {
            Some(IpAddr::V4(addr)) => addr,
            _ => Ipv4Addr::UNSPECIFIED,
        };
        socket.join_multicast_v4(&multicast_addr, &interface_addr)?;

        // Set multicast outgoing interface
        if let Some(IpAddr::V4(addr)) = interface_ip {
            socket.set_multicast_if_v4(&addr)?;
        }

        socket.set_nonblocking(true)?;
        let tokio_socket: std::net::UdpSocket = socket.into();
        UdpSocket::from_std(tokio_socket).context("Failed to create tokio UdpSocket")
    }

    pub async fn send_message(&self, msg: &Message) -> Result<()> {
        let json = serde_json::to_string(msg)?;
        let multicast_dest = SocketAddr::new(
            IpAddr::V4(MULTICAST_ADDR.parse()?),
            DISCOVERY_PORT,
        );
        self.socket.send_to(json.as_bytes(), multicast_dest).await?;
        Ok(())
    }

    pub async fn receive_message(&self) -> Result<(Message, SocketAddr)> {
        let mut buf = vec![0u8; 65536];
        let (len, addr) = self.socket.recv_from(&mut buf).await?;
        let msg: Message = serde_json::from_slice(&buf[..len])?;
        Ok((msg, addr))
    }

    pub async fn send_discovery(&self) -> Result<()> {
        let msg = Message::Discovery {
            node_id: self.node_id.clone(),
            role: self.role,
            timestamp: current_timestamp_micros(),
        };
        self.send_message(&msg).await
    }

    pub async fn send_time_sync_request(&self) -> Result<i64> {
        let t1 = current_timestamp_micros();
        let msg = Message::TimeSyncRequest {
            node_id: self.node_id.clone(),
            t1,
        };
        self.send_message(&msg).await?;
        Ok(t1)
    }

    pub async fn send_time_sync_response(&self, t1: i64, t2: i64) -> Result<()> {
        let t3 = current_timestamp_micros();
        let msg = Message::TimeSyncResponse {
            node_id: self.node_id.clone(),
            t1,
            t2,
            t3,
        };
        self.send_message(&msg).await
    }

    pub async fn send_recording_command(&self, command: RecordingCmd, target_timestamp: i64) -> Result<()> {
        let msg = Message::RecordingCommand {
            command,
            target_timestamp,
        };
        self.send_message(&msg).await
    }

    pub async fn send_status(&self, state: NodeState) -> Result<()> {
        let msg = Message::Status {
            node_id: self.node_id.clone(),
            state,
            timestamp: current_timestamp_micros(),
        };
        self.send_message(&msg).await
    }

    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    pub fn role(&self) -> NodeRole {
        self.role
    }
}

pub fn current_timestamp_micros() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards")
        .as_micros() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_serialization() {
        let msg = Message::Discovery {
            node_id: "test".to_string(),
            role: NodeRole::Leader,
            timestamp: 12345,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: Message = serde_json::from_str(&json).unwrap();
        matches!(decoded, Message::Discovery { .. });
    }
}

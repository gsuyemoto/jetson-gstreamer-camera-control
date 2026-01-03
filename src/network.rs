use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use socket2::{Domain, Protocol, Socket, Type};
use tracing::{info, debug};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::net::UdpSocket;

pub const BROADCAST_ADDR: &str = "192.168.50.255";
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
        role: NodeRole,
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

impl std::fmt::Display for NodeRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NodeRole::Leader => write!(f, "Leader"),
            NodeRole::Follower => write!(f, "Follower"),
        }
    }
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
    pub async fn new(
        node_id: String,
        role: NodeRole,
        interface_ip: Option<IpAddr>,
    ) -> Result<Self> {
        let socket = Self::create_broadcast_socket(interface_ip).await?;

        Ok(Self {
            socket,
            node_id,
            role,
        })
    }

    async fn create_broadcast_socket(interface_ip: Option<IpAddr>) -> Result<UdpSocket> {
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        socket.set_reuse_address(true)?;
        socket.set_broadcast(true)?;

        // Bind to the specific interface if provided, otherwise bind to all interfaces
        let bind_addr = match interface_ip {
            Some(IpAddr::V4(ip)) => SocketAddr::new(IpAddr::V4(ip), DISCOVERY_PORT),
            _ => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), DISCOVERY_PORT),
        };
        socket.bind(&bind_addr.into())?;

        socket.set_nonblocking(true)?;
        let tokio_socket: std::net::UdpSocket = socket.into();
        
        // Set socket options to ensure broadcast works on the specific interface
        if let Some(IpAddr::V4(ip)) = interface_ip {
            use std::os::unix::io::AsRawFd;
            let raw_fd = tokio_socket.as_raw_fd();
            // IP_MULTICAST_IF (for both broadcast and multicast) - set to our interface
            unsafe {
                let addr = libc::in_addr { s_addr: u32::from(ip).to_be() };
                let res = libc::setsockopt(
                    raw_fd,
                    libc::IPPROTO_IP,
                    libc::IP_MULTICAST_IF,
                    &addr as *const _ as *const libc::c_void,
                    std::mem::size_of::<libc::in_addr>() as libc::socklen_t,
                );
                if res < 0 {
                    return Err(anyhow::anyhow!("Failed to set IP_MULTICAST_IF"));
                }
            }
        }
        
        UdpSocket::from_std(tokio_socket).context("Failed to create tokio UdpSocket")
    }

    pub async fn send_message(&self, msg: &Message) -> Result<()> {
        let json = serde_json::to_string(msg)?;
        let broadcast_dest = SocketAddr::new(IpAddr::V4(BROADCAST_ADDR.parse()?), DISCOVERY_PORT);
        debug!("Sending message to {}: {:?}", broadcast_dest, msg);
        self.socket.send_to(json.as_bytes(), broadcast_dest).await?;
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

    pub async fn send_recording_command(
        &self,
        command: RecordingCmd,
        target_timestamp: i64,
    ) -> Result<()> {
        let msg = Message::RecordingCommand {
            command,
            target_timestamp,
            role: self.role,
        };
        self.send_message(&msg).await
    }

    pub async fn send_status(&self, state: NodeState) -> Result<()> {
        let msg = Message::Status {
            node_id: self.node_id.clone(),
            state,
            timestamp: current_timestamp_micros(),
        };

        info!("Sending status message: {:?}", msg);

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

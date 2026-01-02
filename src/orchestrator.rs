use crate::network::{current_timestamp_micros, Message, NetworkManager, NodeRole, NodeState, RecordingCmd};
use crate::sync::{SyncScheduler, TimeSyncManager};
use anyhow::Result;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};

pub enum RecordingEvent {
    Start,
    Stop,
}

pub struct Orchestrator {
    network: Arc<NetworkManager>,
    time_sync: Arc<TimeSyncManager>,
    scheduler: SyncScheduler,
    running: Arc<AtomicBool>,
    recording_tx: mpsc::Sender<RecordingEvent>,
    peers: Arc<tokio::sync::RwLock<HashMap<String, PeerInfo>>>,
}

#[derive(Debug, Clone)]
struct PeerInfo {
    role: NodeRole,
    last_seen: i64,
    state: NodeState,
}

impl Orchestrator {
    pub fn new(
        network: Arc<NetworkManager>,
        time_sync: Arc<TimeSyncManager>,
        running: Arc<AtomicBool>,
        recording_tx: mpsc::Sender<RecordingEvent>,
    ) -> Self {
        let scheduler = SyncScheduler::new(time_sync.clone());
        
        Self {
            network,
            time_sync,
            scheduler,
            running,
            recording_tx,
            peers: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
        }
    }

    pub async fn run(&self) -> Result<()> {
        // Start background tasks
        let discovery_handle = self.spawn_discovery_task();
        let time_sync_handle = self.spawn_time_sync_task();
        let message_handler = self.spawn_message_handler();

        println!("Orchestrator started (role: {:?})", self.network.role());

        // Wait for shutdown signal
        while self.running.load(Ordering::SeqCst) {
            sleep(Duration::from_millis(100)).await;
        }

        // Clean up
        discovery_handle.abort();
        time_sync_handle.abort();
        message_handler.abort();

        Ok(())
    }

    fn spawn_discovery_task(&self) -> tokio::task::JoinHandle<()> {
        let network = self.network.clone();
        let running = self.running.clone();

        tokio::spawn(async move {
            while running.load(Ordering::SeqCst) {
                if let Err(e) = network.send_discovery().await {
                    eprintln!("Failed to send discovery: {}", e);
                }
                sleep(Duration::from_secs(2)).await;
            }
        })
    }

    fn spawn_time_sync_task(&self) -> tokio::task::JoinHandle<()> {
        let network = self.network.clone();
        let running = self.running.clone();
        let role = network.role();

        tokio::spawn(async move {
            // Only followers need to actively sync time with leader
            if role == NodeRole::Follower {
                // Wait a bit for discovery
                sleep(Duration::from_secs(1)).await;

                while running.load(Ordering::SeqCst) {
                    if let Err(e) = network.send_time_sync_request().await {
                        eprintln!("Failed to send time sync request: {}", e);
                    }
                    sleep(Duration::from_secs(5)).await;
                }
            }
        })
    }

    fn spawn_message_handler(&self) -> tokio::task::JoinHandle<()> {
        let network = self.network.clone();
        let time_sync = self.time_sync.clone();
        let scheduler = self.scheduler.clone();
        let running = self.running.clone();
        let recording_tx = self.recording_tx.clone();
        let peers = self.peers.clone();
        let node_id = network.node_id().to_string();

        tokio::spawn(async move {
            while running.load(Ordering::SeqCst) {
                match network.receive_message().await {
                    Ok((msg, _addr)) => {
                        if let Err(e) = Self::handle_message(
                            msg,
                            &network,
                            &time_sync,
                            &scheduler,
                            &recording_tx,
                            &peers,
                            &node_id,
                        )
                        .await
                        {
                            eprintln!("Error handling message: {}", e);
                        }
                    }
                    Err(e) => {
                        eprintln!("Failed to receive message: {}", e);
                        sleep(Duration::from_millis(10)).await;
                    }
                }
            }
        })
    }

    async fn handle_message(
        msg: Message,
        network: &NetworkManager,
        time_sync: &TimeSyncManager,
        scheduler: &SyncScheduler,
        recording_tx: &mpsc::Sender<RecordingEvent>,
        peers: &tokio::sync::RwLock<HashMap<String, PeerInfo>>,
        node_id: &str,
    ) -> Result<()> {
        match msg {
            Message::Discovery {
                node_id: peer_id,
                role,
                timestamp,
            } => {
                // Ignore our own messages
                if peer_id == node_id {
                    return Ok(());
                }

                let mut peers_map = peers.write().await;
                peers_map.insert(
                    peer_id.clone(),
                    PeerInfo {
                        role,
                        last_seen: timestamp,
                        state: NodeState::Idle,
                    },
                );
                println!("Discovered peer: {} (role: {:?})", peer_id, role);
            }

            Message::TimeSyncRequest {
                node_id: peer_id,
                t1,
            } => {
                // Ignore our own messages
                if peer_id == node_id {
                    return Ok(());
                }

                // Leaders respond to time sync requests
                if network.role() == NodeRole::Leader {
                    let t2 = current_timestamp_micros();
                    network.send_time_sync_response(t1, t2).await?;
                }
            }

            Message::TimeSyncResponse {
                node_id: peer_id,
                t1,
                t2,
                t3,
            } => {
                // Ignore our own messages
                if peer_id == node_id {
                    return Ok(());
                }

                let t4 = current_timestamp_micros();
                time_sync.process_sync_response(t1, t2, t3, t4, &peer_id);
            }

            Message::RecordingCommand {
                command,
                target_timestamp,
            } => {
                let now = current_timestamp_micros();
                let micros_until = scheduler.micros_until(target_timestamp, now);

                println!(
                    "Received recording command: {:?}, executing in {} ms",
                    command,
                    micros_until / 1000
                );

                if micros_until > 0 {
                    // Schedule the command
                    let recording_tx = recording_tx.clone();
                    tokio::spawn(async move {
                        sleep(Duration::from_micros(micros_until as u64)).await;
                        let event = match command {
                            RecordingCmd::Start => RecordingEvent::Start,
                            RecordingCmd::Stop => RecordingEvent::Stop,
                        };
                        let _ = recording_tx.send(event).await;
                    });
                } else {
                    // Execute immediately
                    let event = match command {
                        RecordingCmd::Start => RecordingEvent::Start,
                        RecordingCmd::Stop => RecordingEvent::Stop,
                    };
                    recording_tx.send(event).await?;
                }
            }

            Message::Status {
                node_id: peer_id,
                state,
                timestamp,
            } => {
                // Ignore our own messages
                if peer_id == node_id {
                    return Ok(());
                }

                let mut peers_map = peers.write().await;
                if let Some(peer) = peers_map.get_mut(&peer_id) {
                    peer.state = state;
                    peer.last_seen = timestamp;
                }
            }
        }

        Ok(())
    }

    pub async fn send_recording_command(&self, command: RecordingCmd, delay_ms: i64) -> Result<()> {
        let now = current_timestamp_micros();
        let target_timestamp = self.scheduler.schedule_future(now, delay_ms * 1000);
        
        self.network
            .send_recording_command(command, target_timestamp)
            .await?;
        
        println!(
            "Sent recording command: {:?}, scheduled in {} ms",
            command, delay_ms
        );
        
        Ok(())
    }

    pub async fn send_status(&self, state: NodeState) -> Result<()> {
        self.network.send_status(state).await
    }

    pub async fn get_peers(&self) -> Vec<(String, NodeRole, NodeState)> {
        let peers = self.peers.read().await;
        peers
            .iter()
            .map(|(id, info)| (id.clone(), info.role, info.state))
            .collect()
    }

    pub fn is_time_synchronized(&self) -> bool {
        self.time_sync.is_synchronized()
    }
}

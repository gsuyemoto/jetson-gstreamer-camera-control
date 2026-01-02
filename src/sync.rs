use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Time synchronization manager using NTP-style algorithm
pub struct TimeSyncManager {
    /// Clock offset in microseconds (local_time = network_time + offset)
    clock_offset: Arc<RwLock<i64>>,
    /// Round-trip time measurements for each peer
    rtt_measurements: Arc<RwLock<HashMap<String, Vec<i64>>>>,
    /// Maximum number of samples to keep per peer
    max_samples: usize,
}

impl TimeSyncManager {
    pub fn new(max_samples: usize) -> Self {
        Self {
            clock_offset: Arc::new(RwLock::new(0)),
            rtt_measurements: Arc::new(RwLock::new(HashMap::new())),
            max_samples,
        }
    }

    /// Process a time sync response and update clock offset
    /// Using NTP algorithm:
    /// - t1: client timestamp when request sent
    /// - t2: server timestamp when request received
    /// - t3: server timestamp when response sent
    /// - t4: client timestamp when response received
    pub fn process_sync_response(&self, t1: i64, t2: i64, t3: i64, t4: i64, peer_id: &str) {
        // Calculate round-trip time
        let rtt = (t4 - t1) - (t3 - t2);
        
        // Calculate clock offset: offset = ((t2 - t1) + (t3 - t4)) / 2
        let offset = ((t2 - t1) + (t3 - t4)) / 2;

        // Store RTT measurement
        {
            let mut rtt_map = self.rtt_measurements.write().unwrap();
            let measurements = rtt_map.entry(peer_id.to_string()).or_insert_with(Vec::new);
            measurements.push(rtt);
            
            // Keep only the most recent samples
            if measurements.len() > self.max_samples {
                measurements.remove(0);
            }
        }

        // Update clock offset using exponential moving average
        {
            let mut current_offset = self.clock_offset.write().unwrap();
            if *current_offset == 0 {
                // First measurement
                *current_offset = offset;
            } else {
                // Weighted average: 80% old, 20% new
                *current_offset = (*current_offset * 8 + offset * 2) / 10;
            }
        }

        println!(
            "Time sync: offset={} μs, RTT={} μs (peer: {})",
            offset, rtt, peer_id
        );
    }

    /// Get the current clock offset
    pub fn get_clock_offset(&self) -> i64 {
        *self.clock_offset.read().unwrap()
    }

    /// Convert local timestamp to synchronized network timestamp
    pub fn local_to_network_time(&self, local_time: i64) -> i64 {
        local_time - self.get_clock_offset()
    }

    /// Convert network timestamp to local timestamp
    pub fn network_to_local_time(&self, network_time: i64) -> i64 {
        network_time + self.get_clock_offset()
    }

    /// Get average RTT for a peer
    pub fn get_average_rtt(&self, peer_id: &str) -> Option<i64> {
        let rtt_map = self.rtt_measurements.read().unwrap();
        rtt_map.get(peer_id).and_then(|measurements| {
            if measurements.is_empty() {
                None
            } else {
                let sum: i64 = measurements.iter().sum();
                Some(sum / measurements.len() as i64)
            }
        })
    }

    /// Check if time synchronization is stable
    pub fn is_synchronized(&self) -> bool {
        // Consider synchronized if we have an offset measurement
        self.get_clock_offset() != 0
    }

    /// Reset synchronization state
    pub fn reset(&self) {
        *self.clock_offset.write().unwrap() = 0;
        self.rtt_measurements.write().unwrap().clear();
    }
}

#[derive(Clone)]
/// Scheduler for executing actions at specific timestamps
pub struct SyncScheduler {
    time_sync: Arc<TimeSyncManager>,
}

impl SyncScheduler {
    pub fn new(time_sync: Arc<TimeSyncManager>) -> Self {
        Self { time_sync }
    }

    /// Calculate how many microseconds until a network timestamp
    pub fn micros_until(&self, network_timestamp: i64, current_local_time: i64) -> i64 {
        let local_target = self.time_sync.network_to_local_time(network_timestamp);
        local_target - current_local_time
    }

    /// Check if a network timestamp has been reached
    pub fn is_time_reached(&self, network_timestamp: i64, current_local_time: i64) -> bool {
        self.micros_until(network_timestamp, current_local_time) <= 0
    }

    /// Get a future network timestamp (offset from now)
    pub fn schedule_future(&self, current_local_time: i64, offset_micros: i64) -> i64 {
        self.time_sync.local_to_network_time(current_local_time + offset_micros)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_time_sync_offset() {
        let sync = TimeSyncManager::new(10);
        
        // Simulate a time sync exchange
        let t1 = 1000000; // Client sends at 1s
        let t2 = 1500000; // Server receives at 1.5s (server is 0.5s ahead)
        let t3 = 1501000; // Server responds 1ms later
        let t4 = 1002000; // Client receives 2ms after sending
        
        sync.process_sync_response(t1, t2, t3, t4, "test_peer");
        
        let offset = sync.get_clock_offset();
        assert!(offset != 0, "Offset should be calculated");
        
        // Offset should be approximately 500000 μs (0.5s)
        assert!((offset - 500000).abs() < 1000, "Offset should be ~500ms");
    }

    #[test]
    fn test_local_to_network_conversion() {
        let sync = TimeSyncManager::new(10);
        sync.process_sync_response(1000000, 1500000, 1501000, 1002000, "test");
        
        let local_time = 2000000;
        let network_time = sync.local_to_network_time(local_time);
        let back_to_local = sync.network_to_local_time(network_time);
        
        assert_eq!(local_time, back_to_local, "Time conversion should be reversible");
    }
}

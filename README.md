# record-syncd

Synchronized video recording over ethernet ad hoc network using peer-to-peer discovery and NTP-style time synchronization.

## Features

- **Leader-Follower Architecture**: One leader node controls recording start/stop for all nodes
- **UDP Multicast Discovery**: Automatic peer discovery on local network
- **NTP-Style Time Synchronization**: Microsecond-precision time sync between nodes
- **Synchronized Recording**: All nodes start/stop recording at precisely coordinated times
- **H.265 Video Encoding**: Hardware-accelerated encoding on NVIDIA Jetson platforms

## Architecture

### Network Protocol
- **Multicast Address**: 239.255.42.99:9942
- **Discovery**: Nodes broadcast presence every 2 seconds
- **Time Sync**: Followers send sync requests every 5 seconds, leader responds
- **Recording Control**: Leader broadcasts START/STOP commands with target timestamp
- **Status**: Nodes report their current state (Idle, Ready, Recording, Error)

### Message Types
1. `Discovery` - Node announces itself with role and timestamp
2. `TimeSyncRequest` - Follower requests time sync from leader
3. `TimeSyncResponse` - Leader responds with timing information
4. `RecordingCommand` - Leader sends START/STOP with scheduled execution time
5. `Status` - Node reports current state

## Usage

### Leader Node

Start a leader node that controls the recording:

```bash
cargo run -- --role leader --output leader_video.mp4
```

Leader commands (interactive):
- `start [delay_ms]` - Start recording (default: 2000ms delay)
- `stop [delay_ms]` - Stop recording (default: 1000ms delay)
- `peers` - List connected peers
- `quit` - Exit

### Follower Node

Start follower nodes that respond to leader commands:

```bash
cargo run -- --role follower --output follower_video.mp4
```

Followers automatically:
- Discover the leader node
- Synchronize their clock with the leader
- Execute recording commands at precisely scheduled times

### Options

```
Options:
  -r, --role <ROLE>            Node role: leader or follower
  -i, --interface <INTERFACE>  Network interface IP address (optional)
  -o, --output <OUTPUT>        Output video filename [default: output_h265.mp4]
      --node-id <NODE_ID>      Node ID (auto-generated if not provided)
  -h, --help                   Print help
```

### Specifying Network Interface

If you have multiple network interfaces, specify which one to use:

```bash
cargo run -- --role leader --interface 192.168.1.10
```

## Example: Two-Camera Setup

**On Camera 1 (Leader):**
```bash
cargo run -- --role leader --output camera1.mp4
```

Then type:
```
start 3000
```

**On Camera 2 (Follower):**
```bash
cargo run -- --role follower --output camera2.mp4
```

Both cameras will start recording 3 seconds after the command is sent, synchronized to within milliseconds.

## Time Synchronization

The system uses an NTP-style algorithm to synchronize clocks:

1. Follower sends `TimeSyncRequest` with timestamp t1
2. Leader receives at t2, sends `TimeSyncResponse` with t1, t2, t3
3. Follower receives at t4
4. Clock offset calculated as: `offset = ((t2 - t1) + (t3 - t4)) / 2`
5. Round-trip time: `RTT = (t4 - t1) - (t3 - t2)`

The offset is continuously updated using exponential moving average for stability.

## Hardware Requirements

- NVIDIA Jetson platform (tested on Jetson Orin)
- CSI camera connected via `nvarguscamerasrc`
- Ethernet connection between nodes
- GStreamer with NVIDIA hardware acceleration plugins

## Dependencies

- `gstreamer` - Video pipeline
- `tokio` - Async runtime
- `serde` + `serde_json` - Message serialization
- `clap` - CLI argument parsing
- `socket2` - Low-level socket control for multicast
- `anyhow` - Error handling

## Troubleshooting

### Camera Issues

If the camera fails to initialize (hangs or errors), check:
- Camera is properly connected to CSI port
- `nvarguscamerasrc` is available: `gst-inspect-1.0 nvarguscamerasrc`
- Camera permissions: `sudo chmod 666 /dev/video*`

### Network Issues

If peers don't discover each other:
- Ensure both nodes are on same network
- Check multicast is enabled: `ip maddr show`
- Verify no firewall blocking UDP port 9942
- Try specifying network interface with `--interface`

### Sync Issues

If time synchronization is poor:
- Check network latency between nodes
- Ensure stable network connection
- Leader node should have stable clock (consider NTP)

## Future Enhancements

- Multiple leader support with election
- PTP (Precision Time Protocol) support for sub-microsecond sync
- Web UI for monitoring and control
- Support for more camera types
- Recording preview/monitoring
- Automatic failover

## License

MIT

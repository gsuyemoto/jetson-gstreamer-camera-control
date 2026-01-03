**2026-01-02 v1**

Changes Made to Fix Broadcast Communication

1. Socket Configuration (network.rs):
•  Socket now binds to the specific interface IP (when provided via --interface)
•  Added SO_BROADCAST socket option
•  Used IP_MULTICAST_IF to explicitly set the outgoing interface for broadcast packets
•  This ensures packets are sent/received on the correct network interface

2. Logging Added:
•  Network messages are now logged when sent (debug level)
•  Received messages are logged (debug level) 
•  Discovery and status messages logged at info level
•  All logs can be captured with --log-file option   

**2026-01-02 v2**

Changes in stop() function:
1. Pause pipeline first - Transitions through PAUSED state before sending EOS
2. Send EOS event - Tells the pipeline to finalize
3. Extended timeout (30s) - Gives qtmux more time to write the moov atom
4. Use timed_pop() with loop - More reliable than iter_timed() for waiting for EOS
5. Set to NULL state - Finally stops the pipeline
6. Sleep delays - Ensures proper state transitions

Changes Made - Recording State Tracking

New Features:

1. RecordingState Enum - Tracks three states:
◦  Idle - Not recording
◦  Recording - Currently recording
◦  Stopping - Finalizing video file (moov atom being written)
2. Enhanced quit Command - Now checks recording state:
◦  If Recording: prompts user to use stop command first
◦  If Stopping: informs user to wait, shows app will exit after file is finalized
◦  If Idle: exits immediately
3. Recording Lifecycle:
◦  State → Recording when start message received
◦  State → Stopping when stop message received
◦  State → Idle when file finalization complete

User Experience:
When user enters quit:
•  If recording is in progress: "Recording in progress. Please use 'stop' command first."
•  If video is finalizing: "Video is still finalizing... Please wait. The application will exit once the file is written."
•  If idle: Exits cleanly

This ensures the video file is fully written before the app exits, preventing file corruption from premature shutdown.

**2026-01-02 v3**

Changes to Fix MP4 File Finalization

1. Added faststart property to qtmux:
•  Tells the muxer to write the moov atom at the beginning of the file
•  Makes the file seekable and playable before the entire file is written

2. Improved stop() function:
•  Increased EOS timeout from 30s to 60s to allow more time for moov atom write
•  Sends EOS first, then waits for the EOS message
•  Then transitions through PAUSED→NULL states (proper GStreamer shutdown sequence)
•  Better logging to track finalization progress
•  Includes sleep delays to ensure proper state transitions

3. Better state management:
•  EOS is sent immediately (not after pausing)
•  Wait loop checks for EOS completion before state transitions
•  500ms final sleep ensures all buffers are flushed

Test this by:
1. Starting the app
2. Sending a start command (via leader UI or network message)
3. Letting it record for a few seconds
4. Sending a stop command and wait for the logs to show "EOS received - file finalization complete"
5. Then exit normally
6. The video should now be playable without the moov atom error

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

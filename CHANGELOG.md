2026-01-02
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

# Daemon Core owns all state; interfaces are thin clients

Formless is built around a single long-lived Daemon Core process that owns the agent loop, Tools, Sessions, metrics, and storage, with the TUI and CLI as interchangeable thin clients of one versioned protocol (Unix sockets on macOS/Linux, named pipes on Windows). We chose this over embedding the agent in the TUI because multiple concurrent Sessions and clients — including two TUIs on one Session, a headless CLI, and a future hosted Web frontend via a Relay — must share identical state that survives any client disconnecting, and only a single-writer daemon guarantees that. The daemon auto-starts on client launch, idle-closes 60s after the last Run and last client, and never runs as a system service.

**Status:** accepted

**Consequences:** All behaviour must live behind the daemon protocol; no feature may exist only in a client. Protocol versioning and mismatch refusal are part of the v1 surface.

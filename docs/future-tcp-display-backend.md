# Future TCP display backend

Status: design plan, not implemented.

## Purpose

A TCP display backend would let a worker running on another Linux machine use a
physical display attached to the Raspberry Pi. The immediate use is the x86-64
`ubuntu` builder: native C and firmware-emulator builds could run there while
their real user interface is rendered by `ubuntu3`.

This complements cross-compilation. Rust workers may be cross-built for direct
deployment, while native x86 test builds can still exercise their display path
against the real hardware.

```text
worker on ubuntu
    -> TCP display backend
    -> connected stream
    -> receiver on ubuntu3
    -> existing local display backend
    -> SPI or I2C display
```

## Architectural boundary

The TCP backend should preserve the library's existing capability model. It
should accept an already-connected stream descriptor rather than resolving a
host, opening a socket, or making access-control decisions itself. A supervisor
or standalone diagnostic program establishes the connection and gives the
backend only that capability.

The receiver owns the Pi-side display resources and renders through the same
local SSD1306, SH1106, or ST7789 backend used today. The transport does not
duplicate controller-specific display code.

## Transport semantics

TCP fits firmware display traffic better than UDP because display operations
are transactional rather than video-like:

1. The sender writes one length-delimited command.
2. The receiver validates it and invokes the corresponding local backend call.
3. The receiver sends a completion response after rendering finishes.
4. The sender's display call then returns.

Only one command is outstanding. This preserves the synchronous behavior of a
direct physical backend, makes the physical display the natural rate limit, and
prevents a queue of stale frames. `TCP_NODELAY` avoids adding a small-packet
delay to control commands and acknowledgements.

The first protocol should be small and versioned, with an explicit length and a
fixed binary command header followed by raw pixel data. It needs commands for:

- connection handshake and capability reporting;
- `write_native_frame`;
- `write_frame`, including pixel format, dimensions, and stride;
- display on and off;
- orderly close;
- success or structured failure responses.

The handshake should identify the protocol version, controller, native pixel
format, native dimensions, and maximum accepted payload. All lengths,
dimensions, formats, and command kinds must be checked before allocation or
rendering.

The sender should pass producer frames to the receiver without changing their
meaning. The receiver invokes the same native or converting call that a local
worker would have invoked, so conversion and controller behavior remain a
display-backend concern.

## Connection lifetime and failure

Connections need bounded connect, read, write, and acknowledgement timeouts. A
disconnect or partial command fails the current display call and discards the
incomplete command. Reconnection begins with a new handshake and no retained
protocol state.

The receiver should clear or turn off the physical display when its configured
session lifetime ends, following the same ownership rules as a local worker.
Whether a transient network interruption immediately ends that lifetime should
be an explicit receiver policy, not an accidental socket behavior.

The initial receiver should listen only on a trusted interface or behind an SSH
tunnel. Authentication and encryption are outside the display protocol; it
must not become an unauthenticated public network service.

## Regional refresh

The first implementation can transport complete frames. A 240x240 RGB565 frame
is 115,200 bytes, which is routine for a TCP stream and avoids the fragmentation
and reassembly machinery UDP would require.

The planned bounded-rectangle refresh work remains useful independently:

- physical backends can diff a new frame against their shadow frame and update
  only the changed rectangle or OLED page spans;
- the TCP protocol can later carry an explicit changed region;
- identical frames can complete without display-bus traffic;
- a failed partial update invalidates the affected shadow state so the next
  operation restores a known complete image.

Regional transport is an optimization, not a prerequisite for the remote
backend. Correct full-frame request/ack behavior should be established first.

## Later input transport

The established TCP session and its reverse direction could later carry GPIO
button transitions from the Pi to a remotely running firmware emulator. That
is deliberately outside the first display-only implementation. If display,
buttons, or other peripherals begin sharing one session, the session framing
should move into a small common remote-hardware transport rather than expanding
the display API into a general device protocol.

## Suggested implementation order

1. Define and test stream framing, validation, and request/ack behavior using an
   in-memory connected socket pair.
2. Add a receiver that owns one existing local backend on `ubuntu3`.
3. Add the descriptor-based TCP sender backend and exercise it with the display
   demo from `ubuntu`.
4. Test full monochrome and RGB565 frames, power transitions, disconnects,
   receiver restarts, malformed lengths, and timeout behavior.
5. Run one native x86 firmware worker with its display rendered on the Pi.
6. Measure end-to-end latency and then add bounded-region refresh if useful.
7. Consider reverse button events only after the display path is stable.

## Non-goals for the first version

- UDP frame streaming;
- video-rate output;
- more than one outstanding display operation;
- an unbounded frame or animation queue;
- opening network paths from inside `display-backends`;
- replacing the existing local SPI and I2C backends.

#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Relay between `sprocket` (over TCP) and an emulator's serial PTY.

Bytes from the host are delivered to the emulator no faster than a serial
line at BAUD would carry them (10 bits per byte); bytes from the emulator
pass straight through. Used with 86Box, whose PTY serial console otherwise
feeds the emulated UART as fast as bytes arrive, which overruns a UART with
no FIFO however fast the guest is.

Usage: pace-relay.py PTY PORT BAUD
       sprocket hello --port tcp://127.0.0.1:PORT --baud BAUD
"""
import os
import select
import socket
import sys
import time
import tty


def main():
    pty_path, port, baud = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
    byte_time = 10.0 / baud
    fd = os.open(pty_path, os.O_RDWR | os.O_NOCTTY)
    tty.setraw(fd)
    server = socket.socket()
    server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    server.bind(("127.0.0.1", port))
    server.listen(1)
    print(f"relaying {pty_path} on 127.0.0.1:{port} at {baud} baud", flush=True)
    while True:
        conn, _ = server.accept()
        queue = bytearray()
        next_at = time.monotonic()
        while True:
            wait = max(0.0, next_at - time.monotonic()) if queue else 0.5
            ready, _, _ = select.select([conn, fd], [], [], wait)
            if conn in ready:
                data = conn.recv(4096)
                if not data:
                    break
                queue += data
            if fd in ready:
                try:
                    data = os.read(fd, 4096)
                except OSError:
                    data = b""
                if data:
                    conn.sendall(data)
            now = time.monotonic()
            if queue and now >= next_at:
                os.write(fd, queue[:1])
                del queue[:1]
                next_at = max(next_at, now) + byte_time
        conn.close()


if __name__ == "__main__":
    main()

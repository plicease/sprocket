#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""TCP relay that corrupts one byte in each direction, for fault tests.

Listens on LISTEN_PORT for `sprocket`, connects to TARGET_PORT (DOSBox-X's
null-modem), and relays both ways. It flips the low bit of byte number
TO_CLIENT sent host -> client, and of byte FROM_CLIENT sent client -> host;
a negative index disables that direction. Both count from 0 at the first
byte the client sends (its HELLO_REPLY), so the host's repeated HELLOs
before the client starts do not shift them. If the chosen byte is a frame
delimiter or escape, or would become one, the next byte that is not is
corrupted instead, so exactly one frame is damaged and framing is otherwise
unchanged.

Usage: fault-relay.py LISTEN_PORT TARGET_PORT TO_CLIENT FROM_CLIENT
"""
import select
import socket
import sys

SPECIAL = (0x7E, 0x7D)


class Corrupter:
    def __init__(self, index):
        self.index = index
        self.seen = 0
        self.armed = False

    def apply(self, data):
        if not self.armed:
            return data
        data = bytearray(data)
        for i in range(len(data)):
            n = self.seen + i
            if 0 <= self.index <= n:
                b = data[i]
                if b not in SPECIAL and b ^ 1 not in SPECIAL:
                    data[i] = b ^ 1
                    print(f"corrupted byte {n}: {b:02x} -> {b ^ 1:02x}",
                          file=sys.stderr, flush=True)
                    self.index = -1
        self.seen += len(data)
        return bytes(data)


def main():
    listen_port, target_port = int(sys.argv[1]), int(sys.argv[2])
    to_client = Corrupter(int(sys.argv[3]))
    from_client = Corrupter(int(sys.argv[4]))
    server = socket.socket()
    server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    server.bind(("127.0.0.1", listen_port))
    server.listen(1)
    host, _ = server.accept()
    client = socket.create_connection(("127.0.0.1", target_port))
    while True:
        ready, _, _ = select.select([host, client], [], [])
        if host in ready:
            data = host.recv(4096)
            if not data:
                break
            client.sendall(to_client.apply(data))
        if client in ready:
            data = client.recv(4096)
            if not data:
                break
            to_client.armed = from_client.armed = True
            host.sendall(from_client.apply(data))


if __name__ == "__main__":
    main()

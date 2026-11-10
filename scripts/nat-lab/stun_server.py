#!/usr/bin/env python3
"""Простейший STUN-сервер для стенда: отвечает, с какого адреса пришёл запрос."""
import socket
import struct
import sys
import threading

MAGIC = 0x2112A442


def serve(ip, port):
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind((ip, port))
    while True:
        data, (src_ip, src_port) = sock.recvfrom(1500)
        if len(data) < 20 or struct.unpack("!HHI", data[:8]) != (1, 0, MAGIC):
            continue
        tid = data[8:20]
        xport = src_port ^ (MAGIC >> 16)
        xip = struct.unpack("!I", socket.inet_aton(src_ip))[0] ^ MAGIC
        attr = struct.pack("!HHBBHI", 0x0020, 8, 0, 1, xport, xip)
        sock.sendto(struct.pack("!HHI", 0x0101, len(attr), MAGIC) + tid + attr, (src_ip, src_port))


if __name__ == "__main__":
    port = int(sys.argv[1])
    for ip in sys.argv[2:]:
        threading.Thread(target=serve, args=(ip, port), daemon=True).start()
    threading.Event().wait()

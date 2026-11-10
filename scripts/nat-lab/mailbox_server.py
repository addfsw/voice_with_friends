#!/usr/bin/env python3
"""ntfy-совместимый почтовый ящик для стенда (только то, что нужно программе):
POST /<topic> — опубликовать; GET /<topic>/json[?since=<id>] — поток JSON-строк."""
import json
import sys
import threading
import time
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

lock = threading.Condition()
messages = {}  # topic -> [dict]


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.0"

    def log_message(self, fmt, *args):
        sys.stderr.write("mailbox: " + fmt % args + "\n")

    def do_POST(self):
        topic = urlparse(self.path).path.strip("/")
        body = self.rfile.read(int(self.headers.get("Content-Length", 0))).decode()
        msg = {"id": uuid.uuid4().hex[:12], "time": int(time.time()), "event": "message",
               "topic": topic, "message": body}
        with lock:
            messages.setdefault(topic, []).append(msg)
            lock.notify_all()
        out = json.dumps(msg).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(out)))
        self.end_headers()
        self.wfile.write(out)

    def do_GET(self):
        url = urlparse(self.path)
        parts = url.path.strip("/").split("/")
        if len(parts) != 2 or parts[1] != "json":
            self.send_error(404)
            return
        topic = parts[0]
        since = parse_qs(url.query).get("since", [None])[0]
        self.send_response(200)
        self.send_header("Content-Type", "application/x-ndjson")
        self.end_headers()

        def send(obj):
            self.wfile.write((json.dumps(obj) + "\n").encode())
            self.wfile.flush()

        with lock:
            history = messages.get(topic, [])
            if since is None:
                pos = len(history)
            else:
                ids = [m["id"] for m in history]
                pos = ids.index(since) + 1 if since in ids else 0
        send({"id": uuid.uuid4().hex[:12], "event": "open", "topic": topic})
        try:
            while True:
                with lock:
                    lock.wait_for(lambda: len(messages.get(topic, [])) > pos, timeout=30)
                    new = messages.get(topic, [])[pos:]
                    pos += len(new)
                if not new:
                    send({"event": "keepalive", "topic": topic})
                for m in new:
                    send(m)
        except (BrokenPipeError, ConnectionResetError):
            pass


if __name__ == "__main__":
    ThreadingHTTPServer((sys.argv[1], int(sys.argv[2])), Handler).serve_forever()

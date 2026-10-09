"""Minimal sync CDP client for nokk. Uses websocket-client (already installed)."""
import json, threading
from websocket import create_connection


class CDP:
    def __init__(self, ws_url):
        self.ws = create_connection(ws_url, timeout=120)
        self._id = 0
        self._lock = threading.Lock()

    def call(self, method, params=None, timeout=120, session=None):
        with self._lock:
            self._id += 1
            mid = self._id
        msg = {"id": mid, "method": method, "params": params or {}}
        if session:
            msg["sessionId"] = session
        self.ws.send(json.dumps(msg))
        import time
        end = time.time() + timeout
        while time.time() < end:
            msg = json.loads(self.ws.recv())
            if msg.get("id") == mid:
                if "error" in msg:
                    raise RuntimeError("%s %s" % (method, msg["error"]))
                return msg.get("result", {})
        raise TimeoutError(method)

    def close(self):
        self.ws.close()

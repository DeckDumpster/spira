#!/usr/bin/env python3
"""A stub Proxmox HTTPS API for test-round-vm-e2e.sh only: real TLS, real request/response
shapes (round-vm/DESIGN.md §3.3), but next_id/clone/start/stop/destroy answer synchronously
(no task polling — that loop is round-vm/src/pve.rs's own unit tests' job). agent/exec and
agent/file-write are NOT faked: each really execs inside the given podman container, so
round-vm's real key-delivery sequence has to land on a real filesystem.

Usage: round-vm-stub-pve.py <port> <container-name> <cert-pem> <key-pem> <reqlog-path>
"""
import http.server
import json
import ssl
import subprocess
import sys
import threading
from urllib.parse import parse_qsl, urlsplit

PORT = int(sys.argv[1])
CONTAINER = sys.argv[2]
CERT = sys.argv[3]
KEY = sys.argv[4]
REQLOG = sys.argv[5]

VMS = {}
EXEC_RESULTS = {}
_pid_counter = [1000]
_nextid_counter = [776]
_lock = threading.Lock()


def log_req(method, path, params, auth):
    with open(REQLOG, "a") as f:
        f.write(json.dumps({"method": method, "path": path, "params": params, "auth": auth}) + "\n")


def podman_exec(argv):
    r = subprocess.run(["podman", "exec", CONTAINER, *argv], capture_output=True)
    return r.returncode


def podman_write_file(path, content):
    r = subprocess.run(
        ["podman", "exec", "-i", CONTAINER, "sh", "-c", 'cat > "$1"', "_", path],
        input=content.encode(),
        capture_output=True,
    )
    return r.returncode


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def _send(self, data, code=200):
        body = json.dumps({"data": data}).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _params_get(self):
        q = urlsplit(self.path)
        return q.path, parse_qsl(q.query, keep_blank_values=True)

    def _params_post(self):
        q = urlsplit(self.path)
        length = int(self.headers.get("Content-Length", 0))
        body = self.rfile.read(length).decode()
        return q.path, parse_qsl(body, keep_blank_values=True)

    def do_GET(self):
        path, params = self._params_get()
        log_req("GET", path, params, self.headers.get("Authorization"))
        self._route("GET", path, params)

    def do_DELETE(self):
        path, params = self._params_get()
        log_req("DELETE", path, params, self.headers.get("Authorization"))
        self._route("DELETE", path, params)

    def do_POST(self):
        path, params = self._params_post()
        log_req("POST", path, params, self.headers.get("Authorization"))
        self._route("POST", path, params)

    def _route(self, method, path, params):
        node_prefix = "/api2/json/nodes/"
        try:
            if method == "GET" and path == "/api2/json/cluster/nextid":
                with _lock:
                    _nextid_counter[0] += 1
                    nid = str(_nextid_counter[0])
                return self._send(nid)
            if not path.startswith(node_prefix):
                return self._send(None)
            rest = path[len(node_prefix):]
            _, sub = rest.split("/", 1)
            if method == "GET" and sub == "qemu":
                with _lock:
                    return self._send([{"vmid": int(v), "name": d["name"]} for v, d in VMS.items()])
            parts = sub.split("/")
            vmid = parts[1]
            tail = "/".join(parts[2:])
            if method == "POST" and tail == "clone":
                d = dict(params)
                with _lock:
                    VMS[d["newid"]] = {"name": d.get("name", ""), "running": False}
                return self._send(None)
            if method == "POST" and tail == "status/start":
                with _lock:
                    VMS[vmid]["running"] = True
                return self._send(None)
            if method == "POST" and tail == "status/stop":
                with _lock:
                    if vmid in VMS:
                        VMS[vmid]["running"] = False
                return self._send(None)
            if method == "GET" and tail == "status/current":
                with _lock:
                    running = VMS.get(vmid, {}).get("running", False)
                return self._send({"status": "running" if running else "stopped"})
            if method == "DELETE" and tail == "":
                with _lock:
                    VMS.pop(vmid, None)
                return self._send(None)
            if method == "GET" and tail == "agent/network-get-interfaces":
                return self._send({"result": [{"name": "ens18", "ip-addresses": [{"ip-address-type": "ipv4", "ip-address": "127.0.0.1"}]}]})
            if method == "POST" and tail == "agent/exec":
                argv = [v for k, v in params if k == "command"]
                rc = podman_exec(argv)
                with _lock:
                    _pid_counter[0] += 1
                    pid = _pid_counter[0]
                    EXEC_RESULTS[pid] = rc
                return self._send({"pid": pid})
            if method == "GET" and tail == "agent/exec-status":
                pid = int(dict(params)["pid"])
                with _lock:
                    rc = EXEC_RESULTS.get(pid)
                if rc is None:
                    return self._send({"exited": 0})
                return self._send({"exited": 1, "exitcode": rc})
            if method == "POST" and tail == "agent/file-write":
                d = dict(params)
                rc = podman_write_file(d["file"], d.get("content", ""))
                if rc != 0:
                    return self._send({"error": "write failed"}, code=500)
                return self._send(None)
            self._send(None)
        except Exception as e:  # a malformed request is the stub's own 500, not a crash
            self._send({"error": str(e)}, code=500)


def main():
    server = http.server.ThreadingHTTPServer(("127.0.0.1", PORT), Handler)
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    ctx.load_cert_chain(CERT, KEY)
    server.socket = ctx.wrap_socket(server.socket, server_side=True)
    server.serve_forever()


if __name__ == "__main__":
    main()

"""A stand-in for the scheduler's control socket in a world that runs no `spira-runtime serve`:
answers every request with exit 0, and exits when the walk is done or the deadline passes."""
import json, os, socket, sys, time

sock_path, phase_file, deadline_s = sys.argv[1], sys.argv[2], float(sys.argv[3])
if os.path.exists(sock_path):
    os.unlink(sock_path)
srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
srv.bind(sock_path)
srv.listen(8)
srv.settimeout(1.0)
end = time.time() + deadline_s
while time.time() < end:
    try:
        phase = open(phase_file).read().strip()
    except OSError:
        phase = ""
    if phase == "done":
        break
    try:
        conn, _ = srv.accept()
    except socket.timeout:
        continue
    with conn:
        line = conn.makefile("r").readline()
        body = json.dumps({"stand_in": json.loads(line)}).encode() if line.strip() else b""
        conn.sendall(json.dumps({"exit_code": 0, "len": len(body)}).encode() + b"\n" + body)
srv.close()
if os.path.exists(sock_path):
    os.unlink(sock_path)

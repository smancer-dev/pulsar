#!/usr/bin/env python3
# Probe an mpv JSON-IPC socket to find which properties give real values on this
# build (mpv 0.34, --wid, --untimed). Used to validate Pulsar's Faz 4 metrics.
import socket, json, time, sys

SOCK = sys.argv[1] if len(sys.argv) > 1 else "/tmp/pulsar-mpv-0.sock"


def get(prop):
    s = socket.socket(socket.AF_UNIX)
    s.settimeout(1.5)
    s.connect(SOCK)
    s.sendall((json.dumps({"command": ["get_property", prop]}) + "\n").encode())
    buf = b""
    while b"\n" not in buf:
        d = s.recv(65536)
        if not d:
            break
        buf += d
    s.close()
    return json.loads(buf.split(b"\n")[0].decode())


def pass_count(vp):
    try:
        for grp in ("fresh", "redraw"):
            for p in vp["data"].get(grp, []):
                desc = p.get("desc", "")
                if "output" in desc or "screen" in desc:
                    return p.get("count", 0)
    except Exception as e:
        return None
    return None


a_vp = get("vo-passes")
a_cnt = pass_count(a_vp)
a_t = get("demuxer-cache-time").get("data")
time.sleep(1.5)
b_vp = get("vo-passes")
b_cnt = pass_count(b_vp)
b_t = get("demuxer-cache-time").get("data")

delta = (b_cnt - a_cnt) if isinstance(a_cnt, int) and isinstance(b_cnt, int) else None
print("vo-passes output count:", a_cnt, "->", b_cnt, "delta/1.5s=", delta,
      "=> fps~", round(delta / 1.5, 1) if isinstance(delta, int) else "n/a")
print("demuxer-cache-time advance/1.5s:",
      round(b_t - a_t, 3) if (a_t is not None and b_t is not None) else "n/a")
print("demuxer-cache-duration now:", get("demuxer-cache-duration").get("data"))

print("--- all pass groups ---")
try:
    for grp in ("fresh", "redraw"):
        for p in b_vp["data"].get(grp, []):
            print("  [{}] count={} desc={}".format(grp, p.get("count"), p.get("desc")))
except Exception as e:
    print("pass dump err", e)

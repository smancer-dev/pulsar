#!/usr/bin/env bash
# Emulate a lossy / slow path for adaptive-streaming tests (pulsar-core/docs/adaptive-streaming.md,
# Phase 0 acceptance: `loss 3% delay 120ms`, 5 minutes, no freeze > 300 ms).
#
# Needs root (tc). Shapes BOTH directions of one interface: egress with a netem qdisc, ingress
# via an IFB mirror (the video arrives inbound on the client). For a host+client on the SAME
# machine use `lo` (one qdisc covers both directions).
#
#   sudo scripts/netem.sh up   [-i IFACE] [--loss 3%] [--delay 120ms] [--jitter 20ms] [--rate 2mbit]
#   sudo scripts/netem.sh down [-i IFACE]
#   sudo scripts/netem.sh status [-i IFACE]
#
# Examples (from the desktop repo root):
#   sudo scripts/netem.sh up --loss 3% --delay 120ms                  # Phase 0 acceptance profile
#   sudo scripts/netem.sh up --rate 2mbit --delay 40ms                # fixed 2 Mbit cap (sawtooth check)
#   sudo scripts/netem.sh up -i lo --loss 3% --delay 60ms             # self-connect on one box (lo → 120 ms RTT)
#   sudo scripts/netem.sh down
set -euo pipefail

cmd=${1:-status}; shift || true
iface=""; loss=""; delay=""; jitter=""; rate=""
while [ $# -gt 0 ]; do
	case "$1" in
		-i|--iface) iface=$2; shift 2 ;;
		--loss) loss=$2; shift 2 ;;
		--delay) delay=$2; shift 2 ;;
		--jitter) jitter=$2; shift 2 ;;
		--rate) rate=$2; shift 2 ;;
		*) echo "unknown arg: $1" >&2; exit 2 ;;
	esac
done
if [ -z "$iface" ]; then
	iface=$(ip route show default 2>/dev/null | awk '{print $5; exit}')
	[ -n "$iface" ] || { echo "no default interface; pass -i IFACE" >&2; exit 2; }
fi
ifb=ifb-pulsar

netem_args() {
	local a=()
	[ -n "$delay" ] && { a+=(delay "$delay"); [ -n "$jitter" ] && a+=("$jitter"); }
	[ -n "$loss" ] && a+=(loss "$loss")
	[ -n "$rate" ] && a+=(rate "$rate")
	echo "${a[@]}"
}

case "$cmd" in
	up)
		[ "$(id -u)" = 0 ] || { echo "run with sudo" >&2; exit 1; }
		args=$(netem_args)
		[ -n "$args" ] || { echo "nothing to emulate: pass --loss/--delay/--rate" >&2; exit 2; }
		"$0" down -i "$iface" >/dev/null 2>&1 || true
		# Egress.
		tc qdisc add dev "$iface" root netem $args
		if [ "$iface" != lo ]; then
			# Ingress: redirect to an IFB device that carries the same netem.
			modprobe ifb numifbs=0 2>/dev/null || true
			ip link add "$ifb" type ifb 2>/dev/null || true
			ip link set "$ifb" up
			tc qdisc add dev "$iface" handle ffff: ingress
			tc filter add dev "$iface" parent ffff: protocol all u32 match u32 0 0 action mirred egress redirect dev "$ifb"
			tc qdisc add dev "$ifb" root netem $args
		fi
		echo "netem UP on $iface (both directions): $args"
		;;
	down)
		[ "$(id -u)" = 0 ] || { echo "run with sudo" >&2; exit 1; }
		tc qdisc del dev "$iface" root 2>/dev/null || true
		tc qdisc del dev "$iface" ingress 2>/dev/null || true
		tc qdisc del dev "$ifb" root 2>/dev/null || true
		ip link del "$ifb" 2>/dev/null || true
		echo "netem DOWN on $iface"
		;;
	status)
		echo "--- $iface"; tc qdisc show dev "$iface"
		ip link show "$ifb" >/dev/null 2>&1 && { echo "--- $ifb"; tc qdisc show dev "$ifb"; } || true
		;;
	*) echo "usage: $0 up|down|status [-i IFACE] [--loss 3%] [--delay 120ms] [--jitter 20ms] [--rate 2mbit]" >&2; exit 2 ;;
esac

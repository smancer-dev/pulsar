#!/usr/bin/env python3
"""On-Pi verification harness for the Linux chord-combo handling (kbdhook.rs).

Creates ONE persistent synthetic uinput keyboard and keeps its fd alive across the
whole run — exactly like a real keyboard (grabbed once by the capture thread before
any overlay opens, still readable while the grab is suspended). The Pulsar client's
evdev capture thread rescans (~1 Hz) and EVIOCGRABs new keyboards, so within ~1 s it
grabs this device. We then HOLD Ctrl+Shift and tap each trigger key in the sequence.

The fixed code gates the combo on the live kernel key state (EVIOCGKEY via
get_key_state) of every grabbed device, so the held Ctrl+Shift on THIS device fires
`overlay-toggle` (M) / `kbd-leave` (Q/F12). Watch the dbg_log for
`client-input: chord ... live=true/true` and the resulting behaviour.

  /dev/uinput is world-writable on the Pi, so no sudo is needed.

Usage:
  python3 synthcombo.py            # one Ctrl+Shift+M
  python3 synthcombo.py M,M        # open then close the overlay (persistent fd)
  python3 synthcombo.py M,M,Q      # open, close, then leave the session
"""
import sys
import time

from evdev import UInput, ecodes as e

caps = {
    e.EV_KEY: [
        e.KEY_A, e.KEY_ESC,            # makes kbdhook::wanted() accept it as a keyboard
        e.KEY_LEFTCTRL, e.KEY_RIGHTCTRL,
        e.KEY_LEFTSHIFT, e.KEY_RIGHTSHIFT,
        e.KEY_M, e.KEY_Q, e.KEY_F12,
    ]
}

ui = UInput(caps, name="pulsar-synth-kbd")
print("uinput:", ui.device.path, flush=True)
time.sleep(2.6)  # let kbdhook's ~1 s rescan enumerate + grab this device


def chord(key, label):
    print("=>", label, flush=True)
    ui.write(e.EV_KEY, e.KEY_LEFTCTRL, 1); ui.syn(); time.sleep(0.06)
    ui.write(e.EV_KEY, e.KEY_LEFTSHIFT, 1); ui.syn(); time.sleep(0.10)
    ui.write(e.EV_KEY, key, 1); ui.syn(); time.sleep(0.12)   # value=1 → the chord fires here
    ui.write(e.EV_KEY, key, 0); ui.syn(); time.sleep(0.06)
    ui.write(e.EV_KEY, e.KEY_LEFTSHIFT, 0); ui.syn(); time.sleep(0.06)
    ui.write(e.EV_KEY, e.KEY_LEFTCTRL, 0); ui.syn(); time.sleep(0.06)


keymap = {"M": e.KEY_M, "Q": e.KEY_Q, "F12": e.KEY_F12}
seq = (sys.argv[1] if len(sys.argv) > 1 else "M").upper().split(",")
for i, name in enumerate(seq):
    chord(keymap[name], "Ctrl+Shift+" + name)
    if i < len(seq) - 1:
        time.sleep(2.0)  # let each toggle settle (> the frontend 400 ms debounce)
print("done", flush=True)
time.sleep(0.3)
ui.close()

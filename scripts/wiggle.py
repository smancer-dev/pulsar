#!/usr/bin/env python3
"""Synthetic relative-mouse motion through a uinput device so kbdhook grabs it and
forwards to the host (reproduces "moving the mouse" for the stutter A/B). Arg: seconds."""
import sys, time, math
from evdev import UInput, ecodes as e

dur = float(sys.argv[1]) if len(sys.argv) > 1 else 12.0
caps = {e.EV_REL: [e.REL_X, e.REL_Y], e.EV_KEY: [e.BTN_LEFT, e.BTN_RIGHT]}
ui = UInput(caps, name="pulsar-synth-mouse")
print("uinput:", ui.device.path, flush=True)
time.sleep(2.5)  # let kbdhook's ~1Hz rescan grab the new device
t0 = time.time(); n = 0
while time.time() - t0 < dur:
    a = (time.time() - t0) * 6.0
    dx = int(13 * math.cos(a)); dy = int(13 * math.sin(a * 1.3))
    ui.write(e.EV_REL, e.REL_X, dx); ui.write(e.EV_REL, e.REL_Y, dy); ui.syn()
    n += 1
    time.sleep(0.004)  # ~250 Hz, a fast real mouse
print("done moves=%d rate=%.0f/s" % (n, n / dur), flush=True)
ui.close()

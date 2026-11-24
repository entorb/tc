"""Press X11 key combos via XTEST without implicit modifiers (xdotool adds Alt to F-keys on Xvfb).

usage: python3 ui_key.py COMBO...   COMBO = keysyms joined by "+", e.g. F1  Down  ctrl+f  shift+F4  alt+Return
"""

import ctypes
import sys
import time

ALIASES = {"ctrl": "Control_L", "shift": "Shift_L", "alt": "Alt_L", "super": "Super_L"}

x11 = ctypes.CDLL("libX11.so.6")
xtst = ctypes.CDLL("libXtst.so.6")
x11.XOpenDisplay.restype = ctypes.c_void_p
x11.XStringToKeysym.restype = ctypes.c_ulong
x11.XKeysymToKeycode.argtypes = [ctypes.c_void_p, ctypes.c_ulong]
xtst.XTestFakeKeyEvent.argtypes = [ctypes.c_void_p, ctypes.c_uint, ctypes.c_int, ctypes.c_ulong]
x11.XFlush.argtypes = [ctypes.c_void_p]

dpy = x11.XOpenDisplay(None)
if not dpy:
    sys.exit("ui_key: cannot open display")


def code(name: str) -> int:
    sym = x11.XStringToKeysym(ALIASES.get(name.lower(), name).encode())
    kc = x11.XKeysymToKeycode(dpy, sym) if sym else 0
    if not kc:
        sys.exit(f"ui_key: unknown key {name}")
    return kc


def send(kc: int, down: bool) -> None:
    xtst.XTestFakeKeyEvent(dpy, kc, int(down), 0)
    x11.XFlush(dpy)
    time.sleep(0.05)


for combo in sys.argv[1:]:
    codes = [code(k) for k in combo.split("+")]
    for kc in codes:
        send(kc, True)
    for kc in reversed(codes):
        send(kc, False)

#!/usr/bin/env python3
"""Deterministic audio fixtures for lactea (python3 stdlib only).

Writes 44100 Hz stereo IEEE-float32 WAV files next to this script:
  synth128.wav  20 s  128 bpm kick/hat/pad/lead (port of milk/tools/render.c)
  silence.wav    5 s  digital silence
  sweep.wav     10 s  logarithmic sine sweep 20 Hz -> 20 kHz (both channels, -6 dBFS)

Usage: gen_audio.py [outdir]
"""
import math
import os
import struct
import sys

SR = 44100


def write_wav(path, frames):
    """frames: list of (l, r) floats; written as WAVE_FORMAT_IEEE_FLOAT 32-bit stereo."""
    n = len(frames)
    flat = [x for fr in frames for x in fr]
    data = struct.pack("<%df" % len(flat), *flat)
    fmt = struct.pack("<HHIIHHH", 3, 2, SR, SR * 8, 8, 32, 0)  # cbSize = 0
    fact = struct.pack("<I", n)
    body = (
        b"WAVE"
        + b"fmt " + struct.pack("<I", len(fmt)) + fmt
        + b"fact" + struct.pack("<I", len(fact)) + fact
        + b"data" + struct.pack("<I", len(data)) + data
    )
    with open(path, "wb") as f:
        f.write(b"RIFF" + struct.pack("<I", len(body)) + body)


def synth128(seconds=20):
    beat = 60.0 / 128.0
    rng = 1
    out = []
    for t in range(seconds * SR):
        ts = t / SR
        bp = math.fmod(ts, beat)
        hp = math.fmod(ts + beat / 2, beat)
        bar = math.fmod(ts, beat * 16)
        kick = math.sin(2 * math.pi * (50 + 120 * math.exp(-bp * 30)) * bp) * math.exp(-bp * 8)
        rng = (rng * 1103515245 + 12345) & 0xFFFFFFFF
        hat = ((rng >> 16) / 32768.0 - 1) * math.exp(-hp * 40) * 0.3
        root = 110 if bar < beat * 8 else 98
        pad = 0.15 * (
            math.sin(2 * math.pi * root * ts)
            + math.sin(2 * math.pi * root * 1.5 * ts)
            + math.sin(2 * math.pi * root * 2.52 * ts) * (0.5 + 0.5 * math.sin(ts * 0.7))
        )
        up = 1 if (int(ts / beat * 2) % 4) == 3 else 0
        lead = (
            0.12
            * math.sin(2 * math.pi * root * 4 * (1 + 0.25 * up) * ts)
            * math.exp(-math.fmod(ts, beat / 2) * 6)
        )
        v = 0.6 * kick + hat + pad + lead
        out.append((v + 0.05 * math.sin(ts * 3), v))
    return out


def silence(seconds=5):
    return [(0.0, 0.0)] * (seconds * SR)


def sweep(seconds=10, f0=20.0, f1=20000.0, amp=0.5):
    # phase(t) = 2*pi*f0*T/ln(f1/f0) * (exp(t/T*ln(f1/f0)) - 1)
    k = math.log(f1 / f0)
    out = []
    for i in range(seconds * SR):
        t = i / SR
        ph = 2 * math.pi * f0 * seconds / k * (math.exp(t / seconds * k) - 1)
        v = amp * math.sin(ph)
        out.append((v, v))
    return out


def main():
    if len(sys.argv) > 1 and sys.argv[1].startswith("-"):
        print(f"usage: {os.path.basename(sys.argv[0])} [OUTDIR]  (default: this script's directory)")
        sys.exit(0 if sys.argv[1] in ("-h", "--help") else 2)
    outdir = sys.argv[1] if len(sys.argv) > 1 else os.path.dirname(os.path.abspath(__file__))
    os.makedirs(outdir, exist_ok=True)
    write_wav(os.path.join(outdir, "synth128.wav"), synth128())
    write_wav(os.path.join(outdir, "silence.wav"), silence())
    write_wav(os.path.join(outdir, "sweep.wav"), sweep())


if __name__ == "__main__":
    main()

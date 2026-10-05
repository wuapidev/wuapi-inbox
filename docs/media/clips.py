#!/usr/bin/env python3
"""The clips of the README. Run by clips.sh."""
import glob
import os
import shutil
import subprocess
import sys
import tempfile
import time

root = sys.argv[1]
names = sys.argv[2:] or ["start", "chats", "search", "palette"]
binary = os.path.join(root, "target/capture-build/release/wuapi-inbox")
media = os.path.join(root, "docs/media")
sheets = os.path.join(root, "target/clips")
os.makedirs(sheets, exist_ok=True)

RATE = 20               # frames a second, recorded and played
BAR = 48                # the height of the timer under the start clip
PREVIEW_RATE = 10       # the WebP the README shows
PREVIEW_WIDTH = 720
SLOT = 1_000_000 // RATE


def run(*command, **more):
    subprocess.run(command, check=True, **more)


def record(name, work):
    """Runs the script; returns when the process was started, what it
    printed as {label: µs since 1970}, the frame size, and the frames as
    (µs since the first, bytes)."""
    data = tempfile.mkdtemp(dir=work)
    env = dict(os.environ, WUAPI_INBOX_CAPTURE_DIR=work,
               WUAPI_INBOX_CAPTURE_SCRIPT=os.path.join(media, "capture", f"clip-{name}.txt"))
    started = time.time_ns() // 1000
    done = subprocess.run(
        [binary, "--provider", "mock", "--no-keychain", "--data-dir", data, "--theme", "light",
         "--app-id", "dev.wuapi.inbox.capture"],
        env=env, text=True, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    at = {}
    for line in done.stderr.splitlines():
        words = line.split()
        if words[:2] == ["capture:", "at"]:
            at[words[2]] = int(words[3])
        elif line.startswith("capture: dropped"):
            print(line)
    with open(os.path.join(work, f"{name}.txt")) as times:
        width, height = map(int, times.readline().split())
        taken = [int(line) for line in times if line.strip()]
    return started, at, (width, height), taken, os.path.join(work, f"{name}.rgb")


def clip(name):
    work = tempfile.mkdtemp(dir=sheets)
    started, at, (width, height), taken, pixels = record(name, work)
    frame_bytes = width * height * 3
    # The start clip begins when the process was started: nothing is on the
    # screen until the first frame, and the timer under it runs until the
    # chat list is drawn.
    lead = (at[f"record-{name}"] - started) if name == "start" else 0
    slots = (lead + taken[-1]) // SLOT + 1
    command = ["ffmpeg", "-v", "error", "-y", "-f", "rawvideo", "-pix_fmt", "rgb24",
               "-s", f"{width}x{height}", "-r", str(RATE), "-i", "-"]
    if name == "start":
        listed = (at["chats"] - started) / 1e6
        texts = []
        for slot in range(slots):
            seconds = slot / RATE
            texts.append(f"{seconds:.2f} s   starting" if seconds < listed
                         else f"{listed:.2f} s   from the start of the process to the chat list drawn")
        distinct = sorted(set(texts))
        arguments = []
        for index, text in enumerate(distinct):
            arguments += [os.path.join(work, f"text{index}.png"), text]
        run("swift", os.path.join(media, "caption.swift"), str(width), str(BAR), *arguments)
        for slot, text in enumerate(texts):
            shutil.copy(os.path.join(work, f"text{distinct.index(text)}.png"),
                        os.path.join(work, f"bar{slot:04d}.png"))
        command += ["-r", str(RATE), "-i", os.path.join(work, "bar%04d.png"),
                    "-filter_complex", "[0:v][1:v]vstack"]
        print(f"start: the chat list drawn {listed:.3f} s after the process was started; "
              f"the first frame saved at {lead / 1e6:.3f} s")
    mp4 = os.path.join(media, f"clip-{name}.mp4")
    command += ["-c:v", "libx264", "-preset", "veryslow", "-crf", "22", "-pix_fmt", "yuv420p",
                "-movflags", "+faststart", "-r", str(RATE), mp4]
    encoder = subprocess.Popen(command, stdin=subprocess.PIPE)
    blank = bytes([18, 20, 18]) * (width * height)     # nothing on the screen yet
    with open(pixels, "rb") as frames:
        current, index = None, -1
        for slot in range(slots):
            # The newest frame taken by this moment of the clip.
            moment = slot * SLOT - lead
            while index + 1 < len(taken) and taken[index + 1] <= moment + SLOT // 2:
                current = frames.read(frame_bytes)
                index += 1
            encoder.stdin.write(current if current is not None else blank)
    encoder.stdin.close()
    if encoder.wait() != 0:
        sys.exit(f"{name}: ffmpeg failed")

    stills = os.path.join(work, "stills")
    os.makedirs(stills)
    run("ffmpeg", "-v", "error", "-i", mp4, "-vf",
        f"fps={PREVIEW_RATE},scale={PREVIEW_WIDTH}:-2:flags=lanczos", os.path.join(stills, "%04d.png"))
    webp = os.path.join(media, f"clip-{name}.webp")
    run("img2webp", "-loop", "0", "-lossy", "-q", "72", "-m", "6", "-d", str(1000 // PREVIEW_RATE),
        *sorted(glob.glob(os.path.join(stills, "*.png"))), "-o", webp, stdout=subprocess.DEVNULL)
    # Every fifth frame of the clip, to look at.
    run("ffmpeg", "-v", "error", "-y", "-i", mp4, "-vf",
        f"select=not(mod(n\\,5)),scale=440:-2,tile=6x{(slots // 5) // 6 + 1}", "-frames:v", "1",
        os.path.join(sheets, f"{name}-sheet.png"))
    shutil.rmtree(work)
    print(f"{name}: {slots / RATE:.2f} s, {len(taken)} frames, "
          f"mp4 {os.path.getsize(mp4) / 1e6:.2f} MB, webp {os.path.getsize(webp) / 1e6:.2f} MB")


for name in names:
    clip(name)

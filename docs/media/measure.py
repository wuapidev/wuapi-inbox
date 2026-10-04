#!/usr/bin/env python3
"""The measurements of docs/media/measurements.md. Run by measure.sh.

Prints a report in Markdown and writes it to OUT/report.md, with the raw
samples in OUT/samples.json.
"""
import json
import os
import platform
import re
import shutil
import statistics
import subprocess
import sys
import tempfile
import time

root, out = sys.argv[1], sys.argv[2]
shipped = os.path.join(root, "target/release/wuapi-inbox")
timing = os.path.join(root, "target/capture-build/release/wuapi-inbox")
bench = os.path.join(root, "target/capture-build/release/examples/store_bench")
scripts = os.path.join(root, "docs/media/capture")
scratch = os.path.join(out, "tmp")
os.makedirs(scratch, exist_ok=True)

STARTS = 20        # launches timed
USES = 5           # runs of measure-use.txt
MEMORY_RUNS = 3    # processes per memory state
SETTLE = 10        # seconds after the start before a first reading
IDLE = 300         # seconds left alone before the idle reading
MINUTE = 60        # the window the idle processor time is taken over

samples = {}
lines = []


def say(line=""):
    print(line, flush=True)
    lines.append(line)


def flags(data, extra=()):
    return ["--provider", "mock", "--no-keychain", "--data-dir", data,
            "--theme", "light", "--app-id", "dev.wuapi.inbox.capture", *extra]


def scripted(script, extra=()):
    """Runs the timing build through a script; returns when it was started
    (µs since 1970) and the lines it printed."""
    data = tempfile.mkdtemp(dir=scratch)
    env = dict(os.environ, WUAPI_INBOX_CAPTURE_SCRIPT=os.path.join(scripts, script),
               WUAPI_INBOX_CAPTURE_DIR=data)
    started = time.time_ns() // 1000
    done = subprocess.run([timing, *flags(data, extra)], env=env, text=True,
                          stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    shutil.rmtree(data, ignore_errors=True)
    return started, [line.split()[1:] for line in done.stderr.splitlines()
                     if line.startswith("capture: ")]


def spread(values, unit="ms", digits=1):
    values = sorted(values)
    return (f"{statistics.median(values):.{digits}f} {unit} "
            f"({values[0]:.{digits}f} to {values[-1]:.{digits}f}, n={len(values)})")


def output(*command):
    return subprocess.run(command, text=True, capture_output=True).stdout


# ----- the machine ---------------------------------------------------------

hardware = output("system_profiler", "SPHardwareDataType")
field = lambda name: (re.search(name + r": (.*)", hardware) or [None, "?"])[1]
locked = "CGSSessionScreenIsLocked" in output("ioreg", "-n", "Root", "-d1")
say(f"- Machine: {field('Model Name')} ({field('Model Identifier')}), "
    f"{field('Chip')}, {field('Memory')}")
say(f"- System: macOS {platform.mac_ver()[0]} ({output('sw_vers', '-buildVersion').strip()})")
say(f"- Screen: {'locked' if locked else 'unlocked'} while measuring")
say(f"- Source: commit {output('git', '-C', root, 'rev-parse', '--short', 'HEAD').strip()}"
    + (" with uncommitted changes" if output("git", "-C", root, "status", "--porcelain").strip() else ""))
say(f"- Compiler: {output('rustc', '--version').strip()}, release profile")
say(f"- Date: {time.strftime('%Y-%m-%d %H:%M %z')}")
say(f"- Binary as shipped (`target/release/wuapi-inbox`): "
    f"{os.path.getsize(shipped) / 1e6:.1f} MB")
say()

# ----- start ---------------------------------------------------------------

starts = {"main": [], "window": [], "chats": []}
for _ in range(STARTS):
    started, printed = scripted("measure-start.txt")
    at = {line[1]: int(line[2]) for line in printed if line[0] == "at"}
    for name in starts:
        starts[name].append((at[name] - started) / 1000)
    time.sleep(0.5)
samples["start_ms"] = starts
say("## Start")
say()
say("| From the process being started to | Median (range) |")
say("|---|---|")
say(f"| `main` running | {spread(starts['main'])} |")
say(f"| the window open with its first frame drawn | {spread(starts['window'], digits=0)} |")
say(f"| the chat list drawn | {spread(starts['chats'], digits=0)} |")
say()

# ----- use -----------------------------------------------------------------

first, again, search, frames, same = [], [], [], [], 0
rows_after_search = []
for _ in range(USES):
    _, printed = scripted("measure-use.txt")
    before = None
    for line in printed:
        if line[0] == "time":
            label, micros, list_rows, thread_rows, chat = line[1], int(line[2]), int(line[3]), int(line[4]), line[5]
            if label == "search":
                search.append(micros / 1000)
                rows_after_search.append(list_rows)
            elif chat == before:
                same += 1     # the list moved under the click: the same chat again
            else:
                (first if label == "first" else again).append(micros / 1000)
                if label == "open":
                    samples.setdefault("rows_at_open", []).append(thread_rows)
            before = chat
        elif line[0] == "glide":
            frames += [int(micros) / 1000 for micros in line[1:]]
samples.update(first_open_ms=first, open_ms=again, search_ms=search, frame_ms=frames,
               list_rows_while_searching=rows_after_search)
frames.sort()
say("## Opening a chat, searching, scrolling")
say()
say("From the input event to the frame laid out and painted into a scene, "
    "on the demo data.")
say()
say("| What | Median (range) |")
say("|---|---|")
say(f"| Open a chat whose history is in the local database | {spread(again)} |")
say(f"| Open a chat for the first time (what is stored; the history comes after) | {spread(first)} |")
say(f"| One keystroke in the search field, results drawn | {spread(search)} |")
say(f"| One frame while scrolling a conversation | {spread(frames)}, "
    f"95th percentile {frames[int(len(frames) * 0.95)]:.1f} ms |")
say()
say(f"Rows in the conversations opened: {min(samples.get('rows_at_open', [0]))} to "
    f"{max(samples.get('rows_at_open', [0]))}. Clicks that landed on the chat already open "
    f"and were left out: {same}.")
say()

# ----- the store over a long history ----------------------------------------

say("## The local store over 100,000 messages")
say()
say("```text")
for line in output(bench, "100000", os.path.join(scratch, "store-bench")).splitlines():
    say(line)
say("```")
say()

# ----- memory and processor ------------------------------------------------


def reading(pid):
    """Resident size and physical footprint in MB, threads, children."""
    rss = int(output("ps", "-o", "rss=", "-p", str(pid)).strip()) / 1024
    report = output("footprint", "-p", str(pid), "-f", "bytes", "--noCategories")
    found = re.search(r"phys_footprint: (\d+)", report) or re.search(r"Footprint: (\d+) B", report)
    footprint = int(found[1]) / 1048576 if found else float("nan")
    children = output("pgrep", "-P", str(pid)).split()
    return {"rss": rss, "footprint": footprint, "children": len(children)}


def processor_seconds(pid):
    minutes, seconds = output("ps", "-o", "time=", "-p", str(pid)).strip().split(":")
    return int(minutes) * 60 + float(seconds)


def launch(binary, extra=(), script=None):
    data = tempfile.mkdtemp(dir=scratch)
    env = dict(os.environ)
    if script:
        env.update(WUAPI_INBOX_CAPTURE_SCRIPT=os.path.join(scripts, script),
                   WUAPI_INBOX_CAPTURE_DIR=data)
    return subprocess.Popen([binary, *flags(data, extra)], env=env,
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL), data


listed = [launch(shipped) for _ in range(MEMORY_RUNS)]
opened = [launch(shipped, ("--open-chat", "1")) for _ in range(MEMORY_RUNS)]
used = [launch(timing, script="measure-memory.txt") for _ in range(MEMORY_RUNS)]
time.sleep(SETTLE)
memory = {
    "chat list, 10 s after the start": [reading(p.pid) for p, _ in listed],
    "a conversation open, 10 s after the start": [reading(p.pid) for p, _ in opened],
}
time.sleep(30)     # measure-memory.txt takes about 25 s
memory["six chats opened and scrolled back (timing build)"] = [reading(p.pid) for p, _ in used]
time.sleep(IDLE - MINUTE - SETTLE - 30)
before = [processor_seconds(p.pid) for p, _ in listed + opened]
time.sleep(MINUTE)
after = [processor_seconds(p.pid) for p, _ in listed + opened]
memory[f"chat list, after {IDLE // 60} minutes left alone"] = [reading(p.pid) for p, _ in listed]
memory[f"a conversation open, after {IDLE // 60} minutes left alone"] = [reading(p.pid) for p, _ in opened]
memory[f"six chats opened and scrolled back, after {IDLE // 60} minutes (timing build)"] = [
    reading(p.pid) for p, _ in used]
total = [processor_seconds(p.pid) for p, _ in listed + opened]
for process, data in listed + opened + used:
    process.terminate()
    process.wait()
    shutil.rmtree(data, ignore_errors=True)
samples["memory_mb"] = memory
idle = [(b - a) / MINUTE * 100 for a, b in zip(before, after)]
samples["idle_cpu_percent"] = idle
samples["cpu_seconds_in_5_min"] = total

say("## Memory")
say()
say("| State | Physical footprint | Resident size |")
say("|---|---|---|")
for state, readings in memory.items():
    say(f"| {state} | {spread([r['footprint'] for r in readings], 'MB')} | "
        f"{spread([r['rss'] for r in readings], 'MB')} |")
say()
children = max(r["children"] for readings in memory.values() for r in readings)
say(f"Child processes of the application in any reading: {children}.")
say()
say("## Processor, left alone")
say()
say(f"Processor time used over one minute, from {(IDLE - MINUTE) // 60} to {IDLE // 60} minutes "
    f"after the start, as a share of one core: {spread(idle, '%', 2)}. "
    f"Processor time used in the whole {IDLE // 60} minutes, start included: "
    f"{spread(total, 's', 2)}.")
say()

shutil.rmtree(scratch, ignore_errors=True)
with open(os.path.join(out, "report.md"), "w") as report:
    report.write("\n".join(lines) + "\n")
with open(os.path.join(out, "samples.json"), "w") as raw:
    json.dump(samples, raw, indent=1)

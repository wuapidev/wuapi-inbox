# How we measured

Every number in the README comes from this page, and this page comes from one run of `docs/media/measure.sh`. Nothing here is an estimate or a figure from another product.

## The run

- Machine: MacBook Pro (MacBookPro18,1), Apple M1 Pro, 16 GB
- System: macOS 26.6.2 (25G83)
- Screen: unlocked while measuring
- Source: commit a94d4d0 with uncommitted changes
- Compiler: rustc 1.99.0 (b940084d7 2026-09-28), release profile
- Date: 2026-10-04 04:36 -0300
- Binary as shipped (`target/release/wuapi-inbox`): 50.4 MB
- Published download, 0.1.0, macOS on Apple Silicon: `.dmg` 22,484,012 bytes (22.5 MB), `.tar.gz` 20,051,010 bytes (20.1 MB), as the releases page lists them

The source is the commit above plus the changes of this pull request: the timing steps of the `capture` feature, the scripts on this page, and one change to the search query (see "The local store over 100,000 messages").

```sh
docs/media/measure.sh        # builds, measures, writes target/measure/report.md and samples.json
```

Each figure is the median of the runs, with the lowest and the highest in brackets and the number of samples. The raw samples of this run are in [measurements.json](measurements.json).

## What ran

- The provider is the built-in demo (`--provider mock`): two numbers, and conversations of 15 to 53 rows (date dividers included) among those opened. No network is involved. The demo invents its world at every start, so each start also loads the chats into the store; with a real account they are already in the database.
- `--no-keychain` and a scratch `--data-dir`: no keychain is read, and with no key to be had the database is kept in memory instead of an encrypted file. The owner's data and the wuapi provider were never used. The store over a file encrypted with SQLCipher is timed separately, below.
- A 1240 x 800 point window on a display with two pixels to the point, light theme.
- Memory, processor and size are from the binary as it ships (`cargo build --release -p wuapi-inbox`). Times are from the same code built with the `capture` feature, which adds the steps that drive the window and print the times (`crates/app/src/capture.rs`); they are not in a release build.
- The file cache was warm: the binary had been run before. A first start after a restart of the machine was not measured.

## Start


| From the process being started to | Median (range) |
|---|---|
| `main` running | 20.7 ms (10.2 to 26.5, n=20) |
| the window open with its first frame drawn | 510 ms (485 to 599, n=20) |
| the chat list drawn | 570 ms (541 to 663, n=20) |

The script that started the process took the time just before starting it; the application printed the time when `main` began, when `open_window` returned (GPUI draws the first frame before it returns) and when the chat list had rows and its frame was drawn. 20 starts, one after the other.

## Opening a chat, searching, scrolling

From the input event to the frame laid out and painted into a scene, on the demo data. That is the application's own work on its main thread: the database query, the layout and the paint. The GPU then draws the scene and the display shows it at its next refresh; those two are not in the figures. Pictures are decoded in the background and can arrive a frame or more later.

| What | Median (range) |
|---|---|
| Open a chat whose history is in the local database | 13.2 ms (6.2 to 22.8, n=60) |
| Open a chat for the first time (what is stored; the history comes after) | 8.2 ms (3.5 to 19.4, n=30) |
| One keystroke in the search field, results drawn | 8.3 ms (4.3 to 22.3, n=40) |
| One frame while scrolling a conversation | 3.8 ms (2.4 to 11.0, n=1190), 95th percentile 4.9 ms |

Rows in the conversations opened: 15 to 53. Clicks that landed on the chat already open and were left out: 0.

Five runs of `docs/media/capture/measure-use.txt`: six chats opened once, the same six opened twice more, a conversation scrolled back by 2,400 points and down again at 60 steps a second, and "Thursday" typed into the search field a letter at a time. Each keystroke searches the chat names and the message history and draws the list again.

## The local store over 100,000 messages

The demo history is too small to say anything about a long one, so the store is timed by itself over a made-up history: `crates/client-core/examples/store_bench.rs`, a file encrypted with SQLCipher, 100,000 messages in 200 chats, the words drawn from 5,000 invented ones with the common ones far more frequent. These are the queries the window runs to list the chats, to open one (its newest 120 messages) and to search (the newest 40 matches); the time to lay out and paint is not in them.

```text
history: 100000 messages in 200 chats, written in 4.4 s; database file 35.4 MB (encrypted)
chat list (200 chats): median 3.221 ms, range 2.987 to 6.152 ms, 30 runs
open a chat (newest 120 messages of one chat): median 0.939 ms, range 0.795 to 1.258 ms, 30 runs
search, the most common word (`bababa`, 40 results shown of at most 40): median 81.758 ms, range 76.209 to 509.439 ms, 30 runs
search, a rare word (`tudebe`, 40 results shown of at most 40): median 0.601 ms, range 0.591 to 1.193 ms, 30 runs
search, two words (`bobaba pababa`, 40 results shown of at most 40): median 1.021 ms, range 1.019 to 1.951 ms, 30 runs
search, two letters, as typed (`ba`, 40 results shown of at most 40): median 90.598 ms, range 84.259 to 119.433 ms, 30 runs
```

The most common word is the worst case for a search: it is drawn far more often than any other, and every match is read and sorted by time before the newest 40 are kept. One of its 30 runs took 509 ms.

This benchmark found a defect, fixed in the same change: the search query let SQLite choose the join order, and it walked the messages by time and asked the full-text index again for each one. Before the fix the most common word took 17.6 ms over 1,000 messages and 276 ms over 5,000, and a search over 20,000 did not finish in five minutes. `search_stays_quick_over_a_long_history` in `crates/client-core/src/tests.rs` fails without the fix (8.2 s for two searches over 20,000 messages) and passes with it.

## Memory

Physical footprint is what `footprint -p <pid>` reports and what Activity Monitor shows as Memory; resident size is `ps -o rss`. Three processes for each state, all nine running at the same time.

| State | Physical footprint | Resident size |
|---|---|---|
| chat list, 10 s after the start | 114.7 MB (114.6 to 114.9, n=3) | 91.4 MB (90.6 to 91.5, n=3) |
| a conversation open, 10 s after the start | 119.9 MB (118.4 to 120.8, n=3) | 98.1 MB (94.6 to 103.4, n=3) |
| six chats opened and scrolled back (timing build) | 150.0 MB (144.7 to 161.6, n=3) | 144.0 MB (141.1 to 152.8, n=3) |
| chat list, after 5 minutes left alone | 114.7 MB (114.5 to 114.9, n=3) | 71.3 MB (71.2 to 71.3, n=3) |
| a conversation open, after 5 minutes left alone | 119.9 MB (118.4 to 120.8, n=3) | 80.0 MB (71.9 to 80.0, n=3) |
| six chats opened and scrolled back, after 5 minutes (timing build) | 150.3 MB (144.9 to 161.8, n=3) | 75.8 MB (74.8 to 83.4, n=3) |

Child processes of the application in any reading: 0.

The application is one process: it starts no helper, renderer or GPU process. The states marked "timing build" need the window driven, so they use the build with the `capture` feature (`docs/media/capture/measure-memory.txt`: six chats opened, each scrolled back by 2,400 points). Resident size falls when the system compresses or pages out memory that is not touched; the footprint counts it still.

## Processor, left alone

Processor time used over one minute, from 4 to 5 minutes after the start, as a share of one core: 0.08 % (0.07 to 0.08, n=6). Processor time used in the whole 5 minutes, start included: 0.99 s (0.88 to 1.07, n=6).


Six processes of the shipped build, three on the chat list and three with a conversation open, from `ps -o time` (hundredths of a second) at the start and the end of the minute. The demo provider keeps sending live messages during that minute, as a busy account would.

## The clips

`docs/media/clips.sh` records the four clips of the README with the same `capture` feature: the application saves its own frame 20 times a second into an 880 x 560 point window's worth of pixels while a script drives it, and the clips play at the speed they were recorded. Saving a frame takes the main thread between 33 and 50 ms, so the application is slower while it is being recorded than in the figures above, which were taken without recording. The timer under the start clip is that recording's own time from the launch of the process to the chat list drawn, 0.62 s, inside the range measured above; the dark frames before it are the time in which nothing was on the screen.

## What is not here

- No comparison with another client. WhatsApp for Mac is installed on this machine but was not running, and starting it would have opened a personal account; a browser tab or another client with a real account holds a different history from the demo's, so its memory would not be comparable either. The figures above are absolute.
- Windows and Linux were not measured.
- A first start after a restart of the machine, with nothing in the file cache.
- A real account: the wuapi provider was not used. With it, the first sync depends on the network; what the window does afterwards is the same local queries.
- The time the GPU takes to draw a frame and the delay until the display shows it.

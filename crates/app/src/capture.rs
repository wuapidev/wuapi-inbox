//! Frames of the window saved as PNG files, for the documentation.
//!
//! A development aid behind the `capture` feature, never in a release
//! build. The application draws its own frame into a texture and writes it
//! out, so nothing else on the screen can end up in the picture and no
//! screen-recording permission is involved. The native title bar is the
//! system's, not ours, and is not in the frame.
//!
//! `WUAPI_INBOX_CAPTURE_SCRIPT` names a script and `WUAPI_INBOX_CAPTURE_DIR`
//! the directory the frames go to (default: the current one). The script
//! has one step per line; `#` starts a comment:
//!
//! ```text
//! size 1240 800        # resize the window, in points
//! wait 1500            # milliseconds
//! key cmd-k            # a keystroke, as the keymap spells it
//! type Lisbon          # text, one character at a time
//! move 300 200         # the pointer, in points from the top left
//! click 300 200
//! scroll 700 400 -240  # at a point, by so many points down
//! shot chats-light     # writes chats-light.png
//! quit
//! ```
//!
//! The same scripts take the measurements and the clips of
//! `docs/media/measurements.md` (see `docs/media/measure.sh` and
//! `docs/media/clips.sh`), with a few more steps:
//!
//! ```text
//! until chats 10000        # wait for the chat list (or `messages`: the
//!                          # open conversation), at most so long; then
//!                          # print `capture: at chats <µs since 1970>`
//! time open click 200 300  # do the step, draw the frame, and print
//!                          # `capture: time open <µs> ...` (see `timed`)
//! glide 800 400 -900 1500  # scroll by so many points over so long, a
//!                          # frame at a time, and print each frame's µs
//! record clip 30           # start saving frames at this rate: clip.rgb
//!                          # (raw RGB) and clip.txt (size, then the µs
//!                          # of each frame since the first)
//! stop                     # end the recording
//! ```

use crate::ui::AppView;
use gpui_kit::{
    point, px, size, AnyWindowHandle, App, AppContext as _, AsyncApp, Entity, Keystroke, Modifiers,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PlatformInput, ScrollDelta,
    ScrollWheelEvent, Task, TouchPhase, Window,
};
use std::cell::Cell;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The variable that names the script.
const SCRIPT: &str = "WUAPI_INBOX_CAPTURE_SCRIPT";
/// The variable that names where the frames go.
const DIR: &str = "WUAPI_INBOX_CAPTURE_DIR";

/// One line of the script.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Step {
    Size(f32, f32),
    Wait(Duration),
    Key(String),
    Type(String),
    Move(f32, f32),
    Click(f32, f32),
    Scroll(f32, f32, f32),
    Shot(String),
    Until(Ready, Duration),
    Time(String, Box<Step>),
    Glide(f32, f32, f32, Duration),
    Record(String, u32),
    Stop,
    Quit,
}

/// What `until` waits for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Ready {
    /// The chat list has rows.
    Chats,
    /// The open conversation has rows.
    Messages,
}

/// Reads a script. An unknown step or a missing number is an error that
/// names the line, so a typo does not become a silently shorter run.
pub(crate) fn parse(script: &str) -> Result<Vec<Step>, String> {
    let mut steps = Vec::new();
    for (index, line) in script.lines().enumerate() {
        let line = line.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        let (name, rest) = line.split_once(' ').unwrap_or((line, ""));
        let rest = rest.trim();
        let numbers = |count: usize| -> Result<Vec<f32>, String> {
            let numbers: Vec<f32> = rest
                .split_whitespace()
                .map(str::parse)
                .collect::<Result<_, _>>()
                .map_err(|_| format!("line {}: `{name}` takes numbers", index + 1))?;
            if numbers.len() == count {
                Ok(numbers)
            } else {
                Err(format!(
                    "line {}: `{name}` takes {count} numbers",
                    index + 1
                ))
            }
        };
        let text = || -> Result<String, String> {
            if rest.is_empty() {
                Err(format!("line {}: `{name}` needs a value", index + 1))
            } else {
                Ok(rest.to_owned())
            }
        };
        steps.push(match name {
            "size" => {
                let n = numbers(2)?;
                Step::Size(n[0], n[1])
            }
            "wait" => Step::Wait(Duration::from_millis(numbers(1)?[0] as u64)),
            "key" => Step::Key(text()?),
            "type" => Step::Type(text()?),
            "move" => {
                let n = numbers(2)?;
                Step::Move(n[0], n[1])
            }
            "click" => {
                let n = numbers(2)?;
                Step::Click(n[0], n[1])
            }
            "scroll" => {
                let n = numbers(3)?;
                Step::Scroll(n[0], n[1], n[2])
            }
            "shot" => Step::Shot(text()?),
            "until" => {
                let (what, limit) = rest.split_once(' ').unwrap_or((rest, ""));
                let ready = match what {
                    "chats" => Ready::Chats,
                    "messages" => Ready::Messages,
                    _ => {
                        return Err(format!(
                            "line {}: `until` takes chats or messages",
                            index + 1
                        ))
                    }
                };
                let limit = limit
                    .trim()
                    .parse()
                    .map_err(|_| format!("line {}: `until` takes a time limit", index + 1))?;
                Step::Until(ready, Duration::from_millis(limit))
            }
            "time" => {
                let (label, inner) = rest.split_once(' ').unwrap_or((rest, ""));
                let inner = parse(inner)
                    .map_err(|error| error.replacen("line 1", &format!("line {}", index + 1), 1))?;
                match inner.as_slice() {
                    [step @ (Step::Key(_) | Step::Type(_) | Step::Click(..) | Step::Scroll(..))] => {
                        Step::Time(label.to_owned(), Box::new(step.clone()))
                    }
                    _ => {
                        return Err(format!(
                            "line {}: `time` takes a name and a key, type, click or scroll step",
                            index + 1
                        ))
                    }
                }
            }
            "glide" => {
                let n = numbers(4)?;
                Step::Glide(n[0], n[1], n[2], Duration::from_millis(n[3] as u64))
            }
            "record" => {
                let (name, rate) = rest.split_once(' ').unwrap_or((rest, ""));
                let rate = rate
                    .trim()
                    .parse()
                    .ok()
                    .filter(|rate| (1..=60).contains(rate) && !name.is_empty())
                    .ok_or_else(|| {
                        format!(
                            "line {}: `record` takes a name and a rate of 1 to 60",
                            index + 1
                        )
                    })?;
                Step::Record(name.to_owned(), rate)
            }
            "stop" => Step::Stop,
            "quit" => Step::Quit,
            other => return Err(format!("line {}: unknown step `{other}`", index + 1)),
        });
    }
    Ok(steps)
}

/// Whether this run is taking frames.
pub(crate) fn running() -> bool {
    std::env::var_os(SCRIPT).is_some()
}

/// Runs the script named in the environment against the window, if there
/// is one.
pub(crate) fn start(window: AnyWindowHandle, view: Entity<AppView>, cx: &mut App) {
    let Some(path) = std::env::var_os(SCRIPT) else {
        return;
    };
    let steps = match std::fs::read_to_string(&path)
        .map_err(|error| error.to_string())
        .and_then(|script| parse(&script))
    {
        Ok(steps) => steps,
        Err(error) => {
            eprintln!("capture: {}: {error}", PathBuf::from(path).display());
            std::process::exit(2);
        }
    };
    let dir = std::env::var_os(DIR).map(PathBuf::from).unwrap_or_default();
    // `open_window` has drawn the first frame by the time it returns.
    at("window");
    cx.spawn(async move |cx| {
        let mut recording: Option<Recording> = None;
        for step in steps {
            match step {
                Step::Wait(time) => cx.background_executor().timer(time).await,
                Step::Until(ready, limit) => until(ready, limit, window, &view, cx).await,
                Step::Time(label, step) => {
                    if let Err(error) = timed(&label, &step, &dir, window, &view, cx) {
                        eprintln!("capture: {error}");
                    }
                }
                Step::Glide(x, y, down, time) => glide(x, y, down, time, window, cx).await,
                Step::Record(name, rate) => {
                    if let Some(recording) = recording.take() {
                        recording.finish().await;
                    }
                    recording = Some(record(name, rate, dir.clone(), window, cx));
                }
                Step::Stop => {
                    if let Some(recording) = recording.take() {
                        recording.finish().await;
                    }
                }
                Step::Quit => {
                    if let Some(recording) = recording.take() {
                        recording.finish().await;
                    }
                    cx.update(|cx| cx.quit());
                    break;
                }
                step => {
                    let done =
                        cx.update_window(window, |_, window, cx| perform(&step, &dir, window, cx));
                    if let Ok(Err(error)) = &done {
                        eprintln!("capture: {error}");
                    }
                    if done.is_err() {
                        break;
                    }
                    // Let what the step set off (a query, a decode) get going.
                    cx.background_executor()
                        .timer(Duration::from_millis(30))
                        .await;
                }
            }
        }
    })
    .detach();
}

/// Prints a moment as microseconds since 1970, for whoever started the
/// process to set against its own clock.
pub(crate) fn at(label: &str) {
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_micros());
    eprintln!("capture: at {label} {micros}");
}

/// Waits until the window has what `ready` names, draws it and prints
/// when that was.
async fn until(
    ready: Ready,
    limit: Duration,
    window: AnyWindowHandle,
    view: &Entity<AppView>,
    cx: &mut AsyncApp,
) {
    let start = Instant::now();
    loop {
        let (chats, messages, _) = cx.update(|cx| view.read(cx).capture_state(cx));
        let there = match ready {
            Ready::Chats => chats > 0,
            Ready::Messages => messages > 0,
        };
        if there {
            break;
        }
        if start.elapsed() > limit {
            eprintln!("capture: gave up waiting for {ready:?}");
            return;
        }
        cx.background_executor()
            .timer(Duration::from_millis(1))
            .await;
    }
    cx.update_window(window, |_, window, cx| window.draw(cx).clear(cx))
        .ok();
    at(match ready {
        Ready::Chats => "chats",
        Ready::Messages => "messages",
    });
}

/// Does one step and draws the frame that shows it, and prints how long
/// the two took together, in microseconds: `capture: time <label> <µs>
/// <list> <thread> <open>`. That is from the input event to the frame
/// laid out and painted into a scene, the application's own work; the GPU
/// drawing the scene and the display showing it come after and are not in
/// it. The rest is what the window holds then: the rows of the chat list,
/// the rows of the open conversation and which chat that is.
fn timed(
    label: &str,
    step: &Step,
    dir: &Path,
    window: AnyWindowHandle,
    view: &Entity<AppView>,
    cx: &mut AsyncApp,
) -> Result<(), String> {
    let gone = |_| "the window is gone".to_owned();
    let start = Instant::now();
    // Two updates: what the step emits (the search field's change, say) is
    // handled when the first one ends.
    cx.update_window(window, |_, window, cx| perform(step, dir, window, cx))
        .map_err(gone)??;
    cx.update_window(window, |_, window, cx| window.draw(cx).clear(cx))
        .map_err(gone)?;
    let took = start.elapsed();
    let (list, thread, open) = cx.update(|cx| view.read(cx).capture_state(cx));
    eprintln!(
        "capture: time {label} {} {list} {thread} {}",
        took.as_micros(),
        open.as_deref().unwrap_or("-"),
    );
    Ok(())
}

/// Scrolls by `down` points over `time`, sixty steps a second, drawing a
/// frame after each, and prints how long each step and its frame took:
/// `capture: glide <µs> <µs> ...`.
async fn glide(
    x: f32,
    y: f32,
    down: f32,
    time: Duration,
    window: AnyWindowHandle,
    cx: &mut AsyncApp,
) {
    let interval = Duration::from_micros(16_667);
    let steps = (time.as_micros() / interval.as_micros()).max(1) as u32;
    let step = Step::Scroll(x, y, down / steps as f32);
    let start = Instant::now();
    let mut frames = Vec::new();
    for index in 1..=steps {
        let frame = Instant::now();
        let scrolled = cx.update_window(window, |_, window, cx| {
            perform(&step, Path::new(""), window, cx)
        });
        let drawn = cx.update_window(window, |_, window, cx| window.draw(cx).clear(cx));
        if scrolled.is_err() || drawn.is_err() {
            return;
        }
        frames.push(frame.elapsed().as_micros().to_string());
        if let Some(rest) = (interval * index).checked_sub(start.elapsed()) {
            cx.background_executor().timer(rest).await;
        }
    }
    eprintln!("capture: glide {}", frames.join(" "));
}

/// Frames being saved at a fixed rate.
struct Recording {
    stop: Rc<Cell<bool>>,
    task: Task<()>,
}

impl Recording {
    /// Ends the recording and waits for the last frame to be on disk.
    async fn finish(self) {
        self.stop.set(true);
        self.task.await;
    }
}

/// Starts saving the window `rate` times a second, until told to stop:
/// `<name>.rgb` holds the frames one after the other, three bytes a pixel,
/// and `<name>.txt` their size and then when each was taken, in
/// microseconds since the first. On a display that draws two pixels to
/// the point the frames are halved, back to points. Writing happens on
/// another thread; a frame it has no room for is dropped and said so.
fn record(
    name: String,
    rate: u32,
    dir: PathBuf,
    window: AnyWindowHandle,
    cx: &mut AsyncApp,
) -> Recording {
    let stop = Rc::new(Cell::new(false));
    let stopped = stop.clone();
    let task = cx.spawn(async move |cx| {
        let (frames, incoming) = std::sync::mpsc::sync_channel::<(Duration, image::RgbaImage)>(8);
        let label = name.clone();
        // Two pixels to the point: the frames are saved in points.
        let halve = cx
            .update_window(window, |_, window, _| window.scale_factor() >= 2.)
            .unwrap_or(false);
        let writer = std::thread::spawn(move || write_frames(&dir, &name, halve, incoming));
        at(&format!("record-{label}"));
        let interval = Duration::from_secs_f64(1. / f64::from(rate));
        let start = Instant::now();
        while !stopped.get() {
            let taken = start.elapsed();
            match cx.update_window(window, |_, window, cx| {
                window.draw(cx).clear(cx);
                window.render_to_image()
            }) {
                Ok(Ok(frame)) => {
                    if frames.try_send((taken, frame)).is_err() {
                        eprintln!("capture: dropped a frame of {label}");
                    }
                }
                Ok(Err(error)) => eprintln!("capture: {label}: {error}"),
                Err(_) => break,
            }
            // The next tick of the rate that is still ahead.
            let tick = (start.elapsed().as_secs_f64() / interval.as_secs_f64()) as u32 + 1;
            if let Some(rest) = (interval * tick).checked_sub(start.elapsed()) {
                cx.background_executor().timer(rest).await;
            }
        }
        drop(frames);
        match writer.join() {
            Ok(Ok(count)) => eprintln!("capture: recorded {label} {count}"),
            Ok(Err(error)) => eprintln!("capture: {label}: {error}"),
            Err(_) => eprintln!("capture: {label}: the writer stopped"),
        }
    });
    Recording { stop, task }
}

fn write_frames(
    dir: &Path,
    name: &str,
    halve: bool,
    frames: std::sync::mpsc::Receiver<(Duration, image::RgbaImage)>,
) -> std::io::Result<usize> {
    std::fs::create_dir_all(dir)?;
    let mut pixels =
        std::io::BufWriter::new(std::fs::File::create(dir.join(format!("{name}.rgb")))?);
    let mut times = String::new();
    let mut count = 0;
    for (taken, frame) in frames {
        let (width, height) = frame.dimensions();
        // Back to points, each the mean of four pixels.
        let halve = halve && width % 2 == 0 && height % 2 == 0;
        let (out_width, out_height) = if halve {
            (width / 2, height / 2)
        } else {
            (width, height)
        };
        if count == 0 {
            times.push_str(&format!("{out_width} {out_height}\n"));
        }
        let raw = frame.as_raw();
        let stride = width as usize * 4;
        let mut out = Vec::with_capacity(out_width as usize * out_height as usize * 3);
        for y in 0..out_height as usize {
            for x in 0..out_width as usize {
                for channel in 0..3 {
                    out.push(if halve {
                        let at = y * 2 * stride + x * 8 + channel;
                        ((u16::from(raw[at])
                            + u16::from(raw[at + 4])
                            + u16::from(raw[at + stride])
                            + u16::from(raw[at + stride + 4])
                            + 2)
                            / 4) as u8
                    } else {
                        raw[y * stride + x * 4 + channel]
                    });
                }
            }
        }
        pixels.write_all(&out)?;
        times.push_str(&format!("{}\n", taken.as_micros()));
        count += 1;
    }
    pixels.flush()?;
    std::fs::write(dir.join(format!("{name}.txt")), times)?;
    Ok(count)
}

fn perform(step: &Step, dir: &Path, window: &mut Window, cx: &mut App) -> Result<(), String> {
    let none = Modifiers::default();
    match step {
        Step::Size(width, height) => window.resize(size(px(*width), px(*height))),
        // These take time: `start` runs them itself.
        Step::Wait(_)
        | Step::Until(..)
        | Step::Time(..)
        | Step::Glide(..)
        | Step::Record(..)
        | Step::Stop => {}
        Step::Key(keystroke) => {
            let keystroke =
                Keystroke::parse(keystroke).map_err(|error| format!("`{keystroke}`: {error}"))?;
            window.dispatch_keystroke(keystroke, cx);
        }
        Step::Type(text) => {
            for character in text.chars() {
                let key = if character == ' ' {
                    "space".to_owned()
                } else {
                    character.to_string()
                };
                let mut keystroke =
                    Keystroke::parse(&key).map_err(|error| format!("`{key}`: {error}"))?;
                keystroke.key_char = Some(character.to_string());
                window.dispatch_keystroke(keystroke, cx);
            }
        }
        Step::Move(x, y) => {
            window.dispatch_event(
                PlatformInput::MouseMove(MouseMoveEvent {
                    position: point(px(*x), px(*y)),
                    pressed_button: None,
                    modifiers: none,
                }),
                cx,
            );
        }
        Step::Click(x, y) => {
            let position = point(px(*x), px(*y));
            window.dispatch_event(
                PlatformInput::MouseMove(MouseMoveEvent {
                    position,
                    pressed_button: None,
                    modifiers: none,
                }),
                cx,
            );
            window.dispatch_event(
                PlatformInput::MouseDown(MouseDownEvent {
                    button: MouseButton::Left,
                    position,
                    modifiers: none,
                    click_count: 1,
                    first_mouse: false,
                }),
                cx,
            );
            window.dispatch_event(
                PlatformInput::MouseUp(MouseUpEvent {
                    button: MouseButton::Left,
                    position,
                    modifiers: none,
                    click_count: 1,
                }),
                cx,
            );
        }
        Step::Scroll(x, y, down) => {
            window.dispatch_event(
                PlatformInput::ScrollWheel(ScrollWheelEvent {
                    position: point(px(*x), px(*y)),
                    delta: ScrollDelta::Pixels(point(px(0.), px(-*down))),
                    modifiers: none,
                    touch_phase: TouchPhase::Moved,
                }),
                cx,
            );
        }
        Step::Shot(name) => {
            // The frame as it stands now, whether or not the system has
            // asked for one since the last step.
            window.refresh();
            window.draw(cx).clear(cx);
            let image = window
                .render_to_image()
                .map_err(|error| format!("{name}: {error}"))?;
            std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
            let path = dir.join(format!("{name}.png"));
            image
                .save(&path)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            eprintln!("capture: {}", path.display());
        }
        Step::Quit => cx.quit(),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_script_is_read_line_by_line() {
        let steps = parse(
            "# the chats\n\
             size 1240 800\n\
             wait 500   # let the list load\n\
             key cmd-k\n\
             type Lisbon trip\n\
             move 10 20\n\
             click 300 200.5\n\
             scroll 700 400 -240\n\
             \n\
             shot chats-light\n\
             until chats 10000\n\
             time open click 200 300\n\
             glide 800 400 -900 1500\n\
             record clip 30\n\
             stop\n\
             quit\n",
        )
        .unwrap();
        assert_eq!(
            steps,
            vec![
                Step::Size(1240., 800.),
                Step::Wait(Duration::from_millis(500)),
                Step::Key("cmd-k".into()),
                Step::Type("Lisbon trip".into()),
                Step::Move(10., 20.),
                Step::Click(300., 200.5),
                Step::Scroll(700., 400., -240.),
                Step::Shot("chats-light".into()),
                Step::Until(Ready::Chats, Duration::from_millis(10_000)),
                Step::Time("open".into(), Box::new(Step::Click(200., 300.))),
                Step::Glide(800., 400., -900., Duration::from_millis(1500)),
                Step::Record("clip".into(), 30),
                Step::Stop,
                Step::Quit,
            ]
        );
    }

    #[test]
    fn a_mistake_names_its_line() {
        assert_eq!(
            parse("wait 10\nclick 4\n").unwrap_err(),
            "line 2: `click` takes 2 numbers"
        );
        assert_eq!(
            parse("shoot now\n").unwrap_err(),
            "line 1: unknown step `shoot`"
        );
        assert_eq!(parse("shot\n").unwrap_err(), "line 1: `shot` needs a value");
        assert_eq!(
            parse("wait soon\n").unwrap_err(),
            "line 1: `wait` takes numbers"
        );
        assert_eq!(
            parse("wait 10\ntime open click 4\n").unwrap_err(),
            "line 2: `click` takes 2 numbers"
        );
        assert_eq!(
            parse("time open quit\n").unwrap_err(),
            "line 1: `time` takes a name and a key, type, click or scroll step"
        );
        assert_eq!(
            parse("until soon 5\n").unwrap_err(),
            "line 1: `until` takes chats or messages"
        );
        assert_eq!(
            parse("record clip 0\n").unwrap_err(),
            "line 1: `record` takes a name and a rate of 1 to 60"
        );
    }
}

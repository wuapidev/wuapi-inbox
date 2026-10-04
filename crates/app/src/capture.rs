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

use gpui_kit::{
    point, px, size, AnyWindowHandle, App, AppContext as _, Keystroke, Modifiers, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PlatformInput, ScrollDelta, ScrollWheelEvent,
    TouchPhase, Window,
};
use std::path::PathBuf;
use std::time::Duration;

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
    Quit,
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
pub(crate) fn start(window: AnyWindowHandle, cx: &mut App) {
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
    cx.spawn(async move |cx| {
        for step in steps {
            if let Step::Wait(time) = step {
                cx.background_executor().timer(time).await;
                continue;
            }
            let quit = step == Step::Quit;
            let done = cx.update_window(window, |_, window, cx| perform(&step, &dir, window, cx));
            if let Ok(Err(error)) = &done {
                eprintln!("capture: {error}");
            }
            if quit || done.is_err() {
                break;
            }
            // Let what the step set off (a query, a decode) get going.
            cx.background_executor()
                .timer(Duration::from_millis(30))
                .await;
        }
    })
    .detach();
}

fn perform(
    step: &Step,
    dir: &std::path::Path,
    window: &mut Window,
    cx: &mut App,
) -> Result<(), String> {
    let none = Modifiers::default();
    match step {
        Step::Size(width, height) => window.resize(size(px(*width), px(*height))),
        Step::Wait(_) => {}
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
    }
}

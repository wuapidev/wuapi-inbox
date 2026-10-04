//! Recording a voice note in the real window, with a microphone that
//! hears what the test says and an output that makes no sound. Nothing
//! here opens a device.

use super::*;
use crate::audio::tests::{FakeOutput, Tape};
use crate::record::tests::{FakeInput, Mic};
use crate::record::Format;
use crate::ui::voice_record::{Phase, NOT_AVAILABLE};
use client_provider::MediaKind;
use std::cell::RefCell;
use std::rc::Rc;

/// A chat where files can be sent, a fake microphone and a fake output.
fn open_for_recording(cx: &mut TestAppContext) -> (Harness, Rc<RefCell<Mic>>) {
    cx.update(|cx| prepare(cx, None));
    let harness = open_prepared(cx, ShellOptions::default());
    harness.settle(cx);
    let first = cx.update(|cx| chat_rows(harness.shell.read(cx))[0].0.clone());
    let mic = Rc::new(RefCell::new(Mic::default()));
    let input = FakeInput(
        mic.clone(),
        Format {
            rate: 48_000,
            channels: 1,
        },
    );
    let tape = Rc::new(RefCell::new(Tape::default()));
    cx.update(|cx| {
        harness.shell.update(cx, |shell, cx| {
            shell.set_audio_input(Box::new(input));
            shell.set_audio_output(Box::new(FakeOutput(tape)));
            shell.open_chat(client_provider::ChatId::new(first), None, cx)
        })
    });
    cx.run_until_parked();
    focus_composer(&harness, cx);
    (harness, mic)
}

fn phase(harness: &Harness, cx: &mut TestAppContext) -> Phase {
    cx.update(|cx| harness.shell.read(cx).voice.phase.clone())
}

fn error(harness: &Harness, cx: &mut TestAppContext) -> Option<String> {
    cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .voice
            .error
            .as_ref()
            .map(|error| error.to_string())
    })
}

/// The bar takes in what the microphone heard: one of its ticks passes.
fn tick(cx: &mut TestAppContext) {
    cx.executor().advance_clock(Duration::from_millis(60));
    cx.run_until_parked();
}

fn length(harness: &Harness, cx: &mut TestAppContext) -> f32 {
    cx.update(|cx| harness.shell.read(cx).voice.length().as_secs_f32())
}

fn composer_text(harness: &Harness, cx: &mut TestAppContext) -> String {
    cx.update(|cx| harness.shell.read(cx).composer.read(cx).value().to_string())
}

fn voice_notes(harness: &Harness, cx: &mut TestAppContext) -> usize {
    newest_media(harness, cx)
        .iter()
        .filter(|(kind, _, _)| *kind == MediaKind::Voice)
        .count()
}

#[gpui_kit::test]
fn a_voice_note_is_recorded_listened_to_and_sent_through_the_outbox(cx: &mut TestAppContext) {
    let (harness, mic) = open_for_recording(cx);
    // Nothing is open, and nothing is heard, before the click.
    assert_eq!(mic.borrow().opened.len(), 0);
    assert!(shows(harness.window, "voice", cx) && shows(harness.window, "emoji", cx));
    assert!(!shows(harness.window, "record-bar", cx));

    click(harness.window, "voice", cx);
    assert_eq!(phase(&harness, cx), Phase::Recording);
    assert_eq!(mic.borrow().opened.len(), 1);
    // The bar is in the composer's place: the composer's buttons are not
    // there, and the bar's are, inside it.
    let composer = bounds(harness.window, "composer", cx);
    let bar = bounds(harness.window, "record-bar", cx);
    assert!(within(bar, composer));
    assert!(!shows(harness.window, "emoji", cx) && !shows(harness.window, "attach", cx));
    for part in [
        "record-discard",
        "record-light",
        "record-state",
        "record-time",
        "record-levels",
        "record-pause",
        "record-stop",
        "record-send",
    ] {
        assert!(within(bounds(harness.window, part, cx), bar), "{part}");
    }

    // It hears two seconds: the time and the meter follow.
    mic.borrow_mut().hear(300., 2.);
    tick(cx);
    assert!((length(&harness, cx) - 2.).abs() < 0.05);
    cx.update(|cx| {
        let levels = harness.shell.read(cx).voice.recorder.levels().to_vec();
        assert!(levels.len() >= 38 && levels.iter().skip(1).all(|level| *level > 60));
    });
    // Space pauses: what is said meanwhile is not in the note.
    press(harness.window, "space", cx);
    mic.borrow_mut().hear(300., 5.);
    tick(cx);
    assert!((length(&harness, cx) - 2.).abs() < 0.05);
    press(harness.window, "space", cx);
    mic.borrow_mut().hear(300., 1.);
    tick(cx);
    assert!((length(&harness, cx) - 3.).abs() < 0.05);

    // The shortcut stops it: the microphone is closed at once, and the
    // note can be listened to.
    press(harness.window, "ctrl-shift-r", cx);
    assert_eq!(phase(&harness, cx), Phase::Preview);
    assert_eq!(mic.borrow().closes, 1);
    assert!(mic.borrow().sink.is_none(), "the microphone is closed");
    assert!(shows(harness.window, "record-play", cx) && !shows(harness.window, "record-pause", cx));
    press(harness.window, "space", cx);
    cx.update(|cx| assert!(harness.shell.read(cx).audio.player.is_playing()));
    press(harness.window, "space", cx);
    cx.update(|cx| assert!(!harness.shell.read(cx).audio.player.is_playing()));

    // Enter sends: a pending voice bubble at once, with its length and
    // its shape, and the composer is back with the keyboard.
    let (uploads, sends) = (harness.mock.upload_calls(), harness.mock.send_calls());
    press(harness.window, "enter", cx);
    until(&harness, cx, |shell| shell.voice.phase == Phase::Idle);
    assert_eq!(voice_notes(&harness, cx), 1);
    assert!(!shows(harness.window, "record-bar", cx) && shows(harness.window, "voice", cx));
    let shape = cx.update(|cx| {
        let shell = harness.shell.read(cx);
        let source = shell
            .open
            .as_ref()
            .unwrap()
            .rows
            .iter()
            .rev()
            .find_map(|row| match row {
                Row::Message(row) => match &row.stored.message.content {
                    MessageContent::Media(media) if media.kind == MediaKind::Voice => {
                        media.source.clone()
                    }
                    _ => None,
                },
                _ => None,
            })
            .expect("the voice bubble has its file");
        assert!(source.as_str().starts_with("local:"));
        shell.media.shape(source.as_str())
    });
    let shape = shape.expect("its shape is known before anything is uploaded");
    assert!((shape.duration.as_secs_f32() - 3.).abs() < 0.05);
    assert!(shape.bars.iter().any(|bar| *bar > 100));
    type_text(harness.window, "sent", cx);
    assert_eq!(composer_text(&harness, cx), "sent");

    // The outbox uploads the file and sends the message, as a voice note
    // in Ogg/Opus that the player's own decoder reads.
    let pass = harness
        .runtime
        .block_on(harness.engine.flush_outbox())
        .unwrap();
    assert_eq!(pass.sent, 1);
    assert_eq!(harness.mock.upload_calls(), uploads + 1);
    assert_eq!(harness.mock.send_calls(), sends + 1);
    let (bytes, mime) = harness.mock.uploaded().pop().expect("an upload");
    assert_eq!(mime, crate::record::MIME);
    let clip = crate::audio::decode(bytes, Some(&mime)).unwrap();
    assert!((clip.duration().as_secs_f32() - 3.).abs() < 0.05);
    // The microphone was opened once and closed once, all along.
    assert_eq!((mic.borrow().opened.len(), mic.borrow().closes), (1, 1));
}

#[gpui_kit::test]
fn escape_throws_a_recording_away_and_asks_first_when_it_is_worth_something(
    cx: &mut TestAppContext,
) {
    let (harness, mic) = open_for_recording(cx);
    // A second of it: Escape, and it is gone.
    press(harness.window, "ctrl-shift-r", cx);
    assert_eq!(phase(&harness, cx), Phase::Recording);
    mic.borrow_mut().hear(300., 1.);
    tick(cx);
    press(harness.window, "escape", cx);
    assert_eq!(phase(&harness, cx), Phase::Idle);
    assert_eq!(mic.borrow().closes, 1);
    assert_eq!(length(&harness, cx), 0.);
    assert!(!shows(harness.window, "record-bar", cx));
    // The keyboard is the composer's, and the chat is still open.
    type_text(harness.window, "a", cx);
    assert_eq!(composer_text(&harness, cx), "a");
    press(harness.window, "backspace", cx);

    // Ten seconds of it: Escape asks; anything else keeps it.
    click(harness.window, "voice", cx);
    mic.borrow_mut().hear(300., 10.);
    tick(cx);
    press(harness.window, "escape", cx);
    assert_eq!(phase(&harness, cx), Phase::Recording);
    assert!(shows(harness.window, "record-ask", cx));
    assert!(mic.borrow().sink.is_some(), "still recording");
    press(harness.window, "space", cx);
    assert!(!shows(harness.window, "record-ask", cx));
    press(harness.window, "space", cx);
    // Escape twice throws it away; nothing was queued by any of it.
    press(harness.window, "escape", cx);
    press(harness.window, "escape", cx);
    assert_eq!(phase(&harness, cx), Phase::Idle);
    assert_eq!(mic.borrow().closes, 2);
    assert_eq!(voice_notes(&harness, cx), 0);
    assert!(harness.engine.store().outbox_pending().unwrap().is_empty());
    cx.update(|cx| assert!(harness.shell.read(cx).open.is_some()));

    // The bin asks the same way, and leaving the chat does not ask: a
    // recording does not follow to another conversation.
    click(harness.window, "voice", cx);
    mic.borrow_mut().hear(300., 10.);
    tick(cx);
    click(harness.window, "record-discard", cx);
    assert!(shows(harness.window, "record-ask", cx));
    press(harness.window, "ctrl-w", cx);
    assert_eq!(phase(&harness, cx), Phase::Idle);
    assert_eq!(mic.borrow().closes, 3);
    assert!(mic.borrow().sink.is_none());
}

#[gpui_kit::test]
fn what_cannot_be_recorded_or_sent_is_said_in_the_bar(cx: &mut TestAppContext) {
    let (harness, mic) = open_for_recording(cx);
    // No microphone: said where the bar would be, and the composer comes
    // back when it is dismissed.
    mic.borrow_mut().broken = Some("No microphone was found.".into());
    click(harness.window, "voice", cx);
    assert_eq!(phase(&harness, cx), Phase::Idle);
    assert_eq!(
        error(&harness, cx).as_deref(),
        Some("No microphone was found.")
    );
    let said = bounds(harness.window, "record-error", cx);
    assert!(within(said, bounds(harness.window, "composer", cx)));
    click(harness.window, "record-dismiss", cx);
    assert!(!shows(harness.window, "record-bar", cx) && shows(harness.window, "voice", cx));
    mic.borrow_mut().broken = None;

    // Too short: nothing is sent, and it says so.
    click(harness.window, "voice", cx);
    mic.borrow_mut().hear(300., 0.4);
    tick(cx);
    press(harness.window, "enter", cx);
    assert_eq!(phase(&harness, cx), Phase::Idle);
    assert!(error(&harness, cx).unwrap().contains("Too short"));
    assert_eq!(voice_notes(&harness, cx), 0);
    assert!(mic.borrow().sink.is_none());
    click(harness.window, "record-dismiss", cx);

    // The longest a note can be: it stops by itself, and can be sent.
    click(harness.window, "voice", cx);
    let minute = vec![0f32; 48_000 * 60];
    for _ in 0..16 {
        mic.borrow()
            .sink
            .as_ref()
            .unwrap()
            .send(minute.clone())
            .unwrap();
    }
    tick(cx);
    assert_eq!(phase(&harness, cx), Phase::Preview);
    assert!(mic.borrow().sink.is_none(), "the microphone is closed");
    assert_eq!(length(&harness, cx), crate::record::LONGEST.as_secs_f32());
    assert!(error(&harness, cx).unwrap().contains("15 minutes"));
    click(harness.window, "record-discard", cx);
    click(harness.window, "record-discard", cx);
    assert_eq!(phase(&harness, cx), Phase::Idle);
}

#[gpui_kit::test]
fn where_files_cannot_be_sent_nothing_is_recorded(cx: &mut TestAppContext) {
    let (harness, mic) = open_for_recording(cx);
    harness.mock.set_uploads_available(false);
    harness.engine.check_uploads();
    harness.settle(cx);
    cx.update(|cx| assert!(!harness.shell.read(cx).can_record()));
    // The button is there, faint, and does nothing; the shortcut says why.
    click(harness.window, "voice", cx);
    assert_eq!(phase(&harness, cx), Phase::Idle);
    press(harness.window, "ctrl-shift-r", cx);
    assert_eq!(phase(&harness, cx), Phase::Idle);
    assert_eq!(
        mic.borrow().opened.len(),
        0,
        "the microphone was never opened"
    );
    let problem = cx.update(|cx| harness.shell.read(cx).problem.clone().unwrap());
    assert!(problem.contains(NOT_AVAILABLE), "{problem}");
}

#[gpui_kit::test]
fn the_recording_bar_fits_at_every_interface_size_and_gives_the_composer_back(
    cx: &mut TestAppContext,
) {
    let (harness, mic) = open_for_recording(cx);
    for step in theme::SCALE_STEPS {
        cx.update(|cx| settings::update(cx, |settings| settings.interface_scale = step));
        cx.run_until_parked();
        focus_composer(&harness, cx);
        let field = bounds(harness.window, "composer", cx);
        click(harness.window, "voice", cx);
        mic.borrow_mut().hear(300., 1.5);
        tick(cx);
        // The bar is as tall as the composer was: the thread does not jump.
        let composer = bounds(harness.window, "composer", cx);
        assert!(
            (composer.size.height - field.size.height).abs() <= px(1.),
            "{step}%: {field:?} became {composer:?}"
        );
        let bar = bounds(harness.window, "record-bar", cx);
        let mut right = bar.left();
        for part in [
            "record-discard",
            "record-light",
            "record-state",
            "record-time",
            "record-levels",
            "record-pause",
            "record-stop",
            "record-send",
        ] {
            let part_bounds = bounds(harness.window, part, cx);
            assert!(within(part_bounds, bar), "{step}%: {part}");
            assert!(
                part_bounds.left() >= right - px(0.5),
                "{step}%: {part} in a row"
            );
            right = part_bounds.right();
        }
        // The newest message is still whole above it.
        let thread = bounds(harness.window, "thread", cx);
        assert!(thread.bottom() <= composer.top() + px(0.5));
        // Stopped: the same bar, with play in pause's place.
        click(harness.window, "record-stop", cx);
        assert_eq!(phase(&harness, cx), Phase::Preview);
        assert!(within(bounds(harness.window, "record-play", cx), bar));
        // Thrown away (it is short): the composer, its emoji button and
        // the "@" list are back as they were.
        press(harness.window, "escape", cx);
        assert_eq!(phase(&harness, cx), Phase::Idle);
        assert_eq!(bounds(harness.window, "composer", cx), field, "{step}%");
        assert!(shows(harness.window, "emoji", cx) && shows(harness.window, "attach", cx));
    }
    // With something typed there is no microphone: it is the send button.
    type_text(harness.window, "hello", cx);
    assert!(!shows(harness.window, "voice", cx) && shows(harness.window, "send", cx));
    // The shortcut still records; the text waits, untouched.
    press(harness.window, "ctrl-shift-r", cx);
    assert_eq!(phase(&harness, cx), Phase::Recording);
    press(harness.window, "escape", cx);
    assert_eq!(composer_text(&harness, cx), "hello");
    assert_eq!(mic.borrow().opened.len(), mic.borrow().closes);
}

#[gpui_kit::test]
fn the_devices_are_chosen_in_the_settings_and_the_microphone_can_be_tested(
    cx: &mut TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join(settings::FILE_NAME);
    cx.update(|cx| prepare(cx, Some(file.clone())));
    let harness = open_prepared(cx, ShellOptions::default());
    harness.settle(cx);
    let mic = Rc::new(RefCell::new(Mic::default()));
    mic.borrow_mut().names = vec!["Built-in microphone".into(), "USB microphone".into()];
    let tape = Rc::new(RefCell::new(Tape::default()));
    tape.borrow_mut().names = vec!["Speakers".into(), "Headphones".into()];
    let input = FakeInput(
        mic.clone(),
        Format {
            rate: 44_100,
            channels: 2,
        },
    );
    let first = cx.update(|cx| chat_rows(harness.shell.read(cx))[0].0.clone());
    cx.update(|cx| {
        harness.shell.update(cx, |shell, cx| {
            shell.set_audio_input(Box::new(input));
            shell.set_audio_output(Box::new(FakeOutput(tape.clone())));
            shell.open_chat(client_provider::ChatId::new(first), None, cx)
        })
    });
    cx.run_until_parked();

    press(harness.window, "ctrl-,", cx);
    click(harness.window, "settings-audio", cx);
    let panel = bounds(harness.window, "settings-panel", cx);
    for part in ["audio-input", "audio-output", "audio-test", "audio-level"] {
        let part_bounds = bounds(harness.window, part, cx);
        assert!(
            within(part_bounds, panel),
            "{part}: {part_bounds:?} in {panel:?}"
        );
    }
    let devices = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let shell = harness.shell.read(cx);
            (
                shell.voice.device.clone(),
                shell.audio.output_device.clone(),
            )
        })
    };
    assert_eq!(
        devices(cx),
        (None, None),
        "the system's, until one is chosen"
    );

    // The steppers walk the system's default and every device, round.
    click(harness.window, "audio-input-next", cx);
    click(harness.window, "audio-input-next", cx);
    assert_eq!(devices(cx).0.as_deref(), Some("USB microphone"));
    click(harness.window, "audio-output-previous", cx);
    assert_eq!(devices(cx).1.as_deref(), Some("Headphones"));
    assert_eq!(tape.borrow().device.as_deref(), Some("Headphones"));
    // They are kept next to the settings.
    let saved = std::fs::read_to_string(dir.path().join("audio.json")).unwrap();
    assert!(saved.contains("USB microphone") && saved.contains("Headphones"));

    // The test opens the chosen microphone and shows what it hears; it
    // says so while it is open, and nothing is kept.
    assert_eq!(mic.borrow().opened.len(), 0);
    click(harness.window, "audio-test", cx);
    assert_eq!(mic.borrow().opened, [Some("USB microphone".to_owned())]);
    assert!(shows(harness.window, "audio-test-light", cx));
    mic.borrow_mut().hear(300., 0.5);
    tick(cx);
    let (fill, track) = (
        bounds(harness.window, "audio-level-fill", cx),
        bounds(harness.window, "audio-level", cx),
    );
    assert!(
        fill.size.width > track.size.width * 0.3 && fill.size.width <= track.size.width,
        "{fill:?} of {track:?}"
    );
    click(harness.window, "audio-test", cx);
    assert!(mic.borrow().sink.is_none(), "closed");
    assert!(!shows(harness.window, "audio-test-light", cx));
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(shell.voice.recorder.elapsed(), Duration::ZERO);
        assert_eq!(shell.voice.phase, Phase::Idle);
    });
    // Left open, it closes with the settings.
    click(harness.window, "audio-test", cx);
    assert!(mic.borrow().sink.is_some());
    press(harness.window, "escape", cx);
    assert!(mic.borrow().sink.is_none());
    // And by itself after ten seconds.
    press(harness.window, "ctrl-,", cx);
    click(harness.window, "settings-audio", cx);
    click(harness.window, "audio-test", cx);
    for _ in 0..210 {
        tick(cx);
    }
    assert!(mic.borrow().sink.is_none());
    press(harness.window, "escape", cx);

    // A recording uses the microphone that was chosen.
    focus_composer(&harness, cx);
    press(harness.window, "ctrl-shift-r", cx);
    assert_eq!(
        mic.borrow().opened.last().unwrap().as_deref(),
        Some("USB microphone")
    );
    press(harness.window, "escape", cx);

    // No microphone: the test says so, in the settings.
    mic.borrow_mut().broken = Some("No microphone was found.".into());
    press(harness.window, "ctrl-,", cx);
    click(harness.window, "settings-audio", cx);
    click(harness.window, "audio-test", cx);
    assert!(shows(harness.window, "audio-error", cx));
}

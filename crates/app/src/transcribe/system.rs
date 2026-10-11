//! The operating system's own recogniser, on this computer.
//!
//! **Windows**: the speech recogniser every Windows 10 and 11 ships
//! (`System.Speech`, SAPI), run on-device: nothing leaves the computer.
//! It is reached through `powershell.exe` (Windows PowerShell 5.1 is always
//! there, and `System.Speech` comes with it), with a small script and a
//! 16 kHz mono WAV made from the voice note. It recognises only the
//! languages whose speech pack is installed in Windows (one, usually); a
//! language that has none is said so, never guessed with another's
//! recogniser.
//!
//! Dictation quality is that of the classic recogniser: fine for the gist
//! of a clear voice note, well below a Whisper model. The server mode is
//! the one for accuracy.
//!
//! **Elsewhere** there is none yet: Linux has no recogniser of its own, and
//! macOS's (`SFSpeechRecognizer`) is not bound.

use super::{Audio, TranscribeError, Transcriber};
use crate::audio::Clip;
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long recognising may take: the recogniser runs several times faster
/// than the audio plays, so this is for a stuck process, not a long note.
const DEADLINE: Duration = Duration::from_secs(120);
/// The rate the recogniser works at: more is only bigger.
const RATE: u32 = 16_000;
/// The script's exit code when Windows has no recogniser for the language.
const NO_RECOGNISER: i32 = 3;

/// The script. ASCII only: Windows PowerShell reads a file without a BOM as
/// the system's code page.
const SCRIPT: &str = r#"param([string]$Wav, [string]$Lang = "")
$ErrorActionPreference = "Stop"
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding $false
try {
    Add-Type -AssemblyName System.Speech
    $all = @([System.Speech.Recognition.SpeechRecognitionEngine]::InstalledRecognizers())
    $info = $null
    if ($Lang -ne "") {
        $info = $all | Where-Object { $_.Culture.TwoLetterISOLanguageName -eq $Lang } | Select-Object -First 1
    } else {
        $ui = [System.Globalization.CultureInfo]::CurrentUICulture.TwoLetterISOLanguageName
        $info = $all | Where-Object { $_.Culture.TwoLetterISOLanguageName -eq $ui } | Select-Object -First 1
        if (-not $info) { $info = $all | Select-Object -First 1 }
    }
    if (-not $info) { [Console]::Error.WriteLine("NO_RECOGNISER"); exit 3 }
    $engine = New-Object System.Speech.Recognition.SpeechRecognitionEngine($info)
    $engine.LoadGrammar((New-Object System.Speech.Recognition.DictationGrammar))
    $engine.SetInputToWaveFile($Wav)
    $words = New-Object System.Collections.Generic.List[string]
    while ($true) {
        $result = $engine.Recognize()
        if ($null -eq $result) { break }
        $words.Add($result.Text)
    }
    [Console]::Out.Write(($words -join " "))
} catch {
    [Console]::Error.WriteLine($_.Exception.Message)
    exit 4
}
"#;

/// Whether this computer has a recogniser the application can use.
pub fn available() -> bool {
    cfg!(windows)
}

/// The recogniser, or why there is none.
pub fn new() -> Result<Arc<dyn Transcriber>, TranscribeError> {
    if !available() {
        return Err(TranscribeError::Unsupported(
            "This system has no speech recognition the application can use. \
             Choose a server in Settings > Audio."
                .into(),
        ));
    }
    Ok(Arc::new(PowerShellSpeech {
        program: "powershell.exe".into(),
    }))
}

/// Windows' recogniser, through PowerShell.
struct PowerShellSpeech {
    program: String,
}

#[async_trait]
impl Transcriber for PowerShellSpeech {
    async fn transcribe(
        &self,
        audio: Audio,
        language: Option<&str>,
    ) -> Result<String, TranscribeError> {
        let program = self.program.clone();
        let language = language.map(str::to_owned);
        // Decoding and the child process are blocking work.
        tokio::task::spawn_blocking(move || {
            let clip =
                crate::audio::decode(audio.bytes, audio.mime.as_deref()).map_err(|reason| {
                    TranscribeError::File(format!("The voice note could not be read: {reason}"))
                })?;
            recognise(&program, &recogniser_wav(&clip), language.as_deref())
        })
        .await
        .map_err(|_| TranscribeError::Unexpected)?
    }
}

/// A 16 kHz mono 16-bit WAV of `clip`: the channels averaged, and each
/// output sample the mean of the input samples it spans (which is also the
/// low-pass a downsampling needs).
pub(super) fn recogniser_wav(clip: &Clip) -> Vec<u8> {
    let channels = clip.channels.max(1);
    let mono: Vec<f32> = clip
        .samples
        .chunks_exact(channels)
        .map(|frame| frame.iter().map(|s| f32::from(*s)).sum::<f32>() / channels as f32)
        .collect();
    let ratio = f64::from(clip.rate.max(1)) / f64::from(RATE);
    let count = (mono.len() as f64 / ratio) as usize;
    let mut pcm = Vec::with_capacity(count * 2);
    for index in 0..count {
        let from = (index as f64 * ratio) as usize;
        let to = (((index + 1) as f64 * ratio).ceil() as usize)
            .clamp(from + 1, mono.len().max(from + 1));
        let span = &mono[from.min(mono.len() - 1)..to.min(mono.len())];
        let mean = span.iter().sum::<f32>() / span.len().max(1) as f32;
        pcm.extend_from_slice(&(mean.round() as i16).to_le_bytes());
    }
    let mut wav = Vec::with_capacity(44 + pcm.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&RATE.to_le_bytes());
    wav.extend_from_slice(&(RATE * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    wav.extend_from_slice(&pcm);
    wav
}

/// A directory of the voice note's files, removed when done with.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Result<Self, TranscribeError> {
        let mut bytes = [0u8; 8];
        getrandom::fill(&mut bytes).map_err(|_| TranscribeError::Unexpected)?;
        let name: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        let dir = std::env::temp_dir().join(format!("wuapi-inbox-speech-{name}"));
        std::fs::create_dir_all(&dir).map_err(|_| TranscribeError::Unexpected)?;
        Ok(Self(dir))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Runs the script on `wav` with `program`, within the deadline.
fn recognise(program: &str, wav: &[u8], language: Option<&str>) -> Result<String, TranscribeError> {
    let scratch = Scratch::new()?;
    let (wav_path, script_path): (PathBuf, PathBuf) =
        (scratch.0.join("note.wav"), scratch.0.join("recognise.ps1"));
    std::fs::write(&wav_path, wav).map_err(|_| TranscribeError::Unexpected)?;
    std::fs::write(&script_path, SCRIPT).map_err(|_| TranscribeError::Unexpected)?;

    let mut command = Command::new(program);
    command
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(&script_path)
        .arg("-Wav")
        .arg(&wav_path)
        .arg("-Lang")
        .arg(language.unwrap_or(""))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        // No console window flashing up.
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut child = command.spawn().map_err(|error| {
        TranscribeError::Unsupported(format!(
            "Windows' speech recogniser could not start: {error}"
        ))
    })?;
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() > DEADLINE => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(TranscribeError::Offline);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(40)),
            Err(_) => return Err(TranscribeError::Unexpected),
        }
    };
    let output = child
        .wait_with_output()
        .map_err(|_| TranscribeError::Unexpected)?;
    match status.code() {
        Some(0) => Ok(String::from_utf8_lossy(&output.stdout)
            .trim_start_matches('\u{feff}')
            .trim()
            .to_owned()),
        Some(NO_RECOGNISER) => Err(TranscribeError::Unsupported(
            "Windows has no speech recogniser for this language. Add the language's speech \
             pack in Windows' Settings > Time & language > Speech, or use a server."
                .into(),
        )),
        _ => {
            let said = String::from_utf8_lossy(&output.stderr);
            let said = said.lines().next().unwrap_or_default().trim();
            Err(TranscribeError::Setup(if said.is_empty() {
                "Windows' speech recogniser failed.".into()
            } else {
                format!("Windows' speech recogniser said: {said}")
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(rate: u32, channels: usize, seconds: f32) -> Clip {
        let frames = (rate as f32 * seconds) as usize;
        let samples = (0..frames * channels)
            .map(|i| ((i as f32 * 0.05).sin() * 8000.) as i16)
            .collect();
        Clip {
            samples,
            channels,
            rate,
        }
    }

    #[test]
    fn a_note_becomes_a_16_khz_mono_wav() {
        for (rate, channels) in [
            (48_000, 2),
            (48_000, 1),
            (16_000, 1),
            (8_000, 1),
            (44_100, 2),
        ] {
            let wav = recogniser_wav(&clip(rate, channels, 1.5));
            assert_eq!(&wav[0..4], b"RIFF");
            assert_eq!(&wav[8..16], b"WAVEfmt ");
            assert_eq!(u16::from_le_bytes([wav[22], wav[23]]), 1, "mono");
            assert_eq!(
                u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]),
                16_000
            );
            let data = u32::from_le_bytes([wav[40], wav[41], wav[42], wav[43]]) as usize;
            assert_eq!(wav.len(), 44 + data);
            assert_eq!(
                u32::from_le_bytes([wav[4], wav[5], wav[6], wav[7]]) as usize,
                wav.len() - 8
            );
            let seconds = data as f32 / 2. / 16_000.;
            assert!(
                (seconds - 1.5).abs() < 0.01,
                "{rate} Hz x{channels}: {seconds}"
            );
        }
    }

    #[test]
    fn an_empty_or_tiny_clip_does_not_panic() {
        for frames in [0usize, 1, 2] {
            let clip = Clip {
                samples: vec![100; frames],
                channels: 1,
                rate: 48_000,
            };
            let wav = recogniser_wav(&clip);
            assert_eq!(&wav[0..4], b"RIFF");
        }
    }

    /// A stand-in for powershell.exe: a script that answers by its exit code.
    #[cfg(unix)]
    fn fake(dir: &Path, body: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("fake-powershell");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[cfg(unix)]
    #[test]
    fn the_script_gets_the_file_and_its_answer_comes_back() {
        let dir = tempfile::tempdir().unwrap();
        // Checks the arguments it is called with, as the script would read them.
        let program = fake(
            dir.path(),
            r#"[ "$1" = "-NoProfile" ] && [ "$7" = "-Wav" ] && [ -f "$8" ] && [ "$9" = "-Lang" ] && printf '\357\273\277  hola mundo %s \n' "${10}""#,
        );
        let wav = recogniser_wav(&clip(48_000, 1, 0.5));
        assert_eq!(
            recognise(&program, &wav, Some("es")),
            Ok("hola mundo es".into())
        );
    }

    #[cfg(unix)]
    #[test]
    fn exit_codes_say_what_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let wav = recogniser_wav(&clip(16_000, 1, 0.2));
        let none = fake(dir.path(), "exit 3");
        assert!(matches!(
            recognise(&none, &wav, Some("es")),
            Err(TranscribeError::Unsupported(_))
        ));
        let broken = fake(
            dir.path(),
            "echo 'The language pack is damaged' >&2; exit 4",
        );
        assert_eq!(
            recognise(&broken, &wav, None),
            Err(TranscribeError::Setup(
                "Windows' speech recogniser said: The language pack is damaged".into()
            ))
        );
        assert!(matches!(
            recognise("/nonexistent/powershell", &wav, None),
            Err(TranscribeError::Unsupported(_))
        ));
    }

    #[test]
    fn the_scratch_directory_is_removed() {
        let path = {
            let scratch = Scratch::new().unwrap();
            assert!(scratch.0.is_dir());
            scratch.0.clone()
        };
        assert!(!path.exists());
    }

    /// Makes the WAV the Windows VM is tested with, from a real Ogg/Opus
    /// file: `WUAPI_OGG=in.ogg WUAPI_WAV=out.wav cargo test -- --ignored`.
    #[test]
    #[ignore = "writes a file for a manual test on Windows"]
    fn writes_the_wav_of_an_ogg() {
        let (Ok(input), Ok(output)) = (std::env::var("WUAPI_OGG"), std::env::var("WUAPI_WAV"))
        else {
            return;
        };
        let clip = crate::audio::decode(std::fs::read(input).unwrap(), Some("audio/ogg")).unwrap();
        std::fs::write(output, recogniser_wav(&clip)).unwrap();
    }
}

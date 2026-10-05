//! Command-line options.

use crate::product::PRODUCT_NAME;
use crate::theme::Appearance;
use std::path::PathBuf;

/// Which provider backs the session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderKind {
    /// In-memory demo data. Nothing leaves the machine.
    Mock,
    /// wuapi (https://wuapi.dev).
    Wuapi,
    /// The worked example of `docs/PROVIDERS.md`: says back what it is sent.
    #[cfg(feature = "provider-example")]
    Example,
}

/// What the user asked for.
#[derive(Clone, Debug)]
pub struct Options {
    /// The provider to run on.
    pub provider: ProviderKind,
    /// The theme for this run, when given: it wins over the saved one.
    pub appearance: Option<Appearance>,
    /// Where the database lives. Defaults to the platform's data directory.
    pub data_dir: Option<PathBuf>,
    /// Open the chat at this position at startup (development aid).
    pub open_chat: Option<usize>,
    /// Forget the stored wuapi API key and sign in again.
    pub relogin: bool,
    /// wuapi: neither read nor write the OS keychain. The sign-in screen
    /// shows at every start and the key lasts for the session.
    pub no_keychain: bool,
    /// Show the welcome screen first, whether or not it was seen before.
    pub welcome: bool,
    /// The window's application id, instead of the product's (development
    /// aid: lets a window rule tell one instance from another).
    pub app_id: Option<String>,
    /// Hold every animation at this many seconds into its timeline
    /// (development aid, for looking at one frame).
    pub motion_at: Option<f32>,
    /// wuapi: talk to this API instead of the production one (development
    /// aid: a local mock server, a staging deployment).
    pub api_url: Option<String>,
    /// Print a report instead of opening a window (support aid).
    pub diagnose: Option<Diagnose>,
    /// Take updates from this address instead of the project's releases
    /// (a mirror, or a local directory or server when trying a release).
    pub update_url: Option<String>,
    /// wuapi: how live updates arrive, when given: it wins over the default.
    pub live: Option<provider_wuapi::LiveTransport>,
    /// wuapi: the root of the event stream, instead of the one derived from
    /// the API (development aid: a local mock, a staging deployment).
    pub stream_url: Option<String>,
}

/// What `--diagnose` reports on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Diagnose {
    /// The first page of each number's chats, as the provider answers
    /// them, without names, texts or keys.
    Chats,
    /// How long the last messages sent from here took, from Enter to
    /// each tick, as the local database recorded it. Asks the API nothing.
    Sends,
    /// Each number's stories as the provider lists them: whether they are
    /// available, how many, and a line per story, without names, texts,
    /// captions, URLs or keys. Reads only: nothing is marked as seen.
    Stories,
    /// The event stream: how it answers a connection, what it says
    /// meanwhile (event types and counts only), without keys, ids, cursors
    /// or payloads. Opens one of the organization's stream connections for
    /// the twenty seconds it listens.
    Stream,
}

/// The parsed command line.
pub enum Command {
    /// Start the application.
    Run(Options),
    /// Print the usage text.
    Help,
}

/// The usage text.
pub fn usage() -> String {
    format!(
        "{PRODUCT_NAME}, a native WhatsApp client

Usage: {bin} [OPTIONS]

Options:
  --provider <mock|wuapi>   Backend to use [default: wuapi]
  --theme <light|dark>      Theme for this run [default: the one in Settings]
  --data-dir <PATH>         Where to keep the local database
  --login                   wuapi: sign in again, replacing the stored API key
  --no-keychain             wuapi: do not use the OS keychain; sign in at every start
  --api-url <URL>           wuapi: use this API instead of the production one
  --live <auto|stream|polling>
                            wuapi: how live updates arrive; auto uses the event stream
                            when the API has one and polls when it does not
  --stream-url <URL>        wuapi: read the event stream from this address (https://, or
                            http://localhost for a local server)
  --update-url <URL>        Take updates from this address (https://, or file:// and
                            http://localhost when trying a release)
  --welcome                 Show the welcome screen first, even if it was seen before
  --open-chat <N>           Open the Nth chat of the list at startup
  --motion-at <SECONDS>     Hold the animations at this moment (development aid)
  --app-id <ID>             Application id of the window (development aid)
  -h, --help                Show this help",
        bin = env!("CARGO_PKG_NAME"),
    )
}

/// Parses the arguments after the program name.
pub fn parse(args: impl Iterator<Item = String>) -> Result<Command, String> {
    let mut options = Options {
        provider: ProviderKind::Wuapi,
        appearance: None,
        data_dir: None,
        open_chat: None,
        relogin: false,
        no_keychain: false,
        welcome: false,
        app_id: None,
        motion_at: None,
        api_url: None,
        diagnose: None,
        update_url: None,
        live: None,
        stream_url: None,
    };
    let mut args = args.peekable();
    while let Some(arg) = args.next() {
        // Accept both `--flag value` and `--flag=value`.
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => {
                (flag.to_owned(), Some(value.to_owned()))
            }
            _ => (arg.clone(), None),
        };
        let mut value = |name: &str| {
            inline
                .clone()
                .or_else(|| args.next())
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match flag.as_str() {
            "-h" | "--help" => return Ok(Command::Help),
            "--provider" => {
                options.provider = match value("--provider")?.as_str() {
                    "mock" => ProviderKind::Mock,
                    "wuapi" => ProviderKind::Wuapi,
                    #[cfg(feature = "provider-example")]
                    "example" => ProviderKind::Example,
                    other => return Err(format!("unknown provider `{other}`")),
                }
            }
            "--theme" => {
                options.appearance = Some(match value("--theme")?.as_str() {
                    "light" => Appearance::Light,
                    "dark" => Appearance::Dark,
                    other => return Err(format!("unknown theme `{other}`")),
                })
            }
            "--data-dir" => options.data_dir = Some(PathBuf::from(value("--data-dir")?)),
            "--open-chat" => {
                options.open_chat = Some(
                    value("--open-chat")?
                        .parse()
                        .map_err(|_| "--open-chat needs a number".to_owned())?,
                )
            }
            "--login" => options.relogin = true,
            "--no-keychain" => options.no_keychain = true,
            "--welcome" => options.welcome = true,
            "--app-id" => options.app_id = Some(value("--app-id")?),
            "--motion-at" => {
                options.motion_at = Some(
                    value("--motion-at")?
                        .parse::<f32>()
                        .ok()
                        .filter(|seconds| seconds.is_finite() && *seconds >= 0.)
                        .ok_or_else(|| "--motion-at needs a number of seconds".to_owned())?,
                )
            }
            "--api-url" => {
                let url = value("--api-url")?;
                if !(url.starts_with("https://") || url.starts_with("http://")) {
                    return Err("--api-url needs an http(s) URL".to_owned());
                }
                options.api_url = Some(url);
            }
            "--live" => {
                options.live = Some(match value("--live")?.as_str() {
                    "auto" => provider_wuapi::LiveTransport::Auto,
                    "stream" => provider_wuapi::LiveTransport::Stream,
                    "polling" => provider_wuapi::LiveTransport::Polling,
                    other => return Err(format!("unknown live transport `{other}`")),
                })
            }
            "--stream-url" => {
                let url = value("--stream-url")?;
                provider_wuapi::check_stream_url(&url)
                    .map_err(|why| format!("--stream-url: {why}"))?;
                options.stream_url = Some(url);
            }
            "--update-url" => {
                let url = value("--update-url")?;
                crate::update::check_base_url(&url)
                    .map_err(|why| format!("--update-url: {why}"))?;
                options.update_url = Some(url);
            }
            // Not in the usage text: a support aid.
            "--diagnose" => {
                options.diagnose = Some(match value("--diagnose")?.as_str() {
                    "chats" => Diagnose::Chats,
                    "sends" => Diagnose::Sends,
                    "stories" => Diagnose::Stories,
                    "stream" => Diagnose::Stream,
                    other => return Err(format!("unknown report `{other}`")),
                })
            }
            other => return Err(format!("unknown option `{other}`")),
        }
    }
    if options.diagnose.is_some() && options.provider != ProviderKind::Wuapi {
        return Err("--diagnose needs the wuapi provider".to_owned());
    }
    if options.provider != ProviderKind::Wuapi {
        if options.live.is_some() {
            return Err("--live needs the wuapi provider".to_owned());
        }
        if options.stream_url.is_some() {
            return Err("--stream-url needs the wuapi provider".to_owned());
        }
    }
    if options.live == Some(provider_wuapi::LiveTransport::Polling) && options.stream_url.is_some()
    {
        return Err("--stream-url has no use with --live polling".to_owned());
    }
    Ok(Command::Run(options))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> Options {
        match parse(args.iter().map(|a| a.to_string())) {
            Ok(Command::Run(options)) => options,
            _ => panic!("expected options"),
        }
    }

    #[test]
    fn defaults_to_the_wuapi_provider() {
        let options = run(&[]);
        assert_eq!(options.provider, ProviderKind::Wuapi);
        assert_eq!(options.appearance, None, "the saved theme decides");
        assert!(usage().contains("[default: wuapi]"));
    }

    #[test]
    fn the_demo_data_is_asked_for_by_name() {
        assert_eq!(run(&["--provider", "mock"]).provider, ProviderKind::Mock);
    }

    #[cfg(feature = "provider-example")]
    #[test]
    fn the_worked_example_is_asked_for_by_name() {
        assert_eq!(
            run(&["--provider", "example"]).provider,
            ProviderKind::Example
        );
        // The options of another provider are refused with it too.
        assert!(
            parse(["--provider=example".to_owned(), "--live=auto".to_owned()].into_iter()).is_err()
        );
    }

    #[test]
    fn accepts_both_flag_styles() {
        let options = run(&["--provider", "wuapi", "--theme=dark", "--open-chat", "3"]);
        assert_eq!(options.provider, ProviderKind::Wuapi);
        assert_eq!(options.appearance, Some(Appearance::Dark));
        assert_eq!(options.open_chat, Some(3));
    }

    #[test]
    fn rejects_what_it_does_not_know() {
        assert!(parse(["--provider".to_owned(), "signal".to_owned()].into_iter()).is_err());
        assert!(parse(["--nope".to_owned()].into_iter()).is_err());
        assert!(parse(["--theme".to_owned()].into_iter()).is_err());
        assert!(parse(["--api-url=localhost".to_owned()].into_iter()).is_err());
    }

    #[test]
    fn the_diagnosis_is_asked_for_by_name_and_only_for_wuapi() {
        let options = run(&["--provider", "wuapi", "--diagnose", "chats"]);
        assert_eq!(options.diagnose, Some(Diagnose::Chats));
        assert_eq!(
            run(&["--provider=wuapi", "--diagnose=sends"]).diagnose,
            Some(Diagnose::Sends)
        );
        assert_eq!(
            run(&["--provider", "wuapi", "--diagnose", "stories"]).diagnose,
            Some(Diagnose::Stories)
        );
        assert_eq!(run(&["--provider=wuapi"]).diagnose, None);
        assert_eq!(run(&["--diagnose=chats"]).diagnose, Some(Diagnose::Chats));
        assert!(
            parse(["--provider=mock".to_owned(), "--diagnose=chats".to_owned()].into_iter())
                .is_err()
        );
        assert!(
            parse(["--provider=wuapi".to_owned(), "--diagnose=keys".to_owned()].into_iter())
                .is_err()
        );
        assert!(
            !usage().contains("diagnose"),
            "a support aid, not a feature"
        );
    }

    #[test]
    fn takes_another_update_source() {
        let options = run(&["--update-url", "https://example.com/inbox"]);
        assert_eq!(
            options.update_url.as_deref(),
            Some("https://example.com/inbox")
        );
        assert_eq!(run(&[]).update_url, None);
        // A local server or a directory, to try a release before it is out.
        assert!(run(&["--update-url=http://127.0.0.1:8000"])
            .update_url
            .is_some());
        // Never plain http to anywhere else, and never something else.
        assert!(parse(["--update-url=http://example.com".to_owned()].into_iter()).is_err());
        assert!(parse(["--update-url=example.com".to_owned()].into_iter()).is_err());
        assert!(parse(["--update-url".to_owned()].into_iter()).is_err());
        assert!(usage().contains("--update-url"));
    }

    fn refused(args: &[&str]) -> String {
        match parse(args.iter().map(|a| a.to_string())) {
            Err(why) => why,
            Ok(_) => panic!("expected an error for {args:?}"),
        }
    }

    #[test]
    fn live_flag_parses_three_values() {
        use provider_wuapi::LiveTransport::{Auto, Polling, Stream};
        assert_eq!(run(&["--live", "auto"]).live, Some(Auto));
        assert_eq!(run(&["--live=stream"]).live, Some(Stream));
        assert_eq!(
            run(&["--provider=wuapi", "--live", "polling"]).live,
            Some(Polling)
        );
        assert_eq!(run(&[]).live, None, "the adapter's default decides");
        assert!(usage().contains("--live <auto|stream|polling>"));
    }

    #[test]
    fn unknown_live_transport_errors() {
        assert_eq!(
            refused(&["--live", "websocket"]),
            "unknown live transport `websocket`"
        );
        assert_eq!(refused(&["--live"]), "--live needs a value");
    }

    #[test]
    fn stream_url_checked() {
        assert_eq!(
            run(&["--stream-url", "https://stream.example.com"])
                .stream_url
                .as_deref(),
            Some("https://stream.example.com")
        );
        assert_eq!(
            run(&["--stream-url=http://127.0.0.1:9100"])
                .stream_url
                .as_deref(),
            Some("http://127.0.0.1:9100")
        );
        assert_eq!(run(&[]).stream_url, None);
        // The key travels to it: plain http only on this machine, and no
        // user info.
        assert!(refused(&["--stream-url=http://example.com"]).starts_with("--stream-url: "));
        assert!(refused(&["--stream-url=https://me:pw@example.com"]).starts_with("--stream-url: "));
        assert!(refused(&["--stream-url=example.com"]).starts_with("--stream-url: "));
        assert!(usage().contains("--stream-url <URL>"));
    }

    #[test]
    fn stream_url_with_a_query_or_fragment_is_refused_without_echoing_it() {
        for url in [
            "https://h.example/?token=secret",
            "https://h.example/#secret",
        ] {
            let why = refused(&["--stream-url", url]);
            assert!(why.starts_with("--stream-url: "), "{why}");
            assert!(!why.contains("secret"), "{why}");
        }
    }

    #[test]
    fn stream_url_refused_with_live_polling() {
        let why = refused(&["--live=polling", "--stream-url=https://stream.example.com"]);
        assert_eq!(why, "--stream-url has no use with --live polling");
        // Whatever the order.
        assert_eq!(
            refused(&[
                "--stream-url=https://stream.example.com",
                "--live",
                "polling"
            ]),
            why
        );
        // With the other two it is fine.
        assert!(
            run(&["--live=stream", "--stream-url=https://stream.example.com"])
                .stream_url
                .is_some()
        );
    }

    #[test]
    fn live_flags_need_wuapi_provider() {
        assert_eq!(
            refused(&["--provider=mock", "--live=auto"]),
            "--live needs the wuapi provider"
        );
        assert_eq!(
            refused(&["--provider=mock", "--stream-url=https://stream.example.com"]),
            "--stream-url needs the wuapi provider"
        );
        assert!(run(&["--provider=wuapi", "--live=auto"]).live.is_some());
    }

    #[test]
    fn diagnose_stream_parses_not_in_usage() {
        assert_eq!(run(&["--diagnose=stream"]).diagnose, Some(Diagnose::Stream));
        assert!(
            parse(["--provider=mock".to_owned(), "--diagnose=stream".to_owned()].into_iter())
                .is_err()
        );
        assert!(!usage().contains("diagnose"));
    }

    #[test]
    fn takes_another_api_for_development() {
        let options = run(&["--provider=wuapi", "--api-url", "http://127.0.0.1:8787"]);
        assert_eq!(options.api_url.as_deref(), Some("http://127.0.0.1:8787"));
    }
}

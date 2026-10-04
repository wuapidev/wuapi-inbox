//! The desktop application.
//!
//! `main` wires the pieces together and gets out of the way: it picks a
//! provider, opens the local store, starts the sync engine on a Tokio
//! runtime and opens the window. From then on the UI reads the store and
//! the engine keeps the store fresh. With wuapi and no API key yet, the
//! window opens on the sign-in screen and the engine starts once the
//! person has signed in.

// A release build for Windows is a window, not a console program: no
// terminal opens behind it. (A debug build keeps the console, for its log.)
#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

mod animation;
mod attach;
mod audio;
mod brand;
mod calendar;
mod cli;
mod clipboard;
mod emoji;
mod format;
mod gifs;
mod icons;
mod keys;
mod linking;
mod login;
mod markup;
mod mentioning;
mod motion;
mod pictures;
mod product;
mod providers;
mod rail;
mod record;
mod rename;
mod settings;
mod storage;
mod stories;
mod stretch;
mod theme;
mod ui;
mod update;

use gpui_kit::{
    px, size, App, AppContext as _, Bounds, TitlebarOptions, WindowBounds, WindowOptions,
};
use ui::AppView;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                "warn,client_core=info,provider_wuapi=info,updater=info".into()
            }),
        )
        .with_writer(std::io::stderr)
        .init();

    let options = match cli::parse(std::env::args().skip(1)) {
        Ok(cli::Command::Run(options)) => options,
        Ok(cli::Command::Help) => {
            println!("{}", cli::usage());
            return;
        }
        Err(message) => {
            eprintln!("{message}\n\n{}", cli::usage());
            std::process::exit(2);
        }
    };

    // `--diagnose`: a report on the terminal, and no window. `chats` and
    // `stories` read the stored key and ask the provider what the
    // application would ask; `sends` reads the timings of the last sends
    // from the local database. None of them changes anything.
    if let Some(report) = options.diagnose {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("the Tokio runtime starts");
        match runtime.block_on(providers::diagnose(&options, report)) {
            Ok(lines) => {
                for line in lines {
                    println!("{line}");
                }
                return;
            }
            Err(message) => {
                eprintln!("{}: {message}", product::PRODUCT_NAME);
                std::process::exit(1);
            }
        }
    }

    // An install made under the working name becomes this one, once. Not
    // with `--data-dir`: that directory is whoever passed it's business.
    if options.data_dir.is_none() {
        let outcome = rename::migrate(
            &product::data_dir_of(product::LEGACY_SLUG),
            &product::data_dir(),
            product::LEGACY_SLUG,
            product::SLUG,
            &rename::OsSecrets,
        );
        match outcome {
            rename::Outcome::Moved => tracing::info!("moved the data of the earlier install"),
            rename::Outcome::NotMoved(reason) => {
                tracing::warn!(%reason, "the data of the earlier install was not moved")
            }
            rename::Outcome::NothingToDo => {}
        }
    }

    // Started by "Restart now": the instance that asked is given the
    // time to end, so that two never work the same data.
    updater::wait_for_parent(std::time::Duration::from_secs(15));

    // An update that was downloaded and checked while the application ran
    // is put in place now, and the new version started and watched; if it
    // does not come up, the version before is put back and goes on here.
    let boot = update::Boot::new(&options);
    if let updater::Outcome::Exit(code) = boot.at_start() {
        std::process::exit(code);
    }

    // Files that were opened with other applications last time were
    // written out in clear to be opened; they do not outlive the session.
    ui::clear_exports();

    // Network and database work runs here; the UI has its own executor.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .thread_name("sync")
        .build()
        .expect("the Tokio runtime starts");

    // Whatever can start without the user starts now: the demo data, or
    // wuapi with the API key already in the keychain. Otherwise the window
    // opens on the sign-in screen and the session starts from there.
    boot.opening_store();
    let launch = match providers::launch(&options, runtime.handle()) {
        Ok(launch) => launch,
        Err(message) => {
            eprintln!("{}: {message}", product::PRODUCT_NAME);
            std::process::exit(1);
        }
    };

    let appearance = options.appearance;
    let motion_at = options.motion_at;
    let welcome = options.welcome;
    let app_id = options
        .app_id
        .clone()
        .unwrap_or_else(|| product::APP_ID.to_owned());
    let settings_file = options
        .data_dir
        .clone()
        .unwrap_or_else(product::data_dir)
        .join(settings::FILE_NAME);
    let open_chat = options.open_chat;
    let sync = runtime.handle().clone();
    gpui_kit::application()
        .with_assets(icons::Assets)
        .run(move |cx: &mut App| {
            gpui_kit::init(cx);
            theme::install_fonts(cx);
            // The saved preferences; `--theme` has the say for this run.
            settings::init(Some(settings_file), appearance, cx);
            if let Some(seconds) = motion_at {
                cx.set_global(motion::FrozenAt(std::time::Duration::from_secs_f32(
                    seconds,
                )));
            }

            let bounds = Bounds::centered(None, size(px(1240.), px(800.)), cx);
            let window = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(820.), px(520.))),
                titlebar: Some(TitlebarOptions {
                    title: Some(product::PRODUCT_NAME.into()),
                    ..Default::default()
                }),
                app_id: Some(app_id),
                icon: brand::window_icon(),
                ..Default::default()
            };
            gpui_kit::open_window(window, cx, |window, cx| {
                cx.new(|cx| AppView::new(launch, open_chat, welcome, window, cx))
            })
            .expect("the main window opens");

            // The window is up: a version that was just installed has
            // started, and the one before it is no longer kept. From here
            // the updater looks for the next one in the background.
            if boot.confirm_started() {
                tracing::info!("this is the first start after an update");
            }
            boot.start(&sync, cx);

            // Closing the window ends the application on every platform.
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            cx.activate(true);
        });

    // Queued messages are already on disk; stopping the engine's tasks with
    // the runtime loses nothing.
    runtime.shutdown_background();
}

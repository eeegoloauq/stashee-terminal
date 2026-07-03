//! Entry point: CLI dispatch, application setup, CSS.

mod cli;
mod clipboard;
mod grid;
mod keys;
mod pane;
mod paths;
mod proxy;
mod settings;
mod sidebar;
mod window;

use gtk4 as gtk;
use gtk4::gio;
use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;

const APP_ID: &str = "dev.stashee.Terminal";

fn main() -> glib::ExitCode {
    // The OSC 52 proxy is a plain byte relay living inside a pane's
    // pty; it must run before logging, GTK, and the D-Bus
    // single-instance machinery. OsString end to end — the wrapped
    // command is not required to be UTF-8.
    let argv: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if argv.first().is_some_and(|arg| arg == proxy::FLAG) {
        std::process::exit(proxy::run(&argv[1..]));
    }

    // STASHEE_LOG controls verbosity (tracing env-filter syntax).
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("STASHEE_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    // The whole CLI is validated locally, before GApplication: `list`,
    // `--help`, and `--version` must work without a display, and error
    // messages must print in the invoking terminal even when the
    // arguments would otherwise be forwarded to the primary instance.
    match parse_args() {
        Cli::List => return cli::list(),
        Cli::Config => return cli::edit_config(),
        Cli::Welcome => return cli::welcome(),
        Cli::Help => {
            use std::io::IsTerminal;
            print!("{}", cli::help_text(std::io::stdout().is_terminal()));
            return glib::ExitCode::SUCCESS;
        }
        Cli::Version => {
            println!("stashee {}", env!("CARGO_PKG_VERSION"));
            return glib::ExitCode::SUCCESS;
        }
        Cli::Error(message) => {
            eprintln!("stashee: {message}\n\n{}", cli::usage());
            return glib::ExitCode::FAILURE;
        }
        Cli::Open => {}
    }

    // Keybindings live on a capture-phase ShortcutController in the
    // window (see window.rs), so they win over the terminal's input.
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();
    // Startup runs only in the primary instance — exactly one
    // clipboard listener, like the window itself.
    app.connect_startup(|_| {
        load_css();
        clipboard::serve();
    });
    app.connect_activate(window::present);
    // A second invocation's argv arrives here in the primary instance
    // over D-Bus (SPEC.md "CLI"). It passed parse_args() in its own
    // process, so extraction cannot fail.
    app.connect_command_line(|app, cmdline| {
        match cmdline.arguments().get(1).and_then(|arg| arg.to_str()) {
            Some(name) => window::present_workflow(app, name),
            None => window::present(app),
        }
        glib::ExitCode::SUCCESS
    });
    app.run()
}

enum Cli {
    /// Open the app; the workflow name, if any, is re-read from the
    /// forwarded argv in the `command-line` handler.
    Open,
    List,
    Config,
    /// Hidden: the first-run pane's greeting (see window.rs).
    Welcome,
    Help,
    Version,
    Error(String),
}

fn parse_args() -> Cli {
    let mut args: Vec<String> = Vec::new();
    for argument in std::env::args_os().skip(1) {
        match argument.to_str() {
            Some(arg) => args.push(arg.to_owned()),
            None => return Cli::Error("arguments must be valid UTF-8".to_owned()),
        }
    }
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        return Cli::Help;
    }
    if args.iter().any(|arg| arg == "--version" || arg == "-V") {
        return Cli::Version;
    }
    match args.as_slice() {
        [] => Cli::Open,
        [command] if command == "help" => Cli::Help,
        [command] if command == "list" => Cli::List,
        [command] if command == "config" => Cli::Config,
        [flag] if flag == "--welcome" => Cli::Welcome,
        [name] if !name.is_empty() && !name.starts_with('-') => Cli::Open,
        [arg] => Cli::Error(format!("unexpected argument {arg:?}")),
        _ => Cli::Error("expected at most one argument".to_owned()),
    }
}

fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(include_str!("../data/style.css"));
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use netweevil_api::{
    ApiServeOptions, discover_console_dir, embedded_console_available, serve as serve_api,
};
use netweevil_persist::{WorkspacePaths, read_workspace_config};

#[derive(Subcommand, Debug)]
pub(crate) enum ApiCommand {
    Serve(ApiServeArgs),
}

#[derive(Args, Debug)]
pub(crate) struct ApiServeArgs {
    /// Dataset to load. Without it the selection saved in
    /// `.netweevil/workspace.json` is used, or the API starts in setup mode.
    #[arg(long, requires = "default_profile")]
    dataset: Option<String>,
    #[arg(long, requires = "dataset")]
    default_profile: Option<PathBuf>,
    #[arg(long, default_value = "127.0.0.1:8080")]
    bind: SocketAddr,
    #[arg(long = "profile")]
    profiles: Vec<PathBuf>,
    #[arg(long = "transit-feed")]
    transit_feeds: Vec<String>,
    /// Built web console (`frontend/dist`) to serve at `/`; auto-detected.
    #[arg(long)]
    console_dir: Option<PathBuf>,
}

/// `netweevil bootstrap`: the beginner entry point. Creates the workspace,
/// explains where files go, and serves the API plus web console in setup
/// mode so data, profiles and GTFS feeds can be added from the browser.
#[derive(Args, Debug)]
pub(crate) struct BootstrapArgs {
    #[arg(long, default_value = "127.0.0.1:8080")]
    bind: SocketAddr,
    /// Built web console (`frontend/dist`) to serve at `/`; the copy embedded
    /// in this binary is used when present.
    #[arg(long)]
    console_dir: Option<PathBuf>,
    /// Do not open the console in the default browser.
    #[arg(long)]
    no_open: bool,
}

pub(crate) fn api_serve(paths: WorkspacePaths, args: ApiServeArgs) -> Result<()> {
    run_server(
        paths,
        ApiServeOptions {
            bind: args.bind,
            dataset_id: args.dataset,
            default_profile: args.default_profile,
            profiles: args.profiles,
            transit_feeds: args.transit_feeds,
            console_dir: args.console_dir,
        },
        false,
    )
}

pub(crate) fn bootstrap(paths: WorkspacePaths, args: BootstrapArgs) -> Result<()> {
    if !args.no_open {
        open_browser_later(format!("http://{}/", display_host(args.bind)));
    }
    run_server(
        paths,
        ApiServeOptions {
            bind: args.bind,
            dataset_id: None,
            default_profile: None,
            profiles: Vec::new(),
            transit_feeds: Vec::new(),
            console_dir: args.console_dir,
        },
        true,
    )
}

fn run_server(paths: WorkspacePaths, options: ApiServeOptions, banner: bool) -> Result<()> {
    if banner {
        print_bootstrap_banner(&paths, &options)?;
    }
    let runtime = tokio::runtime::Runtime::new().context("creating API runtime")?;
    runtime.block_on(serve_api(paths, options))
}

/// `0.0.0.0` listens everywhere but is not a browsable address.
fn display_host(bind: SocketAddr) -> String {
    if bind.ip().is_unspecified() {
        format!("127.0.0.1:{}", bind.port())
    } else {
        bind.to_string()
    }
}

/// Opens the console once the server has had a moment to bind. Failures are
/// silent: the URL is printed in the banner either way.
fn open_browser_later(url: String) {
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(900));
        #[cfg(target_os = "windows")]
        let result = std::process::Command::new("cmd")
            .args(["/C", "start", "", &url])
            .spawn();
        #[cfg(target_os = "macos")]
        let result = std::process::Command::new("open").arg(&url).spawn();
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        let result = std::process::Command::new("xdg-open").arg(&url).spawn();
        let _ = result;
    });
}

fn print_bootstrap_banner(paths: &WorkspacePaths, options: &ApiServeOptions) -> Result<()> {
    let console = options.console_dir.clone().or_else(|| {
        if embedded_console_available() {
            None
        } else {
            discover_console_dir(&paths.root)
        }
    });
    let saved = read_workspace_config(paths)?;
    eprintln!("NetWeevil bootstrap");
    eprintln!("  workspace root : {}", paths.root.display());
    eprintln!("  state directory: {}", paths.state_dir.display());
    eprintln!("  everything NetWeevil writes goes under the state directory:");
    for location in paths.locations().iter().skip(1) {
        eprintln!("    {:<18} {}", location.key, location.path);
    }
    match saved {
        Some(saved) => eprintln!(
            "  saved selection: dataset '{}', profile {}, {} transit feed(s)",
            saved.dataset_id,
            saved.default_profile,
            saved.transit_feeds.len()
        ),
        None => eprintln!("  saved selection: none yet (setup mode)"),
    }
    let host = display_host(options.bind);
    match console {
        Some(dir) => eprintln!(
            "  console        : http://{host}/  (from {})",
            dir.display()
        ),
        None if embedded_console_available() => {
            eprintln!("  console        : http://{host}/  (built into this executable)")
        }
        None => eprintln!(
            "  console        : not built. Run `pnpm install && pnpm build` in frontend/ and rebuild, or `pnpm dev` there and open http://localhost:5173/"
        ),
    }
    eprintln!("  api            : http://{host}/v1/service");
    eprintln!("  stop           : press Ctrl+C in this window");
    eprintln!(
        "Open the console and use Setup (bottom left) to add a street network, profiles and GTFS feeds."
    );
    Ok(())
}

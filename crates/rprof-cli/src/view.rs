use std::{
    fs::File,
    io::{BufWriter, Write},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result};
use clap::Args;
use signal_hook::consts::{SIGINT, SIGTERM};
use tiny_http::{Header, Method, Response, Server, StatusCode};

use crate::report::TimelineLimits;

#[derive(Args)]
pub struct ViewArgs {
    input: PathBuf,
    /// Loopback port; 0 chooses an available port.
    #[arg(long, default_value_t = 0)]
    port: u16,
    /// Open the URL with xdg-open in addition to printing it.
    #[arg(long)]
    open: bool,
    #[command(flatten)]
    limits: TimelineLimits,
}

pub fn view(args: ViewArgs) -> Result<()> {
    let report = rprof_report::analyze(&args.input, args.limits.options(true))?;
    let mut file = tempfile::NamedTempFile::new()?;
    {
        let mut out = BufWriter::new(file.as_file_mut());
        rprof_report::write_html(&report, &mut out)?;
        out.flush()?;
    }
    drop(report);
    let server = Server::http(("127.0.0.1", args.port))
        .map_err(|e| anyhow::anyhow!("starting local viewer: {e}"))?;
    let address = server
        .server_addr()
        .to_ip()
        .context("viewer has no TCP address")?;
    let url = format!("http://{address}/");
    let stopped = Arc::new(AtomicBool::new(false));
    let interrupt = signal_hook::flag::register(SIGINT, Arc::clone(&stopped))?;
    let terminate = signal_hook::flag::register(SIGTERM, Arc::clone(&stopped))?;
    println!("Viewer: {url}");
    std::io::stdout().flush()?;
    eprintln!("Serving a report snapshot; Ctrl-C to stop.");
    if args.open {
        match std::process::Command::new("xdg-open").arg(&url).spawn() {
            Ok(mut browser) => {
                std::thread::spawn(move || {
                    let _ = browser.wait();
                });
            }
            Err(error) => eprintln!("Could not open browser: {error}; use {url}"),
        }
    }
    let result = (|| -> Result<()> {
        while !stopped.load(Ordering::Relaxed) {
            let Some(request) = server.recv_timeout(Duration::from_millis(100))? else {
                continue;
            };
            let route = request.url().split('?').next().unwrap_or("");
            if request.method() != &Method::Get && request.method() != &Method::Head {
                let _ = request.respond(Response::empty(StatusCode(405)));
            } else if route == "/" || route == "/index.html" {
                let response = Response::from_file(File::open(file.path())?)
                    .with_header(
                        Header::from_bytes("Content-Type", "text/html; charset=utf-8").unwrap(),
                    )
                    .with_header(Header::from_bytes("Cache-Control", "no-store").unwrap())
                    .with_header(Header::from_bytes("X-Content-Type-Options", "nosniff").unwrap());
                // A browser disconnect is not a server failure.
                let _ = request.respond(response);
            } else if route == "/favicon.ico" {
                let _ = request.respond(Response::empty(StatusCode(204)));
            } else {
                let _ = request.respond(Response::empty(StatusCode(404)));
            }
        }
        Ok(())
    })();
    signal_hook::low_level::unregister(interrupt);
    signal_hook::low_level::unregister(terminate);
    result
}

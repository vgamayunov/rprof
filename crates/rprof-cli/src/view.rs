use std::{
    fs::File,
    io::{BufWriter, Read, Write},
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
    /// Persistent, disposable viewer index directory (default: user cache/rprof/indexes).
    #[arg(long)]
    index_dir: Option<PathBuf>,
    /// Rebuild the viewer index even if the capture has not changed.
    #[arg(long)]
    reindex: bool,
    #[command(flatten)]
    limits: TimelineLimits,
}

pub fn view(args: ViewArgs) -> Result<()> {
    let cache = args.index_dir.unwrap_or_else(|| {
        std::env::var_os("XDG_CACHE_HOME")
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".cache")))
            .unwrap_or_else(|| {
                std::env::temp_dir().join(format!("rprof-{}", unsafe { libc::geteuid() }))
            })
            .join("rprof/indexes")
    });
    let index = rprof_report::indexed::CaptureIndex::open(&args.input, &cache, args.reindex)?;
    eprintln!(
        "{} viewer index: {}",
        if index.reused { "Reusing" } else { "Built" },
        index.path.display()
    );
    let report = index.report()?;
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
    eprintln!("Serving an indexed capture snapshot; zoom/expand to load detail. Ctrl-C to stop.");
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
            let Some(mut request) = server.recv_timeout(Duration::from_millis(100))? else {
                continue;
            };
            let route = request.url().split('?').next().unwrap_or("");
            if route == "/api/window" || route == "/api/files" {
                if request.method() != &Method::Post {
                    let _ = request.respond(Response::empty(StatusCode(405)));
                    continue;
                }
                let route = route.to_owned();
                let result = (|| -> Result<Vec<u8>> {
                    anyhow::ensure!(
                        request.body_length().is_none_or(|n| n <= 256 * 1024),
                        "query exceeds 256 KiB"
                    );
                    let mut body = Vec::new();
                    request
                        .as_reader()
                        .take(256 * 1024 + 1)
                        .read_to_end(&mut body)?;
                    anyhow::ensure!(body.len() <= 256 * 1024, "query exceeds 256 KiB");
                    let output = if route == "/api/window" {
                        let limits = args.limits.options(true);
                        serde_json::to_vec(&index.window(
                            serde_json::from_slice(&body)?,
                            limits.max_events,
                            limits.max_bytes,
                        )?)?
                    } else {
                        serde_json::to_vec(&index.files(serde_json::from_slice(&body)?)?)?
                    };
                    anyhow::ensure!(
                        output.len() <= 16 * 1024 * 1024,
                        "window response exceeds 16 MiB; request fewer tracks"
                    );
                    Ok(output)
                })();
                let (status, data) = match result {
                    Ok(data) => (200, data),
                    Err(error) => (
                        400,
                        serde_json::to_vec(&serde_json::json!({"error": error.to_string()}))?,
                    ),
                };
                let _ = request.respond(
                    Response::from_data(data)
                        .with_status_code(StatusCode(status))
                        .with_header(
                            Header::from_bytes("Content-Type", "application/json; charset=utf-8")
                                .unwrap(),
                        )
                        .with_header(Header::from_bytes("Cache-Control", "no-store").unwrap())
                        .with_header(
                            Header::from_bytes("X-Content-Type-Options", "nosniff").unwrap(),
                        ),
                );
            } else if request.method() != &Method::Get && request.method() != &Method::Head {
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

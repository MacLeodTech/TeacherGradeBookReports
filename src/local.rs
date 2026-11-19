//! Local mode: the app on a teacher's own computer.
//!
//! - Listens on 127.0.0.1 only, so nothing else on the network can reach it.
//! - No passwords: the app opens the browser at a link with a secret that
//!   changes every launch. Without it (another program or user on the same
//!   computer) the app shows nothing.
//! - Requests must say `Host: 127.0.0.1` or `localhost`, so a web page can't
//!   reach the app through DNS rebinding.
//! - Student data is in memory by default and gone when the app closes; the
//!   teacher keeps their workbook file. `--keep-data` saves it on this
//!   computer instead (for board laptops approved for student data).

use std::{net::SocketAddr, path::PathBuf};

use axum::{
    extract::Request,
    http::{StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::config::{Config, Storage};

pub const LOCAL_USER: &str = "teacher";

pub const USAGE: &str = "\
Gradebook

USAGE:
  gradebook                 Windows/Mac: start the local app and open it in your browser.
                            Linux/servers: run the server (configured by environment variables).
  gradebook local [options] Start the local app on this computer.
      --keep-data           Save classes and marks on this computer (default: only while open).
      --port <n>            Port to use (default 8765).
      --data-dir <path>     Where settings are kept (default: your user data folder).
      --no-browser          Don't open the browser.
  gradebook serve           Run the server (see README for settings).
  gradebook healthcheck     Exit 0 if the server on $BIND answers (for Docker).";

struct Opts {
    storage: Storage,
    port: u16,
    data_dir: PathBuf,
    browser: bool,
}

fn parse(args: &[String]) -> anyhow::Result<Opts> {
    let mut o = Opts {
        storage: Storage::Memory,
        port: 8765,
        data_dir: dirs::data_dir().unwrap_or_else(std::env::temp_dir).join("Gradebook"),
        browser: true,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--keep-data" => o.storage = Storage::Persistent,
            "--no-browser" => o.browser = false,
            "--port" => o.port = it.next().and_then(|v| v.parse().ok()).ok_or_else(|| anyhow::anyhow!("--port needs a number"))?,
            "--data-dir" => o.data_dir = PathBuf::from(it.next().ok_or_else(|| anyhow::anyhow!("--data-dir needs a path"))?),
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => anyhow::bail!("unknown option '{other}'\n\n{USAGE}"),
        }
    }
    Ok(o)
}

/// The link with this launch's secret.
pub fn launch_url(port: u16, token: &str) -> String {
    format!("http://127.0.0.1:{port}/launch?token={token}")
}

fn write_private(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::{io::Write, os::unix::fs::OpenOptionsExt};
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
        f.write_all(contents.as_bytes())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, contents)
    }
}

pub async fn run(args: &[String]) -> anyhow::Result<()> {
    crate::init_logging("gradebook=warn");
    let opts = parse(args)?;
    std::fs::create_dir_all(&opts.data_dir)?;
    let link_file = opts.data_dir.join("open-gradebook.url");

    // Already running? Open the existing window's link instead of starting twice.
    let listener = match tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], opts.port))).await {
        Ok(l) => l,
        Err(e) => {
            if let Ok(url) = std::fs::read_to_string(&link_file) {
                let url = url.trim();
                if url.starts_with(&format!("http://127.0.0.1:{}/", opts.port)) {
                    println!("Gradebook is already running. Opening it in your browser…");
                    if opts.browser {
                        let _ = open::that(url);
                    }
                    return Ok(());
                }
            }
            anyhow::bail!("Couldn't use port {} ({e}). Try `gradebook local --port 8766`.", opts.port);
        }
    };
    let port = listener.local_addr()?.port();
    let config = Config::local(opts.data_dir.clone(), port, opts.storage);
    let url = launch_url(port, config.launch_token.as_deref().unwrap_or_default());
    write_private(&link_file, &url)?;

    let state = crate::build_state(config).await?;
    crate::bootstrap(&state)?;

    println!("Gradebook is running on this computer only.");
    match opts.storage {
        Storage::Memory => println!(
            "Student data is kept in memory and cleared when Gradebook closes.\n\
             Use \"Save workbook\" to keep your work, and open that file next time."
        ),
        Storage::Persistent => println!("Classes and marks are saved in {}", opts.data_dir.display()),
    }
    println!(
        "\nIf your browser doesn't open, go to:\n  {url}\n\nKeep this window open while you work. Close it (or click Quit in Gradebook) to stop."
    );
    if opts.browser && open::that(&url).is_err() {
        println!("(Couldn't open the browser automatically.)");
    }
    let r = crate::serve(state, listener).await;
    let _ = std::fs::remove_file(&link_file);
    r
}

/// Only answer requests addressed to this computer by name.
pub async fn host_check(req: Request, next: Next) -> Response {
    let host = req.headers().get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("");
    let name = host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host);
    if matches!(name, "127.0.0.1" | "localhost" | "[::1]") {
        next.run(req).await
    } else {
        (StatusCode::FORBIDDEN, "Gradebook only answers on this computer (http://127.0.0.1).").into_response()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn options() {
        let o = super::parse(&["--keep-data".into(), "--port".into(), "9000".into(), "--no-browser".into()]).unwrap();
        assert_eq!(o.storage, crate::config::Storage::Persistent);
        assert_eq!(o.port, 9000);
        assert!(!o.browser);
        assert!(super::parse(&["--bogus".into()]).is_err());
        assert_eq!(super::parse(&[]).unwrap().storage, crate::config::Storage::Memory);
    }
}

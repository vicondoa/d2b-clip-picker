use clap::Parser;
use d2b_clip_picker::placement::Placement;
use d2b_clip_picker::protocol::{IpcPeer, OpenRequest};
use d2b_clip_picker::ui;
use log::{debug, error};
use std::os::fd::FromRawFd;
use std::os::unix::net::UnixStream;

#[derive(Debug, Parser)]
#[command(
    name = "d2b-clip-picker",
    version,
    about = "UI-only d2b clipboard picker"
)]
struct Args {
    /// Inherited socketpair file descriptor from d2b-clipd.
    #[arg(long = "ipc-fd")]
    ipc_fd: i32,
}

fn main() {
    if let Err(err) = run() {
        error!("picker failed: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"))
        .format_timestamp_secs()
        .try_init()
        .ok();

    let args = Args::parse();
    if args.ipc_fd < 0 {
        return Err("--ipc-fd must be non-negative".into());
    }

    let flags = unsafe { libc::fcntl(args.ipc_fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(format!(
            "--ipc-fd {} is not an open inherited file descriptor: {}",
            args.ipc_fd,
            std::io::Error::last_os_error()
        )
        .into());
    }

    let stream = unsafe { UnixStream::from_raw_fd(args.ipc_fd) };
    let mut peer = IpcPeer::new(stream)?;
    peer.send_client_hello()?;
    let request = match peer.read_clipd_frame()? {
        d2b_clip_picker::protocol::ClipdFrame::OpenRequest(request) => request,
        d2b_clip_picker::protocol::ClipdFrame::Close { .. } => return Ok(()),
        d2b_clip_picker::protocol::ClipdFrame::Error { code, .. } => {
            return Err(format!("clipd rejected picker request: {code}").into());
        }
    };

    let placement = choose_placement(&request);
    debug!("starting picker UI");
    ui::run_picker(request, peer, placement)?;
    Ok(())
}

fn choose_placement(request: &OpenRequest) -> Placement {
    if let Some(placement) = request
        .placement_hints
        .as_ref()
        .and_then(Placement::from_hints)
    {
        return placement;
    }

    Placement::default()
}

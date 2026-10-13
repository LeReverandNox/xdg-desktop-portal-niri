//! Fast, cached fuzzel picker for ScreenCast sources — a port of `xdp-wlr-chooser`, upgraded
//! to also offer per-window capture (niri's own IPC gives us window ids directly, so we don't
//! need `ext-image-copy-capture-v1`).

use std::collections::BTreeMap;
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail};
use niri_ipc::socket::Socket;
use niri_ipc::{Request, Response};

use crate::mutter::Target;

const CACHE_TTL: Duration = Duration::from_secs(10);

/// Mirrors the portal's `SourceType` bitmask: `MONITOR = 1`, `WINDOW = 2`.
#[derive(Debug, Clone, Copy)]
pub struct SourceTypes {
    pub monitor: bool,
    pub window: bool,
}

impl SourceTypes {
    pub fn from_mask(mask: u32) -> Self {
        Self {
            monitor: mask & 1 != 0,
            window: mask & 2 != 0,
        }
    }
}

fn cache_path() -> std::path::PathBuf {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    std::path::Path::new(&runtime_dir).join("niri-screencast-portal.cache")
}

fn read_cache() -> Option<Target> {
    let path = cache_path();
    let metadata = std::fs::metadata(&path).ok()?;
    let age = SystemTime::now().duration_since(metadata.modified().ok()?).ok()?;
    if age >= CACHE_TTL {
        return None;
    }
    let contents = std::fs::read_to_string(&path).ok()?;
    parse_target(contents.trim())
}

fn write_cache(target: &Target) {
    let path = cache_path();
    let serialized = serialize_target(target);
    let _ = std::fs::write(path, serialized);
}

fn serialize_target(target: &Target) -> String {
    match target {
        Target::Monitor(connector) => format!("monitor:{connector}"),
        Target::Window(id) => format!("window:{id}"),
    }
}

fn parse_target(s: &str) -> Option<Target> {
    let (kind, value) = s.split_once(':')?;
    match kind {
        "monitor" => Some(Target::Monitor(value.to_string())),
        "window" => value.parse().ok().map(Target::Window),
        _ => None,
    }
}

/// Runs the chooser (or returns the cached pick) and returns the selected capture target.
/// Returns `Ok(None)` if the user cancelled the picker.
pub async fn choose(source_types: SourceTypes) -> Result<Option<Target>> {
    if let Some(cached) = read_cache() {
        return Ok(Some(cached));
    }

    let target = tokio::task::spawn_blocking(move || run_picker(source_types))
        .await
        .context("chooser task panicked")??;

    if let Some(target) = &target {
        write_cache(target);
    }

    Ok(target)
}

fn run_picker(source_types: SourceTypes) -> Result<Option<Target>> {
    let mut choices: BTreeMap<String, Target> = BTreeMap::new();

    if source_types.monitor {
        for (connector, output) in list_outputs()? {
            if output.logical.is_none() {
                // Disabled output — nothing to capture.
                continue;
            }
            let label = format!("Screen: {} ({connector})", friendly_output_name(&output));
            choices.insert(label, Target::Monitor(connector));
        }
    }

    if source_types.window {
        for window in list_windows()? {
            let app_id = window.app_id.as_deref().unwrap_or("?");
            let title = window.title.as_deref().unwrap_or("(untitled)");
            let label = format!("Window [{app_id}]: {title}");
            choices.insert(label, Target::Window(window.id));
        }
    }

    if choices.is_empty() {
        bail!("no capturable sources (monitors/windows) found");
    }

    let mut labels: Vec<&str> = choices.keys().map(String::as_str).collect();
    labels.sort_unstable();
    let menu = labels.join("\n");

    let selected = run_fuzzel(&menu)?;
    let Some(selected) = selected else {
        return Ok(None);
    };

    Ok(choices.remove(selected.trim()))
}

fn run_fuzzel(menu: &str) -> Result<Option<String>> {
    let mut child = Command::new("fuzzel")
        .args(["--dmenu", "--prompt", "Share: "])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("failed to spawn fuzzel — is it installed and in PATH?")?;

    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(menu.as_bytes())
        .context("failed to write menu to fuzzel's stdin")?;

    let output = child.wait_with_output().context("fuzzel did not exit cleanly")?;
    if !output.status.success() {
        // Non-zero exit (e.g. Escape pressed) means the user cancelled.
        return Ok(None);
    }

    let selected = String::from_utf8(output.stdout).context("fuzzel produced non-UTF8 output")?;
    if selected.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(selected))
}

fn list_outputs() -> Result<BTreeMap<String, niri_ipc::Output>> {
    let mut socket = Socket::connect().context("failed to connect to the niri IPC socket")?;
    let reply = socket
        .send(Request::Outputs)
        .context("niri msg outputs failed")?
        .map_err(|err| anyhow::anyhow!("niri returned an error for Outputs: {err}"))?;
    match reply {
        Response::Outputs(outputs) => Ok(outputs.into_iter().collect()),
        other => bail!("unexpected niri IPC response to Outputs: {other:?}"),
    }
}

fn list_windows() -> Result<Vec<niri_ipc::Window>> {
    let mut socket = Socket::connect().context("failed to connect to the niri IPC socket")?;
    let reply = socket
        .send(Request::Windows)
        .context("niri msg windows failed")?
        .map_err(|err| anyhow::anyhow!("niri returned an error for Windows: {err}"))?;
    match reply {
        Response::Windows(windows) => Ok(windows),
        other => bail!("unexpected niri IPC response to Windows: {other:?}"),
    }
}

fn friendly_output_name(output: &niri_ipc::Output) -> String {
    format!("{} {}", output.make, output.model)
}

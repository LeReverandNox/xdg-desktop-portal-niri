//! Thin zbus proxies for niri's `org.gnome.Mutter.ScreenCast` D-Bus interface.
//!
//! niri exposes this directly on the session bus (same contract GNOME Shell/Mutter use),
//! which is the reliable capture path we're routing around `-wlr`'s PipeWire-stall bug to get.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use zbus::Connection;
use zbus::proxy;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

#[proxy(
    default_service = "org.gnome.Mutter.ScreenCast",
    default_path = "/org/gnome/Mutter/ScreenCast",
    interface = "org.gnome.Mutter.ScreenCast"
)]
trait ScreenCast {
    fn create_session(
        &self,
        properties: HashMap<&str, Value<'_>>,
    ) -> zbus::Result<OwnedObjectPath>;
}

#[proxy(default_service = "org.gnome.Mutter.ScreenCast", interface = "org.gnome.Mutter.ScreenCast.Session")]
trait ScreenCastSession {
    fn record_monitor(
        &self,
        connector: &str,
        properties: HashMap<&str, Value<'_>>,
    ) -> zbus::Result<OwnedObjectPath>;

    fn record_window(
        &self,
        properties: HashMap<&str, Value<'_>>,
    ) -> zbus::Result<OwnedObjectPath>;

    fn start(&self) -> zbus::Result<()>;

    fn stop(&self) -> zbus::Result<()>;

    #[zbus(signal)]
    fn closed(&self) -> zbus::Result<()>;
}

#[proxy(default_service = "org.gnome.Mutter.ScreenCast", interface = "org.gnome.Mutter.ScreenCast.Stream")]
trait ScreenCastStream {
    #[zbus(signal)]
    fn pipe_wire_stream_added(&self, node_id: u32) -> zbus::Result<()>;

    #[zbus(property)]
    fn parameters(&self) -> zbus::Result<HashMap<String, OwnedValue>>;
}

/// What to capture, as picked by the chooser.
#[derive(Debug, Clone)]
pub enum Target {
    Monitor(String),
    Window(u64),
}

/// Position/size reported by the Mutter stream once it starts flowing (best-effort: niri
/// currently reports a stub `(1, 1)` size for window streams, see niri issue #4587).
#[derive(Debug, Clone, Copy, Default)]
pub struct StreamGeometry {
    pub position: Option<(i32, i32)>,
    pub size: Option<(i32, i32)>,
}

/// A live capture: the niri-side session/stream plus the PipeWire node id the central
/// `xdg-desktop-portal` dispatcher will use for `OpenPipeWireRemote`.
pub struct Cast {
    session: ScreenCastSessionProxy<'static>,
    pub node_id: u32,
    pub geometry: StreamGeometry,
}

impl Cast {
    pub async fn stop(&self) -> Result<()> {
        self.session
            .stop()
            .await
            .context("failed to stop Mutter.ScreenCast session")
    }
}

/// Opens a `CreateSession` -> `RecordMonitor`/`RecordWindow` -> `Start` sequence against
/// niri's `org.gnome.Mutter.ScreenCast`, and waits for the resulting stream's
/// `PipeWireStreamAdded` signal.
pub async fn start_cast(
    connection: &Connection,
    target: &Target,
    cursor_mode: u32,
) -> Result<Cast> {
    let manager = ScreenCastProxy::new(connection)
        .await
        .context("failed to connect to org.gnome.Mutter.ScreenCast (is niri running?)")?;

    let session_path = manager
        .create_session(HashMap::new())
        .await
        .context("Mutter.ScreenCast.CreateSession failed")?;

    let session = ScreenCastSessionProxy::builder(connection)
        .path(session_path.clone())?
        .build()
        .await
        .context("failed to build Mutter ScreenCast.Session proxy")?;

    let mut properties: HashMap<&str, Value<'_>> = HashMap::new();
    properties.insert("cursor-mode", Value::U32(cursor_mode));

    let stream_path = match target {
        Target::Monitor(connector) => session
            .record_monitor(connector, properties)
            .await
            .with_context(|| format!("RecordMonitor({connector}) failed"))?,
        Target::Window(window_id) => {
            properties.insert("window-id", Value::U64(*window_id));
            session
                .record_window(properties)
                .await
                .with_context(|| format!("RecordWindow({window_id}) failed"))?
        }
    };

    let stream = ScreenCastStreamProxy::builder(connection)
        .path(stream_path)?
        .build()
        .await
        .context("failed to build Mutter ScreenCast.Stream proxy")?;

    // Subscribe before Start() to avoid racing the PipeWireStreamAdded signal.
    let mut added = stream
        .receive_pipe_wire_stream_added()
        .await
        .context("failed to subscribe to PipeWireStreamAdded")?;

    session
        .start()
        .await
        .context("Mutter.ScreenCast.Session.Start failed")?;

    let node_id = {
        use futures_util::StreamExt;
        let signal = tokio::time::timeout(Duration::from_secs(5), added.next())
            .await
            .map_err(|_| anyhow!("timed out waiting for PipeWireStreamAdded"))?
            .ok_or_else(|| anyhow!("PipeWireStreamAdded stream ended unexpectedly"))?;
        signal.args()?.node_id
    };

    let geometry = stream
        .parameters()
        .await
        .ok()
        .map(|params| StreamGeometry {
            position: params.get("position").and_then(|v| v.clone().try_into().ok()),
            size: params.get("size").and_then(|v| v.clone().try_into().ok()),
        })
        .unwrap_or_default();

    Ok(Cast {
        session,
        node_id,
        geometry,
    })
}

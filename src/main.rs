mod chooser;
mod mutter;
mod portal;

use anyhow::{Context, Result};
use tracing_subscriber::EnvFilter;

const BUS_NAME: &str = "org.freedesktop.impl.portal.desktop.niri";
const OBJECT_PATH: &str = "/org/freedesktop/portal/desktop";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    let connection = zbus::Connection::session()
        .await
        .context("failed to connect to the D-Bus session bus")?;

    let portal = portal::ScreenCastPortal::new(connection.clone());

    connection
        .object_server()
        .at(OBJECT_PATH, portal)
        .await
        .context("failed to export the ScreenCast portal interface")?;

    connection
        .request_name(BUS_NAME)
        .await
        .with_context(|| format!("failed to acquire bus name {BUS_NAME}"))?;

    tracing::info!("xdg-desktop-portal-niri ready on {BUS_NAME} at {OBJECT_PATH}");

    std::future::pending::<()>().await;
    Ok(())
}

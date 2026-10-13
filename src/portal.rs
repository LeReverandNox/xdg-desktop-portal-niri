//! Implements `org.freedesktop.impl.portal.ScreenCast` (+ the companion
//! `org.freedesktop.impl.portal.Session` object) against niri's native
//! `org.gnome.Mutter.ScreenCast`, with source selection done via a cached fuzzel chooser.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;
use tracing::{error, info, warn};
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{DeserializeDict, OwnedObjectPath, OwnedValue, SerializeDict, Type};
use zbus::{Connection, ObjectServer, fdo, interface};

use crate::chooser::{self, SourceTypes};
use crate::mutter::{self, Cast, Target};

#[derive(DeserializeDict, Type, Debug, Default)]
#[zvariant(signature = "a{sv}")]
struct SelectSourcesOptions {
    types: Option<u32>,
    #[allow(dead_code)]
    multiple: Option<bool>,
    #[allow(dead_code)]
    cursor_mode: Option<u32>,
    #[allow(dead_code)]
    persist_mode: Option<u32>,
}

#[derive(SerializeDict, Type, Default)]
#[zvariant(signature = "a{sv}")]
struct StreamProperties {
    position: Option<(i32, i32)>,
    size: Option<(i32, i32)>,
    source_type: Option<u32>,
}

#[derive(SerializeDict, Type, Default)]
#[zvariant(signature = "a{sv}")]
struct StartResults {
    streams: Vec<(u32, StreamProperties)>,
}

const SOURCE_TYPE_MONITOR: u32 = 1;
const SOURCE_TYPE_WINDOW: u32 = 2;
const CURSOR_MODE_EMBEDDED: u32 = 2;

struct SessionState {
    #[allow(dead_code)]
    app_id: String,
    target: Option<Target>,
    cast: Option<Cast>,
}

type Sessions = Arc<Mutex<HashMap<OwnedObjectPath, SessionState>>>;

#[derive(Clone)]
pub struct ScreenCastPortal {
    connection: Connection,
    sessions: Sessions,
}

impl ScreenCastPortal {
    pub fn new(connection: Connection) -> Self {
        Self {
            connection,
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

#[interface(name = "org.freedesktop.impl.portal.ScreenCast")]
impl ScreenCastPortal {
    async fn create_session(
        &self,
        _handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        app_id: String,
        _options: HashMap<String, OwnedValue>,
        #[zbus(object_server)] object_server: &ObjectServer,
    ) -> fdo::Result<(u32, HashMap<String, OwnedValue>)> {
        info!(%session_handle, %app_id, "CreateSession");

        object_server
            .at(
                session_handle.clone(),
                SessionObject {
                    session_handle: session_handle.clone(),
                    sessions: self.sessions.clone(),
                },
            )
            .await?;

        let mut sessions = self.sessions.lock().await;
        sessions.insert(
            session_handle,
            SessionState {
                app_id,
                target: None,
                cast: None,
            },
        );

        Ok((0, HashMap::new()))
    }

    async fn select_sources(
        &self,
        _handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        _app_id: String,
        options: SelectSourcesOptions,
    ) -> fdo::Result<(u32, HashMap<String, OwnedValue>)> {
        let source_types = SourceTypes::from_mask(options.types.unwrap_or(SOURCE_TYPE_MONITOR));

        let target = chooser::choose(source_types).await.map_err(|err| {
            error!("chooser failed: {err:#}");
            fdo::Error::Failed(format!("chooser failed: {err}"))
        })?;

        let Some(target) = target else {
            // User cancelled the picker.
            return Ok((1, HashMap::new()));
        };

        let mut sessions = self.sessions.lock().await;
        let session = sessions
            .get_mut(&session_handle)
            .ok_or_else(|| fdo::Error::Failed("SelectSources called on an unknown session".into()))?;
        session.target = Some(target);

        Ok((0, HashMap::new()))
    }

    async fn start(
        &self,
        _handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        _app_id: String,
        _parent_window: String,
        _options: HashMap<String, OwnedValue>,
    ) -> fdo::Result<(u32, StartResults)> {
        let target = {
            let sessions = self.sessions.lock().await;
            let session = sessions
                .get(&session_handle)
                .ok_or_else(|| fdo::Error::Failed("Start called on an unknown session".into()))?;
            session
                .target
                .clone()
                .ok_or_else(|| fdo::Error::Failed("Start called before SelectSources".into()))?
        };

        let cast = mutter::start_cast(&self.connection, &target, CURSOR_MODE_EMBEDDED)
            .await
            .map_err(|err| {
                error!("failed to start niri screencast: {err:#}");
                fdo::Error::Failed(format!("failed to start capture: {err}"))
            })?;

        let source_type = match target {
            Target::Monitor(_) => SOURCE_TYPE_MONITOR,
            Target::Window(_) => SOURCE_TYPE_WINDOW,
        };

        let results = StartResults {
            streams: vec![(
                cast.node_id,
                StreamProperties {
                    position: cast.geometry.position,
                    size: cast.geometry.size,
                    source_type: Some(source_type),
                },
            )],
        };

        let mut sessions = self.sessions.lock().await;
        if let Some(session) = sessions.get_mut(&session_handle) {
            session.cast = Some(cast);
        }

        Ok((0, results))
    }

    #[zbus(property, name = "AvailableSourceTypes")]
    fn available_source_types(&self) -> u32 {
        SOURCE_TYPE_MONITOR | SOURCE_TYPE_WINDOW
    }

    #[zbus(property, name = "AvailableCursorModes")]
    fn available_cursor_modes(&self) -> u32 {
        // Hidden | Embedded | Metadata
        1 | 2 | 4
    }

    #[zbus(property)]
    fn version(&self) -> u32 {
        4
    }
}

/// The `org.freedesktop.impl.portal.Session` object exported at `session_handle`, so the
/// central `xdg-desktop-portal` dispatcher can tear down our capture when the caller is done.
struct SessionObject {
    session_handle: OwnedObjectPath,
    sessions: Sessions,
}

#[interface(name = "org.freedesktop.impl.portal.Session")]
impl SessionObject {
    async fn close(&self) {
        let mut sessions = self.sessions.lock().await;
        let Some(cast) = sessions.remove(&self.session_handle).and_then(|s| s.cast) else {
            return;
        };
        if let Err(err) = cast.stop().await {
            warn!("failed to stop niri screencast session on Close: {err:#}");
        }
    }

    #[zbus(signal)]
    pub async fn closed(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

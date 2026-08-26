//! Which window, if any, is covering a display, asked over X11.
//!
//! This is the Linux half of the sensor `window.rs` uses to decide whether to
//! get out of the way. It answers with a rectangle rather than a window handle,
//! because that is all the caller needs: the monitor under the middle of it is
//! what decides whether the widget is in anybody's way.
//!
//! Only the active window, its geometry and two of its properties are read.
//! Nothing is injected, no other process is opened and no memory is read, which
//! is the same promise the Windows side makes.
//!
//! Games running through Proton, Wine or any other XWayland client are X11
//! clients, so this also works inside a GNOME or KDE Wayland session for most
//! of what people actually play. A native Wayland application is invisible to
//! it: the protocol has no way to ask what another client is doing, by design.
//! Where the answer cannot be had, the widget stays where it is.

use crate::monitor::Rect;

/// The window covering its display right now, with a line describing it for the
/// log. `None` means nothing is, or that this session cannot be asked.
pub fn covering() -> Option<(Rect, String)> {
    #[cfg(target_os = "linux")]
    {
        imp::covering()
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::OnceLock;

    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt, Window};
    use x11rb::rust_connection::RustConnection;

    use crate::monitor::Rect;

    /// The window last seen covering its display. A game that loses the active
    /// window for a moment, to a notification or an overlay, is still a game,
    /// so it is rechecked before the widget is allowed back.
    static LAST_COVERING: AtomicU32 = AtomicU32::new(0);

    struct Session {
        conn: RustConnection,
        root: Window,
        active_window: u32,
        wm_state: u32,
        fullscreen: u32,
        net_wm_name: u32,
        utf8_string: u32,
    }

    /// One connection for the life of the process. A session with no X server
    /// reachable, which is a plain Wayland login without XWayland, is a
    /// permanent no rather than something to retry every third of a second.
    fn session() -> Option<&'static Session> {
        static SESSION: OnceLock<Option<Session>> = OnceLock::new();
        SESSION
            .get_or_init(|| match connect() {
                Ok(session) => Some(session),
                Err(e) => {
                    tracing::info!("no X11 display to watch for fullscreen windows: {e}");
                    None
                }
            })
            .as_ref()
    }

    fn connect() -> Result<Session, Box<dyn std::error::Error>> {
        let (conn, screen) = x11rb::connect(None)?;
        let root = conn.setup().roots[screen].root;

        let atom = |name: &str| -> Result<u32, Box<dyn std::error::Error>> {
            Ok(conn.intern_atom(false, name.as_bytes())?.reply()?.atom)
        };

        Ok(Session {
            active_window: atom("_NET_ACTIVE_WINDOW")?,
            wm_state: atom("_NET_WM_STATE")?,
            fullscreen: atom("_NET_WM_STATE_FULLSCREEN")?,
            net_wm_name: atom("_NET_WM_NAME")?,
            utf8_string: atom("UTF8_STRING")?,
            conn,
            root,
        })
    }

    pub fn covering() -> Option<(Rect, String)> {
        let session = session()?;

        if let Some(window) = active_window(session) {
            if let Some(rect) = covers_its_monitor(session, window) {
                LAST_COVERING.store(window, Ordering::Relaxed);
                return Some((rect, describe(session, window)));
            }
        }

        // Nothing in front qualifies, so the last one that did gets a second
        // look. It is only forgotten once it stops covering anything.
        let remembered = LAST_COVERING.load(Ordering::Relaxed);
        if remembered != 0 {
            if let Some(rect) = covers_its_monitor(session, remembered) {
                return Some((rect, describe(session, remembered)));
            }
            LAST_COVERING.store(0, Ordering::Relaxed);
        }
        None
    }

    fn active_window(session: &Session) -> Option<Window> {
        let reply = session
            .conn
            .get_property(
                false,
                session.root,
                session.active_window,
                AtomEnum::WINDOW,
                0,
                1,
            )
            .ok()?
            .reply()
            .ok()?;
        // Bound rather than chained: the iterator borrows the reply, and as the
        // tail expression it would otherwise outlive it.
        let mut values = reply.value32()?;
        values.next().filter(|id| *id != 0)
    }

    /// The window's rectangle in root coordinates, when it covers the display it
    /// sits on. Two ways of qualifying, because they catch different things: a
    /// well behaved application declares `_NET_WM_STATE_FULLSCREEN`, while a
    /// game that took over the screen by sizing itself to it declares nothing.
    fn covers_its_monitor(session: &Session, window: Window) -> Option<Rect> {
        let rect = geometry(session, window)?;
        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        if width <= 0 || height <= 0 {
            return None;
        }

        if declares_fullscreen(session, window) {
            return Some(rect);
        }

        let monitor = crate::monitor::monitor_at(rect.left + width / 2, rect.top + height / 2)?;
        let bounds = monitor.bounds;
        let covers = rect.left <= bounds.left
            && rect.top <= bounds.top
            && rect.right >= bounds.right
            && rect.bottom >= bounds.bottom;

        covers.then_some(rect)
    }

    fn declares_fullscreen(session: &Session, window: Window) -> bool {
        let Ok(cookie) =
            session
                .conn
                .get_property(false, window, session.wm_state, AtomEnum::ATOM, 0, 32)
        else {
            return false;
        };
        let Ok(reply) = cookie.reply() else {
            return false;
        };
        reply
            .value32()
            .map(|mut states| states.any(|state| state == session.fullscreen))
            .unwrap_or(false)
    }

    /// Absolute position, which `get_geometry` alone does not give: its
    /// coordinates are relative to the parent, and a managed window's parent is
    /// the frame the window manager wrapped it in.
    fn geometry(session: &Session, window: Window) -> Option<Rect> {
        let size = session.conn.get_geometry(window).ok()?.reply().ok()?;
        let origin = session
            .conn
            .translate_coordinates(window, session.root, 0, 0)
            .ok()?
            .reply()
            .ok()?;

        Some(Rect::from_size(
            origin.dst_x as i32,
            origin.dst_y as i32,
            size.width as i32,
            size.height as i32,
        ))
    }

    /// A line for the log. A hidden widget with no explanation is the hardest
    /// kind of fault to chase: it looks like the app is broken when it is doing
    /// exactly what it was told.
    fn describe(session: &Session, window: Window) -> String {
        let class = text_property(
            session,
            window,
            AtomEnum::WM_CLASS.into(),
            AtomEnum::STRING.into(),
        )
        .map(|raw| {
            // Two strings separated by a NUL: the instance, then the class.
            raw.split('\0')
                .rfind(|part| !part.is_empty())
                .unwrap_or("")
                .to_string()
        })
        .unwrap_or_else(|| "unknown".into());

        let title = text_property(session, window, session.net_wm_name, session.utf8_string)
            .or_else(|| {
                text_property(
                    session,
                    window,
                    AtomEnum::WM_NAME.into(),
                    AtomEnum::STRING.into(),
                )
            })
            .unwrap_or_default();

        let geometry = geometry(session, window)
            .map(|r| {
                format!(
                    "{},{} {}x{}",
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top
                )
            })
            .unwrap_or_else(|| "unknown geometry".into());

        format!("{class} \"{title}\" at {geometry}")
    }

    fn text_property(
        session: &Session,
        window: Window,
        property: u32,
        kind: u32,
    ) -> Option<String> {
        let reply = session
            .conn
            .get_property(false, window, property, kind, 0, 256)
            .ok()?
            .reply()
            .ok()?;
        if reply.value.is_empty() {
            return None;
        }
        Some(
            String::from_utf8_lossy(&reply.value)
                .trim_end_matches('\0')
                .to_string(),
        )
    }
}

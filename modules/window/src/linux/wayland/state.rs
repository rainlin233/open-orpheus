use std::{
    collections::{HashMap, HashSet},
    os::fd::RawFd,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU32, Ordering},
    },
};

use super::codec::Iface;

const MAX_POPUP_DIMENSION: i32 = 8_192;

// ── Per-connection tracking state ──────────────────────────────────────────

pub(crate) struct WaylandConn {
    pub(crate) ifaces: HashMap<u32, Iface>,
    pub(crate) pointer_focus: HashMap<u32, u32>,
    pub(crate) pointer_position: HashMap<u32, (i32, i32)>,
    pub(crate) pointer_seat: HashMap<u32, u32>,
    pub(crate) touch_seat: HashMap<u32, u32>,
    pub(crate) xdg_to_wl: HashMap<u32, u32>,
    pub(crate) wl_to_top: HashMap<u32, u32>,
    pub(crate) top_to_xdg: HashMap<u32, u32>,
    pub(crate) compositor_id: Option<u32>,
    pub(crate) xdg_wm_base_id: Option<u32>,
    pub(crate) injected_ids: HashSet<u32>,
    pub(crate) stolen_ids: Vec<u32>,
}

impl WaylandConn {
    pub(crate) fn new() -> Self {
        let mut ifaces = HashMap::new();
        ifaces.insert(1u32, Iface::WlDisplay);
        Self {
            ifaces,
            pointer_focus: HashMap::new(),
            pointer_position: HashMap::new(),
            pointer_seat: HashMap::new(),
            touch_seat: HashMap::new(),
            xdg_to_wl: HashMap::new(),
            wl_to_top: HashMap::new(),
            top_to_xdg: HashMap::new(),
            compositor_id: None,
            xdg_wm_base_id: None,
            injected_ids: HashSet::new(),
            stolen_ids: Vec::new(),
        }
    }

    pub(crate) fn reset_tracking(&mut self) {
        self.ifaces.clear();
        self.ifaces.insert(1u32, Iface::WlDisplay);
        self.pointer_focus.clear();
        self.pointer_position.clear();
        self.pointer_seat.clear();
        self.touch_seat.clear();
        self.xdg_to_wl.clear();
        self.wl_to_top.clear();
        self.top_to_xdg.clear();
        self.compositor_id = None;
        self.xdg_wm_base_id = None;
        self.injected_ids.clear();
        self.stolen_ids.clear();
    }

    pub(crate) fn alloc_injected_id(&mut self) -> Option<u32> {
        let id = self.stolen_ids.pop()?;
        self.injected_ids.insert(id);
        Some(id)
    }

    pub(crate) fn purge(&mut self, id: u32) {
        match self.ifaces.get(&id).copied() {
            Some(Iface::WlPointer) => {
                self.pointer_focus.remove(&id);
                self.pointer_position.remove(&id);
                self.pointer_seat.remove(&id);
            }
            Some(Iface::WlTouch) => {
                self.touch_seat.remove(&id);
            }
            Some(Iface::WlSurface) => {
                self.xdg_to_wl.retain(|_, v| *v != id);
                self.wl_to_top.remove(&id);
                let focused_pointers: Vec<u32> = self
                    .pointer_focus
                    .iter()
                    .filter_map(|(pointer, surface)| (*surface == id).then_some(*pointer))
                    .collect();
                for pointer in focused_pointers {
                    self.pointer_focus.remove(&pointer);
                    self.pointer_position.remove(&pointer);
                }
            }
            Some(Iface::XdgSurface) => {
                let owned_top = self
                    .top_to_xdg
                    .iter()
                    .find(|(_, v)| **v == id)
                    .map(|(k, _)| *k);
                if let Some(tid) = owned_top {
                    self.purge(tid);
                }
                self.xdg_to_wl.remove(&id);
            }
            Some(Iface::XdgToplevel | Iface::XdgPopupShim) => {
                self.top_to_xdg.remove(&id);
                self.wl_to_top.retain(|_, v| *v != id);
            }
            Some(Iface::WlSeat) => {
                self.pointer_seat.retain(|_, v| *v != id);
                self.touch_seat.retain(|_, v| *v != id);
            }
            Some(Iface::WlCompositor) if self.compositor_id == Some(id) => {
                self.compositor_id = None;
            }
            Some(Iface::XdgWmBase) if self.xdg_wm_base_id == Some(id) => {
                self.xdg_wm_base_id = None;
            }
            _ => {}
        }
        self.ifaces.remove(&id);
    }

    pub(crate) fn wl_surface_for_window_object(&self, id: u32, iface: Iface) -> Option<u32> {
        match iface {
            Iface::WlSurface => Some(id),
            Iface::XdgSurface => self.xdg_to_wl.get(&id).copied(),
            Iface::XdgToplevel | Iface::XdgPopupShim => self
                .top_to_xdg
                .get(&id)
                .and_then(|xdg_id| self.xdg_to_wl.get(xdg_id))
                .copied(),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A connection that already knows one surface and one window object.
    fn connected() -> WaylandConn {
        let mut conn = WaylandConn::new();
        conn.ifaces.insert(10, Iface::WlSurface);
        conn.ifaces.insert(20, Iface::XdgSurface);
        conn.ifaces.insert(30, Iface::XdgToplevel);
        conn.xdg_to_wl.insert(20, 10);
        conn.top_to_xdg.insert(30, 20);
        conn.wl_to_top.insert(10, 30);
        conn
    }

    #[test]
    fn a_new_connection_only_knows_the_display() {
        let conn = WaylandConn::new();

        assert_eq!(conn.ifaces.len(), 1);
        assert_eq!(conn.ifaces.get(&1), Some(&Iface::WlDisplay));
        assert!(conn.stolen_ids.is_empty());
        assert!(conn.injected_ids.is_empty());
    }

    #[test]
    fn resets_keep_the_display_but_drop_everything_else() {
        let mut conn = connected();
        conn.stolen_ids.push(99);
        conn.injected_ids.insert(99);

        conn.reset_tracking();

        assert_eq!(conn.ifaces.len(), 1);
        assert!(conn.stolen_ids.is_empty());
        assert!(conn.injected_ids.is_empty());
        assert!(conn.top_to_xdg.is_empty());
    }

    #[test]
    fn injected_ids_are_recycled_from_the_stolen_pool() {
        let mut conn = WaylandConn::new();
        conn.stolen_ids.extend([7, 8]);

        assert_eq!(conn.alloc_injected_id(), Some(8), "last in, first out");
        assert_eq!(conn.alloc_injected_id(), Some(7));
        assert_eq!(conn.alloc_injected_id(), None, "the pool is empty");
        assert!(conn.injected_ids.contains(&7) && conn.injected_ids.contains(&8));
        assert!(conn.stolen_ids.is_empty());
    }

    #[test]
    fn purging_a_toplevel_clears_its_mappings() {
        let mut conn = connected();

        conn.purge(30);

        assert!(!conn.ifaces.contains_key(&30));
        assert!(!conn.top_to_xdg.contains_key(&30));
        assert!(!conn.wl_to_top.contains_key(&10));
        // The xdg surface it belonged to is untouched.
        assert!(conn.ifaces.contains_key(&20));
    }

    #[test]
    fn purging_an_xdg_surface_takes_its_toplevel_with_it() {
        let mut conn = connected();

        conn.purge(20);

        assert!(!conn.ifaces.contains_key(&20));
        assert!(!conn.ifaces.contains_key(&30), "toplevel is purged too");
        assert!(!conn.xdg_to_wl.contains_key(&20));
        assert!(!conn.top_to_xdg.contains_key(&30));
    }

    #[test]
    fn purging_a_surface_forgets_its_focus_and_toplevel() {
        let mut conn = connected();
        conn.pointer_focus.insert(40, 10);

        conn.purge(10);

        assert!(!conn.ifaces.contains_key(&10));
        assert!(!conn.wl_to_top.contains_key(&10));
        assert!(!conn.pointer_focus.contains_key(&40));
        // Mappings that pointed at the dead surface are dropped, so the xdg
        // surface no longer references it.
        assert!(!conn.xdg_to_wl.contains_key(&20));
        assert!(!conn.xdg_to_wl.values().any(|surface| *surface == 10));
    }

    #[test]
    fn purging_a_seat_forgets_the_devices_it_owned() {
        let mut conn = WaylandConn::new();
        conn.ifaces.insert(5, Iface::WlSeat);
        conn.pointer_seat.insert(6, 5);
        conn.touch_seat.insert(7, 5);
        conn.ifaces.insert(6, Iface::WlPointer);
        conn.ifaces.insert(7, Iface::WlTouch);

        conn.purge(5);

        assert!(conn.pointer_seat.is_empty());
        assert!(conn.touch_seat.is_empty());
    }

    #[test]
    fn purging_a_pointer_forgets_its_focus() {
        let mut conn = WaylandConn::new();
        conn.ifaces.insert(6, Iface::WlPointer);
        conn.pointer_focus.insert(6, 10);
        conn.pointer_seat.insert(6, 5);

        conn.purge(6);

        assert!(conn.pointer_focus.is_empty());
        assert!(conn.pointer_seat.is_empty());
    }

    #[test]
    fn window_objects_resolve_to_their_wl_surface() {
        let conn = connected();

        assert_eq!(
            conn.wl_surface_for_window_object(10, Iface::WlSurface),
            Some(10)
        );
        assert_eq!(
            conn.wl_surface_for_window_object(20, Iface::XdgSurface),
            Some(10)
        );
        assert_eq!(
            conn.wl_surface_for_window_object(30, Iface::XdgToplevel),
            Some(10)
        );
        assert_eq!(conn.wl_surface_for_window_object(1, Iface::WlDisplay), None);
        assert_eq!(
            conn.wl_surface_for_window_object(99, Iface::WlSurface),
            Some(99)
        );
    }

    #[test]
    fn pending_popup_consumed_only_by_noted_surface() {
        init_state();
        let fd = 93_001;
        PENDING_POPUPS.get().unwrap().lock().unwrap().insert(
            fd,
            PendingPopup {
                token: 7,
                parent_xdg_surface_id: 20,
                width: 10,
                height: 10,
                anchor_x: 0,
                anchor_y: 0,
                positioner_id: 30,
                xdg_surface_id: None,
            },
        );
        // An unrelated window's toplevel must not steal the reservation.
        assert!(take_pending_popup(fd, 41).is_none());
        // The latest noted surface wins: a foreign surface mapping first
        // (e.g. at startup) must not steal the menu's reservation.
        note_popup_surface(fd, 42);
        note_popup_surface(fd, 43);
        assert!(take_pending_popup(fd, 42).is_none());
        let popup = take_pending_popup(fd, 43).expect("recorded surface consumes");
        assert_eq!(popup.token, 7);
        assert!(take_pending_popup(fd, 43).is_none());
        PENDING_POPUPS.get().unwrap().lock().unwrap().remove(&fd);
    }
}

// ── Global state ───────────────────────────────────────────────────────────

pub(crate) static IS_WAYLAND: OnceLock<bool> = OnceLock::new();
pub(crate) static CONNS: OnceLock<Mutex<HashMap<RawFd, WaylandConn>>> = OnceLock::new();
#[allow(clippy::type_complexity)]
#[derive(Clone, Copy)]
pub(crate) struct LastButton {
    pub(crate) fd: RawFd,
    pub(crate) seat_id: u32,
    pub(crate) serial: u32,
    pub(crate) wl_surface_id: u32,
    pub(crate) x: i32,
    pub(crate) y: i32,
}

#[derive(Default)]
pub(crate) struct LastButtonState {
    pub(crate) by_surface: HashMap<(RawFd, u32), LastButton>,
    pub(crate) latest: Option<(RawFd, u32)>,
}

pub(crate) struct PendingPopup {
    pub(crate) token: u32,
    pub(crate) parent_xdg_surface_id: u32,
    pub(crate) width: i32,
    pub(crate) height: i32,
    pub(crate) anchor_x: i32,
    pub(crate) anchor_y: i32,
    pub(crate) positioner_id: u32,
    /// The xdg_surface the popup will be created for, recorded when it
    /// appears. A reservation is only consumed by a get_toplevel for this
    /// exact surface, so a racing unrelated window can never steal it.
    pub(crate) xdg_surface_id: Option<u32>,
}

pub(crate) static LAST_BUTTON: OnceLock<Mutex<LastButtonState>> = OnceLock::new();
pub(crate) static PENDING_POPUPS: OnceLock<Mutex<HashMap<RawFd, PendingPopup>>> = OnceLock::new();
static NEXT_POPUP_TOKEN: AtomicU32 = AtomicU32::new(1);
pub(crate) type PointerAxisCb = Box<dyn FnOnce(Option<u32>) + Send>;
pub(crate) type PointerAxisWatcherKey = (RawFd, u32);
pub(crate) struct PointerAxisWatcher {
    pub(crate) token: u32,
    pub(crate) callback: PointerAxisCb,
}
pub(crate) static NEXT_POINTER_AXIS: OnceLock<
    Mutex<HashMap<PointerAxisWatcherKey, Vec<PointerAxisWatcher>>>,
> = OnceLock::new();
static NEXT_POINTER_AXIS_TOKEN: AtomicU32 = AtomicU32::new(1);
pub(crate) static RX_BUFS: OnceLock<Mutex<HashMap<RawFd, Vec<u8>>>> = OnceLock::new();
pub(crate) static TX_BUFS: OnceLock<Mutex<HashMap<RawFd, Vec<u8>>>> = OnceLock::new();

#[derive(Default)]
pub(crate) struct PendingControl {
    pub(crate) bytes: Vec<u8>,
    pub(crate) fds: Vec<RawFd>,
}

pub(crate) fn close_pending_control(pending: PendingControl) {
    for fd in pending.fds {
        super::super::proxy::syscalls::call_close(fd);
    }
}

// Pending control data (SCM_RIGHTS) stored alongside incomplete messages.
// Control data is semantically attached to a specific Wayland message on the same
// recvmsg boundary, so it must not be forwarded without the complete message.
pub(crate) static RX_PENDING_CTRL: OnceLock<Mutex<HashMap<RawFd, PendingControl>>> =
    OnceLock::new();
pub(crate) static TX_PENDING_CTRL: OnceLock<Mutex<HashMap<RawFd, PendingControl>>> =
    OnceLock::new();

// Custom window ID map tracking user-assigned IDs via setTitle("\u{200B}\u{200C}<id>")
pub(crate) static CUSTOM_ID_MAP: OnceLock<Mutex<HashMap<String, (RawFd, u32)>>> = OnceLock::new();

pub(crate) type CursorEnterCb = Box<dyn FnOnce(Option<(i32, i32)>) + Send>;
pub(crate) struct CursorEnterWatcher {
    pub(crate) token: u32,
    pub(crate) callback: CursorEnterCb,
}
pub(crate) type CursorEnterWatcherKey = (RawFd, u32);
pub(crate) type CursorEnterWatcherMap = HashMap<CursorEnterWatcherKey, Vec<CursorEnterWatcher>>;
pub(crate) static NEXT_TOPLEVEL_CURSOR_ENTER: OnceLock<Mutex<Vec<CursorEnterWatcher>>> =
    OnceLock::new();
pub(crate) static CURSOR_ENTER_WATCHERS: OnceLock<Mutex<CursorEnterWatcherMap>> = OnceLock::new();
static NEXT_CURSOR_ENTER_TOKEN: AtomicU32 = AtomicU32::new(1);

fn next_unused_token(counter: &AtomicU32, mut is_used: impl FnMut(u32) -> bool) -> u32 {
    loop {
        let token = counter.fetch_add(1, Ordering::Relaxed);
        if token != 0 && !is_used(token) {
            return token;
        }
    }
}

// ── Cursor enter watchers ─────────────────────────────────────────────────

pub(crate) fn arm_first_cursor_enter_watchers(fd: RawFd, wl_surface_id: u32) {
    let Some(pending) = NEXT_TOPLEVEL_CURSOR_ENTER.get() else {
        return;
    };
    let Ok(mut pending) = pending.lock() else {
        return;
    };
    if pending.is_empty() {
        return;
    }
    let mut callbacks: Vec<_> = pending.drain(..).collect();
    // NOTE: the pending lock is intentionally held across the insert below.
    // Every other path takes these two locks in the same order (pending,
    // then watchers) or one at a time, so this cannot deadlock — and it
    // closes the gap where a concurrent cancellation could miss a watcher
    // between drain and insert.
    if let Some(watchers) = CURSOR_ENTER_WATCHERS.get()
        && let Ok(mut watchers) = watchers.lock()
    {
        watchers
            .entry((fd, wl_surface_id))
            .or_default()
            .append(&mut callbacks);
        return;
    }
    drop(pending);
    for watcher in callbacks {
        (watcher.callback)(None);
    }
}

pub(crate) fn watch_next_toplevel_cursor_enter(callback: CursorEnterCb) -> Option<u32> {
    let Some(pending) = NEXT_TOPLEVEL_CURSOR_ENTER.get() else {
        callback(None);
        return None;
    };
    let Ok(mut pending) = pending.lock() else {
        callback(None);
        return None;
    };
    let token = next_unused_token(&NEXT_CURSOR_ENTER_TOKEN, |candidate| {
        pending.iter().any(|watcher| watcher.token == candidate)
            || CURSOR_ENTER_WATCHERS
                .get()
                .and_then(|watchers| watchers.lock().ok())
                .is_some_and(|watchers| {
                    watchers
                        .values()
                        .flatten()
                        .any(|watcher| watcher.token == candidate)
                })
    });
    pending.push(CursorEnterWatcher { token, callback });
    Some(token)
}

pub(crate) fn cancel_cursor_enter_watcher(token: u32) -> bool {
    let pending_watcher = NEXT_TOPLEVEL_CURSOR_ENTER.get().and_then(|pending| {
        let mut pending = pending.lock().ok()?;
        let index = pending.iter().position(|watcher| watcher.token == token)?;
        Some(pending.remove(index))
    });
    if let Some(watcher) = pending_watcher {
        (watcher.callback)(None);
        return true;
    }

    let armed_watcher = CURSOR_ENTER_WATCHERS.get().and_then(|watchers| {
        let mut watchers = watchers.lock().ok()?;
        let mut removed = None;
        watchers.retain(|_, entries| {
            if removed.is_none()
                && let Some(index) = entries.iter().position(|watcher| watcher.token == token)
            {
                removed = Some(entries.remove(index));
            }
            !entries.is_empty()
        });
        removed
    });
    if let Some(watcher) = armed_watcher {
        (watcher.callback)(None);
        true
    } else {
        false
    }
}

pub(crate) fn fire_first_cursor_enter_watchers(fd: RawFd, wl_surface_id: u32, x: i32, y: i32) {
    let Some(watchers) = CURSOR_ENTER_WATCHERS.get() else {
        return;
    };
    let callbacks = {
        let Ok(mut watchers) = watchers.lock() else {
            return;
        };
        watchers.remove(&(fd, wl_surface_id))
    };
    if let Some(watchers) = callbacks {
        for watcher in watchers {
            (watcher.callback)(Some((x, y)));
        }
    }
}

pub(crate) fn clear_first_cursor_enter_watchers_for_fd(fd: RawFd) {
    let removed = CURSOR_ENTER_WATCHERS.get().and_then(|watchers| {
        let mut watchers = watchers.lock().ok()?;
        let mut removed = Vec::new();
        watchers.retain(|(watch_fd, _), entries| {
            if *watch_fd == fd {
                removed.append(entries);
                false
            } else {
                true
            }
        });
        Some(removed)
    });
    if let Some(watchers) = removed {
        for watcher in watchers {
            (watcher.callback)(None);
        }
    }
}

pub(crate) fn clear_first_cursor_enter_watchers_for_surface(fd: RawFd, wl_surface_id: u32) {
    let watchers = CURSOR_ENTER_WATCHERS
        .get()
        .and_then(|watchers| watchers.lock().ok()?.remove(&(fd, wl_surface_id)));
    if let Some(watchers) = watchers {
        for watcher in watchers {
            (watcher.callback)(None);
        }
    }
}

pub(crate) fn clear_last_button_for_surface(fd: RawFd, wl_surface_id: u32) {
    if let Some(last_button) = LAST_BUTTON.get()
        && let Ok(mut last_button) = last_button.lock()
    {
        last_button.by_surface.remove(&(fd, wl_surface_id));
        if last_button.latest == Some((fd, wl_surface_id)) {
            last_button.latest = None;
        }
    }
}

pub(crate) fn arm_next_popup(
    parent_window_id: &str,
    width: i32,
    height: i32,
    anchor: Option<(i32, i32)>,
) -> Option<u32> {
    if width <= 0 || height <= 0 || width > MAX_POPUP_DIMENSION || height > MAX_POPUP_DIMENSION {
        return None;
    }
    let parent_mapping = CUSTOM_ID_MAP
        .get()
        .and_then(|map| map.lock().ok()?.get(parent_window_id).copied());
    let (fd, parent_wl_surface_id) = parent_mapping?;
    let parent_xdg_surface_id = CONNS
        .get()
        .and_then(|conns| conns.lock().ok())
        .and_then(|conns| {
            conns
                .get(&fd)?
                .xdg_to_wl
                .iter()
                .find_map(|(xdg, wl)| (*wl == parent_wl_surface_id).then_some(*xdg))
        });
    let parent_xdg_surface_id = parent_xdg_surface_id?;

    let (anchor_x, anchor_y) = if let Some((x, y)) = anchor {
        (x, y)
    } else {
        let button = LAST_BUTTON
            .get()
            .and_then(|v| v.lock().ok())
            .and_then(|v| v.by_surface.get(&(fd, parent_wl_surface_id)).copied());
        let button = button?;
        (button.x, button.y)
    };

    let pending = PENDING_POPUPS.get()?;
    let already_pending = pending
        .lock()
        .ok()
        .is_some_and(|pending| pending.contains_key(&fd));
    if already_pending {
        return None;
    }

    let positioner_id = CONNS
        .get()
        .and_then(|conns| conns.lock().ok())
        .and_then(|mut conns| conns.get_mut(&fd)?.alloc_injected_id());
    let positioner_id = positioner_id?;

    let Ok(mut pending) = pending.lock() else {
        if let Some(conns) = CONNS.get()
            && let Ok(mut conns) = conns.lock()
            && let Some(conn) = conns.get_mut(&fd)
        {
            conn.injected_ids.remove(&positioner_id);
            conn.stolen_ids.push(positioner_id);
        }
        return None;
    };
    if pending.contains_key(&fd) {
        drop(pending);
        if let Some(conns) = CONNS.get()
            && let Ok(mut conns) = conns.lock()
            && let Some(conn) = conns.get_mut(&fd)
            && conn.injected_ids.remove(&positioner_id)
        {
            conn.stolen_ids.push(positioner_id);
        }
        return None;
    }
    let token = next_unused_token(&NEXT_POPUP_TOKEN, |candidate| {
        pending.values().any(|popup| popup.token == candidate)
    });
    pending.insert(
        fd,
        PendingPopup {
            token,
            parent_xdg_surface_id,
            width,
            height,
            anchor_x,
            anchor_y,
            positioner_id,
            xdg_surface_id: None,
        },
    );
    Some(token)
}

pub(crate) fn window_is_popup(window_id: &str) -> bool {
    let Some((fd, wl_surface_id)) = CUSTOM_ID_MAP
        .get()
        .and_then(|map| map.lock().ok()?.get(window_id).copied())
    else {
        return false;
    };
    let details = CONNS
        .get()
        .and_then(|conns| conns.lock().ok())
        .and_then(|conns| {
            let conn = conns.get(&fd)?;
            let top_id = conn.wl_to_top.get(&wl_surface_id)?;
            Some((*top_id, conn.ifaces.get(top_id).copied()))
        });
    details.is_some_and(|(_, iface)| iface == Some(Iface::XdgPopupShim))
}

pub(crate) fn cancel_pending_popup(token: u32) -> bool {
    let Some(pending) = PENDING_POPUPS.get() else {
        return false;
    };
    let (fd, popup) = {
        let Ok(mut pending) = pending.lock() else {
            return false;
        };
        let Some(fd) = pending
            .iter()
            .find_map(|(fd, popup)| (popup.token == token).then_some(*fd))
        else {
            return false;
        };
        let Some(popup) = pending.remove(&fd) else {
            return false;
        };
        (fd, popup)
    };
    if let Some(conns) = CONNS.get()
        && let Ok(mut conns) = conns.lock()
        && let Some(conn) = conns.get_mut(&fd)
        && conn.injected_ids.remove(&popup.positioner_id)
    {
        conn.stolen_ids.push(popup.positioner_id);
    }
    true
}

pub(crate) fn take_pending_popup(fd: RawFd, xdg_surface_id: u32) -> Option<PendingPopup> {
    let mut pending = PENDING_POPUPS.get()?.lock().ok()?;
    // Only the xdg_surface recorded for this reservation may consume it.
    // Anything else (e.g. an unrelated window created in between) is left
    // for the normal toplevel path; the reservation stays until expiry.
    if pending
        .get(&fd)
        .is_some_and(|popup| popup.xdg_surface_id == Some(xdg_surface_id))
    {
        pending.remove(&fd)
    } else {
        None
    }
}

/// Remember which xdg_surface a pending popup reservation belongs to.
/// Called when an xdg_surface appears while armed; the LATEST surface wins:
/// the menu surface is always created after arming (arm -> create -> show),
/// so an earlier surface is necessarily foreign (e.g. a startup window
/// mapping concurrently) and must not steal the reservation.
pub(crate) fn note_popup_surface(fd: RawFd, xdg_surface_id: u32) {
    let Some(pending) = PENDING_POPUPS.get() else {
        return;
    };
    let Ok(mut pending) = pending.lock() else {
        return;
    };
    if let Some(popup) = pending.get_mut(&fd) {
        popup.xdg_surface_id = Some(xdg_surface_id);
    }
}

pub(crate) fn cancel_pending_popup_for_parent(
    fd: RawFd,
    parent_xdg_surface_id: u32,
    conn: &mut WaylandConn,
) {
    let popup = PENDING_POPUPS.get().and_then(|pending| {
        let mut pending = pending.lock().ok()?;
        if pending
            .get(&fd)
            .is_some_and(|popup| popup.parent_xdg_surface_id == parent_xdg_surface_id)
        {
            pending.remove(&fd)
        } else {
            None
        }
    });
    if let Some(popup) = popup
        && conn.injected_ids.remove(&popup.positioner_id)
    {
        conn.stolen_ids.push(popup.positioner_id);
    }
}

pub(crate) fn cancel_pending_popup_for_connection(fd: RawFd, conn: &mut WaylandConn) {
    let popup = PENDING_POPUPS
        .get()
        .and_then(|pending| pending.lock().ok()?.remove(&fd));
    if let Some(popup) = popup
        && conn.injected_ids.remove(&popup.positioner_id)
    {
        conn.stolen_ids.push(popup.positioner_id);
    }
}

pub(crate) fn watch_next_pointer_axis(window_id: &str, callback: PointerAxisCb) -> Option<u32> {
    let Some((fd, wl_surface_id)) = CUSTOM_ID_MAP
        .get()
        .and_then(|map| map.lock().ok()?.get(window_id).copied())
    else {
        callback(None);
        return None;
    };
    let Some(watchers) = NEXT_POINTER_AXIS.get() else {
        callback(None);
        return None;
    };
    let Ok(mut watchers) = watchers.lock() else {
        callback(None);
        return None;
    };
    let token = next_unused_token(&NEXT_POINTER_AXIS_TOKEN, |candidate| {
        watchers
            .values()
            .flatten()
            .any(|watcher| watcher.token == candidate)
    });
    watchers
        .entry((fd, wl_surface_id))
        .or_default()
        .push(PointerAxisWatcher { token, callback });
    Some(token)
}

pub(crate) fn cancel_pointer_axis_watcher(token: u32) -> bool {
    let Some(watchers) = NEXT_POINTER_AXIS.get() else {
        return false;
    };
    let watcher = {
        let Ok(mut watchers) = watchers.lock() else {
            return false;
        };
        let mut removed = None;
        watchers.retain(|_, entries| {
            if removed.is_none()
                && let Some(index) = entries.iter().position(|entry| entry.token == token)
            {
                removed = Some(entries.remove(index));
            }
            !entries.is_empty()
        });
        removed
    };
    if let Some(watcher) = watcher {
        (watcher.callback)(None);
        true
    } else {
        false
    }
}

pub(crate) fn fire_next_pointer_axis(fd: RawFd, wl_surface_id: u32, axis: u32) {
    let watchers = NEXT_POINTER_AXIS
        .get()
        .and_then(|watchers| watchers.lock().ok()?.remove(&(fd, wl_surface_id)));
    if let Some(watchers) = watchers {
        for watcher in watchers {
            (watcher.callback)(Some(axis));
        }
    }
}

pub(crate) fn clear_pointer_axis_watchers_for_surface(fd: RawFd, wl_surface_id: u32) {
    let watchers = NEXT_POINTER_AXIS
        .get()
        .and_then(|watchers| watchers.lock().ok()?.remove(&(fd, wl_surface_id)));
    if let Some(watchers) = watchers {
        for watcher in watchers {
            (watcher.callback)(None);
        }
    }
}

pub(crate) fn clear_runtime_state_for_fd(fd: RawFd) {
    if let Some(m) = LAST_BUTTON.get()
        && let Ok(mut buttons) = m.lock()
    {
        buttons
            .by_surface
            .retain(|(button_fd, _), _| *button_fd != fd);
        if buttons.latest.is_some_and(|(button_fd, _)| button_fd == fd) {
            buttons.latest = None;
        }
    }
    if let Some(m) = CUSTOM_ID_MAP.get()
        && let Ok(mut map) = m.lock()
    {
        map.retain(|_, value| value.0 != fd);
    }
    let pending_popup = PENDING_POPUPS
        .get()
        .and_then(|pending| pending.lock().ok()?.remove(&fd));
    if let Some(popup) = pending_popup
        && let Some(conns) = CONNS.get()
        && let Ok(mut conns) = conns.lock()
        && let Some(conn) = conns.get_mut(&fd)
        && conn.injected_ids.remove(&popup.positioner_id)
    {
        conn.stolen_ids.push(popup.positioner_id);
    }
    let axis_watchers = NEXT_POINTER_AXIS.get().and_then(|m| {
        let mut watchers = m.lock().ok()?;
        let mut removed = Vec::new();
        watchers.retain(|(watch_fd, _), entries| {
            if *watch_fd == fd {
                removed.append(entries);
                false
            } else {
                true
            }
        });
        Some(removed)
    });
    if let Some(watchers) = axis_watchers {
        for watcher in watchers {
            (watcher.callback)(None);
        }
    }
    clear_first_cursor_enter_watchers_for_fd(fd);
}

pub(crate) fn clear_stream_state_for_fd(fd: RawFd) {
    if let Some(m) = RX_BUFS.get() {
        let _ = m.lock().map(|mut buffers| buffers.remove(&fd));
    }
    if let Some(m) = TX_BUFS.get() {
        let _ = m.lock().map(|mut buffers| buffers.remove(&fd));
    }
    for controls in [&RX_PENDING_CTRL, &TX_PENDING_CTRL] {
        if let Some(controls) = controls.get()
            && let Ok(mut controls) = controls.lock()
            && let Some(pending) = controls.remove(&fd)
        {
            close_pending_control(pending);
        }
    }
}

// ── Lifecycle ──────────────────────────────────────────────────────────────

pub(crate) fn on_close(fd: RawFd) {
    if let Some(m) = CONNS.get()
        && let Ok(mut map) = m.lock()
    {
        map.remove(&fd);
    }
    clear_runtime_state_for_fd(fd);
    clear_stream_state_for_fd(fd);
}

pub(crate) fn is_wayland() -> bool {
    *IS_WAYLAND.get().unwrap_or(&false)
}

pub(crate) fn init_state() {
    CONNS.get_or_init(|| Mutex::new(HashMap::new()));
    LAST_BUTTON.get_or_init(|| Mutex::new(LastButtonState::default()));
    PENDING_POPUPS.get_or_init(|| Mutex::new(HashMap::new()));
    NEXT_POINTER_AXIS.get_or_init(|| Mutex::new(HashMap::new()));
    CUSTOM_ID_MAP.get_or_init(|| Mutex::new(HashMap::new()));
    RX_BUFS.get_or_init(|| Mutex::new(HashMap::new()));
    TX_BUFS.get_or_init(|| Mutex::new(HashMap::new()));
    RX_PENDING_CTRL.get_or_init(|| Mutex::new(HashMap::new()));
    TX_PENDING_CTRL.get_or_init(|| Mutex::new(HashMap::new()));
    NEXT_TOPLEVEL_CURSOR_ENTER.get_or_init(|| Mutex::new(Vec::new()));
    CURSOR_ENTER_WATCHERS.get_or_init(|| Mutex::new(HashMap::new()));
}

pub(crate) fn clear_state() {
    if let Some(m) = CONNS.get()
        && let Ok(mut map) = m.lock()
    {
        map.clear();
    }
    if let Some(m) = LAST_BUTTON.get()
        && let Ok(mut buttons) = m.lock()
    {
        buttons.by_surface.clear();
        buttons.latest = None;
    }
    if let Some(m) = PENDING_POPUPS.get()
        && let Ok(mut pending) = m.lock()
    {
        pending.clear();
    }
    let axis_watchers = NEXT_POINTER_AXIS.get().and_then(|m| {
        let mut watchers = m.lock().ok()?;
        Some(
            watchers
                .drain()
                .flat_map(|(_, entries)| entries)
                .collect::<Vec<_>>(),
        )
    });
    if let Some(watchers) = axis_watchers {
        for watcher in watchers {
            (watcher.callback)(None);
        }
    }
    if let Some(m) = CUSTOM_ID_MAP.get()
        && let Ok(mut map) = m.lock()
    {
        map.clear();
    }
    if let Some(m) = RX_BUFS.get()
        && let Ok(mut map) = m.lock()
    {
        map.clear();
    }
    if let Some(m) = TX_BUFS.get()
        && let Ok(mut map) = m.lock()
    {
        map.clear();
    }
    if let Some(m) = RX_PENDING_CTRL.get()
        && let Ok(mut map) = m.lock()
    {
        for pending in map.drain().map(|(_, pending)| pending) {
            close_pending_control(pending);
        }
    }
    if let Some(m) = TX_PENDING_CTRL.get()
        && let Ok(mut map) = m.lock()
    {
        for pending in map.drain().map(|(_, pending)| pending) {
            close_pending_control(pending);
        }
    }
    let pending_cursor_watchers = NEXT_TOPLEVEL_CURSOR_ENTER
        .get()
        .and_then(|pending| Some(pending.lock().ok()?.drain(..).collect::<Vec<_>>()));
    if let Some(watchers) = pending_cursor_watchers {
        for watcher in watchers {
            (watcher.callback)(None);
        }
    }
    let cursor_watchers = CURSOR_ENTER_WATCHERS.get().and_then(|watchers| {
        Some(
            watchers
                .lock()
                .ok()?
                .drain()
                .flat_map(|(_, entries)| entries)
                .collect::<Vec<_>>(),
        )
    });
    if let Some(watchers) = cursor_watchers {
        for watcher in watchers {
            (watcher.callback)(None);
        }
    }
}

#[cfg(test)]
mod tests_extra {
    use std::sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    };

    use super::*;

    #[test]
    fn token_allocation_skips_zero_and_live_tokens_after_wrap() {
        let counter = AtomicU32::new(u32::MAX);
        let token = next_unused_token(&counter, |candidate| candidate == u32::MAX);
        assert_eq!(token, 1);
    }

    #[test]
    fn runtime_state_cleanup_is_connection_and_surface_scoped() {
        init_state();
        let fd = 91_001;
        let other_fd = 91_002;
        clear_runtime_state_for_fd(fd);
        clear_runtime_state_for_fd(other_fd);

        PENDING_POPUPS.get().unwrap().lock().unwrap().insert(
            fd,
            PendingPopup {
                token: 1,
                parent_xdg_surface_id: 20,
                width: 10,
                height: 10,
                anchor_x: 0,
                anchor_y: 0,
                positioner_id: 30,
                xdg_surface_id: None,
            },
        );
        CUSTOM_ID_MAP
            .get()
            .unwrap()
            .lock()
            .unwrap()
            .insert("test-window".into(), (fd, 10));
        let mut last_buttons = LAST_BUTTON.get().unwrap().lock().unwrap();
        last_buttons.by_surface.insert(
            (fd, 10),
            LastButton {
                fd,
                seat_id: 1,
                serial: 2,
                wl_surface_id: 10,
                x: 3,
                y: 4,
            },
        );
        last_buttons.latest = Some((fd, 10));
        drop(last_buttons);

        let cancelled = Arc::new(AtomicU32::new(0));
        let cancelled_by_cleanup = Arc::clone(&cancelled);
        let retained = Arc::new(AtomicU32::new(0));
        let retained_for_other_surface = Arc::clone(&retained);
        let mut axis_watchers = NEXT_POINTER_AXIS.get().unwrap().lock().unwrap();
        axis_watchers.insert(
            (fd, 10),
            vec![PointerAxisWatcher {
                token: 2,
                callback: Box::new(move |axis| {
                    assert_eq!(axis, None);
                    cancelled_by_cleanup.fetch_add(1, Ordering::Relaxed);
                }),
            }],
        );
        axis_watchers.insert(
            (other_fd, 11),
            vec![PointerAxisWatcher {
                token: 3,
                callback: Box::new(move |axis| {
                    assert_eq!(axis, Some(1));
                    retained_for_other_surface.fetch_add(1, Ordering::Relaxed);
                }),
            }],
        );
        drop(axis_watchers);

        clear_runtime_state_for_fd(fd);
        assert_eq!(cancelled.load(Ordering::Relaxed), 1);
        assert!(
            !PENDING_POPUPS
                .get()
                .unwrap()
                .lock()
                .unwrap()
                .contains_key(&fd)
        );
        assert!(
            !CUSTOM_ID_MAP
                .get()
                .unwrap()
                .lock()
                .unwrap()
                .contains_key("test-window")
        );
        let last_buttons = LAST_BUTTON.get().unwrap().lock().unwrap();
        assert!(last_buttons.by_surface.is_empty());
        assert!(last_buttons.latest.is_none());

        fire_next_pointer_axis(other_fd, 11, 1);
        assert_eq!(retained.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn purging_surface_clears_pointer_focus_and_position() {
        let mut conn = WaylandConn::new();
        conn.ifaces.insert(10, Iface::WlSurface);
        conn.pointer_focus.insert(20, 10);
        conn.pointer_position.insert(20, (30, 40));

        conn.purge(10);

        assert!(!conn.pointer_focus.contains_key(&20));
        assert!(!conn.pointer_position.contains_key(&20));
    }

    #[test]
    fn cancelling_parent_popup_recycles_reserved_id() {
        init_state();
        let fd = 92_001;
        clear_runtime_state_for_fd(fd);
        let mut conn = WaylandConn::new();
        conn.injected_ids.insert(40);
        PENDING_POPUPS.get().unwrap().lock().unwrap().insert(
            fd,
            PendingPopup {
                token: 4,
                parent_xdg_surface_id: 21,
                width: 10,
                height: 10,
                anchor_x: 0,
                anchor_y: 0,
                positioner_id: 40,
                xdg_surface_id: None,
            },
        );

        cancel_pending_popup_for_parent(fd, 21, &mut conn);

        assert!(
            !PENDING_POPUPS
                .get()
                .unwrap()
                .lock()
                .unwrap()
                .contains_key(&fd)
        );
        assert!(!conn.injected_ids.contains(&40));
        assert_eq!(conn.stolen_ids, vec![40]);
    }
}

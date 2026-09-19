//! Server-side decoration requests for popup surfaces.
//!
//! Requesting an `xdg_toplevel` decoration for a surface that is really an
//! `xdg_popup` is a protocol violation. Strict compositors (e.g. niri) reject
//! it with a protocol error and kill the client, taking the whole app down.
//! Chromium falls back to client-side decorations when no configure reply
//! arrives — the same as if the decoration manager were absent — so dropping
//! the request for popups is safe. Regular toplevels are forwarded untouched.
//!
//! NOTE: despite the name, the `surface` argument of get_toplevel_decoration
//! is an xdg_toplevel object id, not a wl_surface id.

use std::os::fd::RawFd;

use super::super::super::is_gnome_desktop;
use super::super::codec::{Iface, WlMessage};
use super::super::state::WaylandConn;
use super::Action;

pub(crate) fn on_get_toplevel_decoration(
    _fd: RawFd,
    conn: &mut WaylandConn,
    msg: &WlMessage,
) -> Action {
    let (Some(decoration_id), Some(top_id)) = (msg.u32_arg(8), msg.u32_arg(12)) else {
        return Action::Forward;
    };
    conn.ifaces
        .insert(decoration_id, Iface::ZxdgToplevelDecoration);

    let is_popup = conn.ifaces.get(&top_id) == Some(&Iface::XdgPopupShim);
    // GNOME keeps today's behavior byte-for-byte; only strict compositors
    // need the suppression.
    if is_popup && !is_gnome_desktop() {
        // Retag so later messages to this never-created object (set_mode,
        // unset_mode, destroy) are swallowed as well.
        conn.ifaces
            .insert(decoration_id, Iface::ZxdgToplevelDecorationSwallowed);
        return Action::Suppress;
    }
    Action::Forward
}

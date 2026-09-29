//! Music video on Linux, drawn by mpv underneath the webview instead of by a `<video>` inside it.
//!
//! The `<video>` element could only ever be *kept in step* with mpv, which plays the audio: two
//! clocks, reconciled from JavaScript, and on WebKitGTK every correction is expensive (a
//! `playbackRate` write is a flushing seek, a seek costs about a second of re-buffering), so a
//! scrub could leave the picture 10-15 s out (#321). Here mpv plays the picture as one more track
//! of the file it is already playing (crates/player/src/video.rs) and draws it into a GtkGLArea
//! placed *under* the webview. The webview is transparent already (the window is, so the page can
//! round its own corners); the page leaves a hole where the picture goes and tells us where that is
//! (`set_rect`).
//!
//! The widget tree, rebuilt once at startup around the one wry made:
//!
//! ```text
//! GtkBox (Tauri's)
//! └── GtkOverlay
//!     ├── GtkBox          main child, empty: it only gives the overlay its size
//!     ├── GtkGLArea       overlay child, placed at the hole by `get-child-position`
//!     └── WebKitWebView   overlay child on top, filling the overlay
//! ```
//!
//! Overlay children add nothing to the overlay's size request, so the picture's box can never hold
//! the window open at a minimum size the way a GtkFixed child would.

use std::cell::{Cell, RefCell};
use std::ffi::{c_char, c_void, CString};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use gtk::glib;
use gtk::prelude::*;
use player::{GlDisplay, VideoRenderer};
use webkit2gtk::WebViewExt;

use crate::state::AppState;

/// The widget tree is in place. Cleared again if the GL context cannot be made, which sends the
/// page back to the `<video>` element.
static AVAILABLE: AtomicBool = AtomicBool::new(false);

pub fn available() -> bool {
    AVAILABLE.load(Ordering::Relaxed)
}

/// `(x, y, w, h)` in GTK logical pixels.
type Place = (i32, i32, i32, i32);

struct Surface {
    overlay: gtk::Overlay,
    area: gtk::GLArea,
    webview: webkit2gtk::WebView,
    renderer: Rc<RefCell<Option<VideoRenderer>>>,
    /// Where the GLArea goes.
    at: Rc<Cell<Option<Place>>>,
    /// The pending [`IDLE_GRACE`] release, while the picture is down.
    release: Cell<Option<glib::SourceId>>,
}

/// How long the GL surface outlives the picture going down. The GL context and mpv's render
/// contexts are not free: measured on Fedora/NVIDIA, ~490 MiB of GPU memory in this process,
/// most of it the CUDA context NVDEC decodes in, and none of it goes when mpv merely stops
/// decoding. Past this the area is unrealized, which frees all of it; the next picture realizes it
/// again. Short absences (closing the view to pick a song, the mini player) keep it.
/// ponytail: the same flat minute as the `<video>` path's IDLE_GRACE (VideoSurface.svelte).
const IDLE_GRACE: std::time::Duration = std::time::Duration::from_secs(60);

thread_local! {
    /// GTK main thread only, like every widget in it.
    static SURFACE: RefCell<Option<Surface>> = const { RefCell::new(None) };
}

/// Rebuild the main window's widget tree around its webview. Best-effort: on any failure the tree
/// is left as wry made it and music videos keep using the `<video>` element.
pub fn install(win: &tauri::WebviewWindow, state: Arc<AppState>) {
    let res = win.with_webview(move |wv| match build(wv.inner(), state) {
        Ok(()) => {
            AVAILABLE.store(true, Ordering::Relaxed);
            tracing::info!("native video: GL surface installed under the webview");
        }
        Err(e) => tracing::warn!(error = e, "native video: not installed, videos use <video>"),
    });
    if let Err(e) = res {
        tracing::warn!(error = %e, "native video: could not reach the webview");
    }
}

fn build(webview: webkit2gtk::WebView, state: Arc<AppState>) -> Result<(), &'static str> {
    let parent = webview
        .parent()
        .and_then(|p| p.downcast::<gtk::Box>().ok())
        .ok_or("the webview is not in a GtkBox")?;
    // Which GL API GTK uses decides the loader: EGL on Wayland, GLX on X11 (GTK 3 has no EGL-on-X11).
    let display = gtk::gdk::Display::default().ok_or("no display")?;
    let wayland = display.type_().name() == "GdkWaylandDisplay";
    LOADER.get_or_init(|| load_gl_loader(wayland));

    let overlay = gtk::Overlay::new();
    overlay.add(&gtk::Box::new(gtk::Orientation::Vertical, 0));
    let area = gtk::GLArea::new();
    // Drawn when mpv has a frame, not on every redraw of the window around it.
    area.set_auto_render(false);
    // `show_all` below must leave it hidden: it appears only while the page has a hole for it.
    area.set_no_show_all(true);
    overlay.add_overlay(&area);
    overlay.set_overlay_pass_through(&area, true);
    parent.remove(&webview);
    overlay.add_overlay(&webview);
    parent.pack_start(&overlay, true, true, 0);
    overlay.show_all();
    webview.grab_focus();

    let at = Rc::new(Cell::new(None));
    {
        let (at, area) = (at.clone(), area.clone());
        overlay.connect_get_child_position(move |_, child| {
            if child != area.upcast_ref::<gtk::Widget>() {
                return None; // the webview: the default, which fills the overlay
            }
            at.get().map(|(x, y, w, h)| gtk::gdk::Rectangle::new(x, y, w, h))
        });
    }

    let renderer: Rc<RefCell<Option<VideoRenderer>>> = Rc::new(RefCell::new(None));
    {
        let renderer = renderer.clone();
        area.connect_realize(move |area| {
            area.make_current();
            if let Some(e) = area.error() {
                tracing::warn!(error = %e, "native video: no GL context");
                return;
            }
            let mut r = state.player.video_renderer(gl_proc, gl_display(&display, wayland), wake);
            match r.ensure_contexts() {
                Ok(()) => *renderer.borrow_mut() = Some(r),
                Err(e) => tracing::warn!(error = %e, "native video: mpv refused the GL context"),
            }
        });
    }
    {
        let renderer = renderer.clone();
        // mpv's contexts go while the GL context they were made in is still there to free them in.
        area.connect_unrealize(move |area| {
            area.make_current();
            renderer.borrow_mut().take();
        });
    }
    {
        let renderer = renderer.clone();
        area.connect_render(move |area, _| {
            // `try_`: mpv can call `wake` from inside a render call, and on this thread that runs
            // straight away rather than being queued.
            if let Ok(mut r) = renderer.try_borrow_mut() {
                if let Some(r) = r.as_mut() {
                    let s = area.scale_factor();
                    let (w, h) = (area.allocated_width() * s, area.allocated_height() * s);
                    if let Err(e) = r.render(current_fbo(), w, h) {
                        tracing::debug!(error = %e, "native video: render failed");
                    }
                }
            }
            glib::Propagation::Stop
        });
    }

    let release = Cell::new(None);
    SURFACE.with(|s| {
        *s.borrow_mut() = Some(Surface { overlay, area, webview, renderer, at, release })
    });
    Ok(())
}

/// The picture has been down for [`IDLE_GRACE`]: give back the GPU memory behind it. mpv is already
/// decoding no video (`set_video_visible(false)`), so its render contexts can go.
fn release_surface() {
    SURFACE.with(|s| {
        let Some(s) = &*s.borrow() else { return };
        s.release.set(None);
        if !s.area.is_visible() {
            s.area.unrealize(); // the unrealize handler frees mpv's contexts first
            tracing::debug!("native video: GL surface released");
        }
    });
}

/// mpv has a frame to show, or a new deck needs a render context. Any thread.
fn wake() {
    glib::MainContext::default().invoke(|| {
        SURFACE.with(|s| {
            let Some(s) = &*s.borrow() else { return };
            if s.area.is_visible() {
                s.area.queue_render(); // the render call makes any missing context itself
                return;
            }
            // Hidden, so no render call is coming: make the context now, or a crossfade deck
            // built while the view was shut would have no way to show its picture.
            if s.area.is_realized() {
                s.area.make_current();
                if let Ok(mut r) = s.renderer.try_borrow_mut() {
                    if let Some(r) = r.as_mut() {
                        let _ = r.ensure_contexts();
                    }
                }
            }
        })
    });
}

/// Put the picture at `rect` (`[x, y, w, h]`, CSS pixels relative to the viewport, as the page
/// measured its hole) or take it away, and tell mpv whether anyone can see it. Returns whether the
/// picture is up; `false` for a rect means there is no GL surface and the page should fall back to
/// the `<video>` element.
pub async fn set_rect(
    app: &tauri::AppHandle,
    state: Arc<AppState>,
    rect: Option<[f64; 4]>,
) -> bool {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let res = app.run_on_main_thread(move || {
        let shown = SURFACE.with(|s| {
            let Some(s) = &*s.borrow() else { return false };
            let Some([x, y, w, h]) = rect.filter(|r| r[2] >= 1.0 && r[3] >= 1.0) else {
                s.at.set(None);
                s.area.hide();
                if s.area.is_realized() {
                    let pending = s.release.take();
                    s.release.set(Some(pending.unwrap_or_else(|| {
                        glib::timeout_add_local_once(IDLE_GRACE, release_surface)
                    })));
                }
                return false;
            };
            if let Some(id) = s.release.take() {
                id.remove();
            }
            // CSS pixels to GTK's: the page zoom (1 unless someone changed it), and a pixel of
            // slack all round, because a picture a pixel short of the hole would leave a hairline
            // of desktop showing through the transparent window. The page's surround covers the
            // excess.
            let z = s.webview.zoom_level();
            let (x0, y0) = ((x * z).floor() as i32 - 1, (y * z).floor() as i32 - 1);
            let (x1, y1) = (((x + w) * z).ceil() as i32 + 1, ((y + h) * z).ceil() as i32 + 1);
            s.at.set(Some((x0, y0, x1 - x0, y1 - y0)));
            s.area.realize(); // the first time, this is what makes the renderer
            if s.renderer.borrow().is_none() {
                AVAILABLE.store(false, Ordering::Relaxed);
                s.at.set(None);
                return false;
            }
            s.area.show();
            s.overlay.queue_resize();
            true
        });
        // After the surface, so mpv never starts a video output with no context to draw into.
        state.player.set_video_visible(shown);
        let _ = tx.send(shown);
    });
    res.is_ok() && rx.await.unwrap_or(false)
}

// --- GL plumbing -------------------------------------------------------------------------------

type GetProc = unsafe extern "C" fn(*const c_char) -> *mut c_void;

/// `eglGetProcAddress` or `glXGetProcAddressARB`, whichever GTK's contexts come from.
static LOADER: OnceLock<Option<GetProc>> = OnceLock::new();

fn load_gl_loader(wayland: bool) -> Option<GetProc> {
    let (lib, sym) = if wayland {
        (c"libEGL.so.1", c"eglGetProcAddress")
    } else {
        (c"libGL.so.1", c"glXGetProcAddressARB")
    };
    // SAFETY: plain dlopen/dlsym; the symbol has exactly the `GetProc` signature in both APIs.
    unsafe {
        let h = libc::dlopen(lib.as_ptr(), libc::RTLD_LAZY);
        if h.is_null() {
            return None;
        }
        let f = libc::dlsym(h, sym.as_ptr());
        (!f.is_null()).then(|| std::mem::transmute::<*mut c_void, GetProc>(f))
    }
}

/// mpv's GL function loader.
fn gl_proc(_: &(), name: &str) -> *mut c_void {
    let Ok(name) = CString::new(name) else { return std::ptr::null_mut() };
    // SAFETY: both are lookups by name that return null when there is no such function.
    unsafe {
        let p = match LOADER.get().copied().flatten() {
            Some(f) => f(name.as_ptr()),
            None => std::ptr::null_mut(),
        };
        // An EGL older than 1.5 only hands out extension functions; core ones are plain symbols.
        if p.is_null() {
            libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr())
        } else {
            p
        }
    }
}

/// The framebuffer GtkGLArea bound before emitting `render`, which is where mpv has to draw.
fn current_fbo() -> i32 {
    type GetIntegerv = unsafe extern "C" fn(u32, *mut i32);
    const GL_DRAW_FRAMEBUFFER_BINDING: u32 = 0x8CA6;
    static F: OnceLock<Option<GetIntegerv>> = OnceLock::new();
    let f = F.get_or_init(|| {
        let p = gl_proc(&(), "glGetIntegerv");
        // SAFETY: glGetIntegerv's signature.
        (!p.is_null()).then(|| unsafe { std::mem::transmute::<*mut c_void, GetIntegerv>(p) })
    });
    let mut fbo = 0;
    if let Some(f) = f {
        // SAFETY: called from `render`, with the area's context current.
        unsafe { f(GL_DRAW_FRAMEBUFFER_BINDING, &mut fbo) };
    }
    fbo
}

/// The native display behind GDK's, for mpv's hardware-decoding interop. Looked up by symbol so
/// this needs neither of gdk's backend features; `None` just costs zero-copy decoding.
fn gl_display(display: &gtk::gdk::Display, wayland: bool) -> Option<GlDisplay> {
    let sym = if wayland {
        c"gdk_wayland_display_get_wl_display"
    } else {
        c"gdk_x11_display_get_xdisplay"
    };
    // SAFETY: both take a GdkDisplay of their own backend, which `wayland` was read from.
    unsafe {
        let f = libc::dlsym(libc::RTLD_DEFAULT, sym.as_ptr());
        if f.is_null() {
            return None;
        }
        let f: unsafe extern "C" fn(*mut c_void) -> *mut c_void = std::mem::transmute(f);
        let p = f(display.as_ptr() as *mut c_void);
        (!p.is_null()).then_some(if wayland { GlDisplay::Wayland(p) } else { GlDisplay::X11(p) })
    }
}

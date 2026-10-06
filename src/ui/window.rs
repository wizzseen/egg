//! GTK 3 window. Video is drawn into a drawing area by `xvimagesink`
//! (`VideoOverlay`), not by Cairo, so playback stays at the file's rate.
//!
//! The gstreamer Rust crates pull glib 0.22, and the gtk-rs GTK 3 crate does
//! not share that glib. The window is built by calling libgtk-3 / libgdk-3
//! directly so the control bar and key presses share one GLib main context
//! with GStreamer.

use std::ffi::{c_char, c_int, c_uint, c_void, CStr, CString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, OnceLock};

use gstreamer::glib;
use gstreamer::glib::object::ObjectType;
use gstreamer::glib::translate::FromGlibPtrNone;
use libloading::Library;

use crate::error::{Error, Result};
use crate::input::keyboard::command_from_key_name;
use crate::player::commands::Command;
use crate::ui::status::{bar_text, StatusSnapshot};

const GTK_WINDOW_TOPLEVEL: c_int = 0;
const GTK_ORIENTATION_HORIZONTAL: c_int = 0;
const GTK_ORIENTATION_VERTICAL: c_int = 1;
const GDK_SHIFT_MASK: u32 = 1;

type InitCheckFn = unsafe extern "C" fn(*mut c_int, *mut *mut *mut c_char) -> c_int;
type WindowNewFn = unsafe extern "C" fn(c_int) -> *mut c_void;
type BoxNewFn = unsafe extern "C" fn(c_int, c_int) -> *mut c_void;
type PackFn = unsafe extern "C" fn(*mut c_void, *mut c_void, c_int, c_int, c_uint);
type ContainerAddFn = unsafe extern "C" fn(*mut c_void, *mut c_void);
type SetBoolFn = unsafe extern "C" fn(*mut c_void, c_int);
type ShowFn = unsafe extern "C" fn(*mut c_void);
type SetTitleFn = unsafe extern "C" fn(*mut c_void, *const c_char);
type SetSizeFn = unsafe extern "C" fn(*mut c_void, c_int, c_int);
type FullscreenFn = unsafe extern "C" fn(*mut c_void);
type ButtonNewFn = unsafe extern "C" fn(*const c_char) -> *mut c_void;
type LabelNewFn = unsafe extern "C" fn(*const c_char) -> *mut c_void;
type SetTextFn = unsafe extern "C" fn(*mut c_void, *const c_char);
type IterateFn = unsafe extern "C" fn(c_int) -> c_int;
type DestroyFn = unsafe extern "C" fn(*mut c_void);
type GrabFocusFn = unsafe extern "C" fn(*mut c_void);
type DrawingAreaNewFn = unsafe extern "C" fn() -> *mut c_void;
type GetWindowFn = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
type GetXidFn = unsafe extern "C" fn(*mut c_void) -> u64;
type GetKeyvalFn = unsafe extern "C" fn(*const c_void, *mut c_uint) -> c_int;
type GetStateFn = unsafe extern "C" fn(*const c_void, *mut c_uint) -> c_int;
type KeyvalNameFn = unsafe extern "C" fn(c_uint) -> *const c_char;
type ProgressNewFn = unsafe extern "C" fn() -> *mut c_void;
type SetFractionFn = unsafe extern "C" fn(*mut c_void, f64);
type SetShowTextFn = unsafe extern "C" fn(*mut c_void, c_int);
type ScaleNewFn = unsafe extern "C" fn(c_int, f64, f64, f64) -> *mut c_void;
type RangeGetFn = unsafe extern "C" fn(*mut c_void) -> f64;
type SetDrawValueFn = unsafe extern "C" fn(*mut c_void, c_int);

struct GtkApi {
    _gtk: Library,
    _gdk: Library,
    init_check: InitCheckFn,
    window_new: WindowNewFn,
    box_new: BoxNewFn,
    pack_start: PackFn,
    container_add: ContainerAddFn,
    set_hexpand: SetBoolFn,
    set_vexpand: SetBoolFn,
    set_can_focus: SetBoolFn,
    set_focus_on_click: SetBoolFn,
    show_all: ShowFn,
    set_title: SetTitleFn,
    set_default_size: SetSizeFn,
    fullscreen: FullscreenFn,
    button_new: ButtonNewFn,
    label_new: LabelNewFn,
    label_set_text: SetTextFn,
    button_set_label: SetTextFn,
    iterate: IterateFn,
    destroy: DestroyFn,
    grab_focus: GrabFocusFn,
    drawing_area_new: DrawingAreaNewFn,
    set_app_paintable: SetBoolFn,
    set_double_buffered: SetBoolFn,
    get_window: GetWindowFn,
    get_xid: GetXidFn,
    get_keyval: GetKeyvalFn,
    get_state: GetStateFn,
    keyval_name: KeyvalNameFn,
    progress_new: ProgressNewFn,
    progress_set_fraction: SetFractionFn,
    progress_set_show_text: SetShowTextFn,
    progress_set_text: SetTextFn,
    widget_hide: ShowFn,
    widget_show: ShowFn,
    scale_new: ScaleNewFn,
    range_get_value: RangeGetFn,
    range_set_value: SetFractionFn,
    scale_set_draw_value: SetDrawValueFn,
}

fn api() -> Result<&'static GtkApi> {
    static API: OnceLock<GtkApi> = OnceLock::new();
    if let Some(api) = API.get() {
        return Ok(api);
    }
    let loaded = load_api()?;
    Ok(API.get_or_init(|| loaded))
}

fn load_api() -> Result<GtkApi> {
    unsafe {
        let gtk = Library::new("libgtk-3.so.0")
            .map_err(|e| Error::PlayerInit(format!("libgtk-3.so.0: {e}")))?;
        let gdk = Library::new("libgdk-3.so.0")
            .map_err(|e| Error::PlayerInit(format!("libgdk-3.so.0: {e}")))?;
        let loaded = GtkApi {
            init_check: *gtk
                .get(b"gtk_init_check\0")
                .map_err(|e| Error::PlayerInit(e.to_string()))?,
            window_new: *gtk
                .get(b"gtk_window_new\0")
                .map_err(|e| Error::PlayerInit(e.to_string()))?,
            box_new: *gtk
                .get(b"gtk_box_new\0")
                .map_err(|e| Error::PlayerInit(e.to_string()))?,
            pack_start: *gtk
                .get(b"gtk_box_pack_start\0")
                .map_err(|e| Error::PlayerInit(e.to_string()))?,
            container_add: *gtk
                .get(b"gtk_container_add\0")
                .map_err(|e| Error::PlayerInit(e.to_string()))?,
            set_hexpand: *gtk
                .get(b"gtk_widget_set_hexpand\0")
                .map_err(|e| Error::PlayerInit(e.to_string()))?,
            set_vexpand: *gtk
                .get(b"gtk_widget_set_vexpand\0")
                .map_err(|e| Error::PlayerInit(e.to_string()))?,
            set_can_focus: *gtk
                .get(b"gtk_widget_set_can_focus\0")
                .map_err(|e| Error::PlayerInit(e.to_string()))?,
            set_focus_on_click: *gtk
                .get(b"gtk_widget_set_focus_on_click\0")
                .map_err(|e| Error::PlayerInit(e.to_string()))?,
            show_all: *gtk
                .get(b"gtk_widget_show_all\0")
                .map_err(|e| Error::PlayerInit(e.to_string()))?,
            set_title: *gtk
                .get(b"gtk_window_set_title\0")
                .map_err(|e| Error::PlayerInit(e.to_string()))?,
            set_default_size: *gtk
                .get(b"gtk_window_set_default_size\0")
                .map_err(|e| Error::PlayerInit(e.to_string()))?,
            fullscreen: *gtk
                .get(b"gtk_window_fullscreen\0")
                .map_err(|e| Error::PlayerInit(e.to_string()))?,
            button_new: *gtk
                .get(b"gtk_button_new_with_label\0")
                .map_err(|e| Error::PlayerInit(e.to_string()))?,
            label_new: *gtk
                .get(b"gtk_label_new\0")
                .map_err(|e| Error::PlayerInit(e.to_string()))?,
            label_set_text: *gtk
                .get(b"gtk_label_set_text\0")
                .map_err(|e| Error::PlayerInit(e.to_string()))?,
            button_set_label: *gtk
                .get(b"gtk_button_set_label\0")
                .map_err(|e| Error::PlayerInit(e.to_string()))?,
            iterate: *gtk
                .get(b"gtk_main_iteration_do\0")
                .map_err(|e| Error::PlayerInit(e.to_string()))?,
            destroy: *gtk
                .get(b"gtk_widget_destroy\0")
                .map_err(|e| Error::PlayerInit(e.to_string()))?,
            grab_focus: *gtk
                .get(b"gtk_widget_grab_focus\0")
                .map_err(|e| Error::PlayerInit(e.to_string()))?,
            drawing_area_new: *gtk
                .get(b"gtk_drawing_area_new\0")
                .map_err(|e| Error::PlayerInit(e.to_string()))?,
            set_app_paintable: *gtk
                .get(b"gtk_widget_set_app_paintable\0")
                .map_err(|e| Error::PlayerInit(e.to_string()))?,
            set_double_buffered: *gtk
                .get(b"gtk_widget_set_double_buffered\0")
                .map_err(|e| Error::PlayerInit(e.to_string()))?,
            get_window: *gtk
                .get(b"gtk_widget_get_window\0")
                .map_err(|e| Error::PlayerInit(e.to_string()))?,
            get_xid: *gdk
                .get(b"gdk_x11_window_get_xid\0")
                .map_err(|e| Error::PlayerInit(e.to_string()))?,
            get_keyval: *gdk
                .get(b"gdk_event_get_keyval\0")
                .map_err(|e| Error::PlayerInit(e.to_string()))?,
            get_state: *gdk
                .get(b"gdk_event_get_state\0")
                .map_err(|e| Error::PlayerInit(e.to_string()))?,
            keyval_name: *gdk
                .get(b"gdk_keyval_name\0")
                .map_err(|e| Error::PlayerInit(e.to_string()))?,
            progress_new: *gtk
                .get(b"gtk_progress_bar_new\0")
                .map_err(|e| Error::PlayerInit(e.to_string()))?,
            progress_set_fraction: *gtk
                .get(b"gtk_progress_bar_set_fraction\0")
                .map_err(|e| Error::PlayerInit(e.to_string()))?,
            progress_set_show_text: *gtk
                .get(b"gtk_progress_bar_set_show_text\0")
                .map_err(|e| Error::PlayerInit(e.to_string()))?,
            progress_set_text: *gtk
                .get(b"gtk_progress_bar_set_text\0")
                .map_err(|e| Error::PlayerInit(e.to_string()))?,
            widget_hide: *gtk
                .get(b"gtk_widget_hide\0")
                .map_err(|e| Error::PlayerInit(e.to_string()))?,
            widget_show: *gtk
                .get(b"gtk_widget_show\0")
                .map_err(|e| Error::PlayerInit(e.to_string()))?,
            scale_new: *gtk
                .get(b"gtk_scale_new_with_range\0")
                .map_err(|e| Error::PlayerInit(e.to_string()))?,
            range_get_value: *gtk
                .get(b"gtk_range_get_value\0")
                .map_err(|e| Error::PlayerInit(e.to_string()))?,
            range_set_value: *gtk
                .get(b"gtk_range_set_value\0")
                .map_err(|e| Error::PlayerInit(e.to_string()))?,
            scale_set_draw_value: *gtk
                .get(b"gtk_scale_set_draw_value\0")
                .map_err(|e| Error::PlayerInit(e.to_string()))?,
            _gtk: gtk,
            _gdk: gdk,
        };
        if (loaded.init_check)(std::ptr::null_mut(), std::ptr::null_mut()) == 0 {
            return Err(Error::PlayerInit(
                "GTK could not open a display. Is DISPLAY set?".into(),
            ));
        }
        Ok(loaded)
    }
}

struct ClickBinding {
    cmd: Command,
    tx: mpsc::Sender<Command>,
}

unsafe extern "C" fn on_clicked(_button: *mut c_void, data: *mut c_void) {
    let binding = unsafe { &*(data as *const ClickBinding) };
    let _ = binding.tx.send(binding.cmd);
}

unsafe extern "C" fn on_key(_widget: *mut c_void, event: *mut c_void, data: *mut c_void) -> c_int {
    let tx = unsafe { &*(data as *const mpsc::Sender<Command>) };
    let Some(cmd) = key_command(event) else {
        return 0;
    };
    let _ = tx.send(cmd);
    1
}

unsafe extern "C" fn on_delete(
    _widget: *mut c_void,
    _event: *mut c_void,
    data: *mut c_void,
) -> c_int {
    let tx = unsafe { &*(data as *const mpsc::Sender<Command>) };
    let _ = tx.send(Command::Quit);
    // Keep the widget alive until the player shuts the pipeline down.
    1
}

struct SeekBinding {
    tx: mpsc::Sender<Command>,
    dragging: Arc<AtomicBool>,
}

unsafe extern "C" fn on_seek_press(
    _widget: *mut c_void,
    _event: *mut c_void,
    data: *mut c_void,
) -> c_int {
    let binding = unsafe { &*(data as *const SeekBinding) };
    binding.dragging.store(true, Ordering::SeqCst);
    0
}

unsafe extern "C" fn on_seek_release(
    widget: *mut c_void,
    _event: *mut c_void,
    data: *mut c_void,
) -> c_int {
    let binding = unsafe { &*(data as *const SeekBinding) };
    binding.dragging.store(false, Ordering::SeqCst);
    let Ok(api) = api() else {
        return 0;
    };
    let fraction = unsafe { (api.range_get_value)(widget) };
    let _ = binding.tx.send(Command::SeekFraction(fraction));
    0
}

fn key_command(event: *mut c_void) -> Option<Command> {
    let api = api().ok()?;
    unsafe {
        let mut keyval = 0u32;
        if (api.get_keyval)(event, &mut keyval) == 0 {
            return None;
        }
        let mut state = 0u32;
        let _ = (api.get_state)(event, &mut state);
        let name_ptr = (api.keyval_name)(keyval);
        if name_ptr.is_null() {
            return None;
        }
        let name = CStr::from_ptr(name_ptr).to_str().ok()?;
        command_from_key_name(name, state & GDK_SHIFT_MASK != 0)
    }
}

fn connect_signal<T>(
    widget: *mut c_void,
    signal: &str,
    trampoline: unsafe extern "C" fn(),
    data: Box<T>,
) {
    let Ok(api_name) = CString::new(signal) else {
        return;
    };
    unsafe {
        let obj = glib::Object::from_glib_none(widget as *mut glib::gobject_ffi::GObject);
        let _id = glib::signal::connect_raw(
            obj.as_ptr(),
            api_name.as_ptr(),
            Some(trampoline),
            Box::into_raw(data),
        );
    }
}

/// Initialize GTK before the video window is created.
pub fn ensure_gtk() -> Result<()> {
    api().map(|_| ())
}

/// Native video window with a control bar. Headless playback does not create one.
pub struct VideoWindow {
    window: *mut c_void,
    play_button: *mut c_void,
    status_label: *mut c_void,
    progress: *mut c_void,
    seek: *mut c_void,
    dragging: Arc<AtomicBool>,
    rx: mpsc::Receiver<Command>,
    iterate_fn: IterateFn,
    label_set_text: SetTextFn,
    button_set_label: SetTextFn,
    progress_set_fraction: SetFractionFn,
    progress_set_text: SetTextFn,
    widget_show: ShowFn,
    widget_hide: ShowFn,
    range_set_value: SetFractionFn,
    fullscreen_fn: FullscreenFn,
    destroy: DestroyFn,
}

impl VideoWindow {
    /// Show the control window and return the X11 id of the video area.
    /// `xvimagesink` draws into that window.
    pub fn create(title: &str) -> Result<(Self, u64)> {
        let api = api()?;
        let (tx, rx) = mpsc::channel();
        unsafe {
            let window = (api.window_new)(GTK_WINDOW_TOPLEVEL);
            let vbox = (api.box_new)(GTK_ORIENTATION_VERTICAL, 0);
            let bar = (api.box_new)(GTK_ORIENTATION_HORIZONTAL, 4);
            let video = (api.drawing_area_new)();
            (api.set_hexpand)(video, 1);
            (api.set_vexpand)(video, 1);
            (api.set_can_focus)(video, 1);
            // GTK must not paint over the Xv overlay, or the picture goes grey.
            (api.set_app_paintable)(video, 1);
            (api.set_double_buffered)(video, 0);
            (api.pack_start)(vbox, video, 1, 1, 0);
            let seek = (api.scale_new)(GTK_ORIENTATION_HORIZONTAL, 0.0, 1.0, 0.0001);
            (api.scale_set_draw_value)(seek, 0);
            (api.set_hexpand)(seek, 1);
            (api.set_can_focus)(seek, 0);
            (api.pack_start)(vbox, seek, 0, 1, 4);
            let progress = (api.progress_new)();
            (api.progress_set_show_text)(progress, 1);
            (api.progress_set_fraction)(progress, 0.0);
            (api.set_hexpand)(progress, 1);
            (api.pack_start)(vbox, progress, 0, 1, 0);
            (api.pack_start)(vbox, bar, 0, 1, 6);

            let dragging = Arc::new(AtomicBool::new(false));
            let press = Box::new(SeekBinding {
                tx: tx.clone(),
                dragging: Arc::clone(&dragging),
            });
            let release = Box::new(SeekBinding {
                tx: tx.clone(),
                dragging: Arc::clone(&dragging),
            });
            connect_signal(
                seek,
                "button-press-event",
                std::mem::transmute::<
                    unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> c_int,
                    unsafe extern "C" fn(),
                >(on_seek_press),
                press,
            );
            connect_signal(
                seek,
                "button-release-event",
                std::mem::transmute::<
                    unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> c_int,
                    unsafe extern "C" fn(),
                >(on_seek_release),
                release,
            );

            add_button(api, bar, "Back 10", Command::StepBackward(10), &tx)?;
            add_button(api, bar, "Back", Command::StepBackward(1), &tx)?;
            let play = add_button(api, bar, "Pause", Command::PlayPause, &tx)?;
            add_button(api, bar, "Fwd", Command::StepForward(1), &tx)?;
            add_button(api, bar, "Fwd 10", Command::StepForward(10), &tx)?;
            add_button(api, bar, "Vol-", Command::VolumeDown, &tx)?;
            add_button(api, bar, "Vol+", Command::VolumeUp, &tx)?;
            add_button(api, bar, "Spd-", Command::SpeedDown, &tx)?;
            add_button(api, bar, "Spd+", Command::SpeedUp, &tx)?;
            add_button(api, bar, "1x", Command::SpeedReset, &tx)?;
            add_button(api, bar, "Loop", Command::ToggleLoop, &tx)?;
            add_button(api, bar, "Info", Command::ShowInfo, &tx)?;
            add_button(api, bar, "To .hm", Command::SaveHm, &tx)?;
            add_button(api, bar, "Quit", Command::Quit, &tx)?;

            let status = CString::new("").unwrap_or_else(|_| CString::new(" ").unwrap());
            let label = (api.label_new)(status.as_ptr());
            (api.set_can_focus)(label, 0);
            (api.set_hexpand)(label, 1);
            (api.pack_start)(bar, label, 1, 1, 8);

            let title = CString::new(title).unwrap_or_else(|_| CString::new("egg").unwrap());
            (api.set_title)(window, title.as_ptr());
            (api.set_default_size)(window, 1280, 800);
            (api.container_add)(window, vbox);

            connect_key(window, &tx);
            connect_key(video, &tx);
            connect_signal(
                window,
                "delete-event",
                std::mem::transmute::<
                    unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> c_int,
                    unsafe extern "C" fn(),
                >(on_delete),
                Box::new(tx.clone()),
            );

            (api.show_all)(window);
            (api.widget_hide)(progress);
            let gdk_window = (api.get_window)(video);
            if gdk_window.is_null() {
                return Err(Error::VideoSink(
                    "video area has no X11 window after realize".into(),
                ));
            }
            let xid = (api.get_xid)(gdk_window);
            if xid == 0 {
                return Err(Error::VideoSink("video area X11 id is 0".into()));
            }
            (api.grab_focus)(video);

            Ok((
                Self {
                    window,
                    play_button: play,
                    status_label: label,
                    progress,
                    seek,
                    dragging,
                    rx,
                    iterate_fn: api.iterate,
                    label_set_text: api.label_set_text,
                    button_set_label: api.button_set_label,
                    progress_set_fraction: api.progress_set_fraction,
                    progress_set_text: api.progress_set_text,
                    widget_show: api.widget_show,
                    widget_hide: api.widget_hide,
                    range_set_value: api.range_set_value,
                    fullscreen_fn: api.fullscreen,
                    destroy: api.destroy,
                },
                xid,
            ))
        }
    }

    pub fn iterate(&self) {
        unsafe {
            (self.iterate_fn)(0);
        }
    }

    pub fn poll_command(&self) -> Option<Command> {
        self.rx.try_recv().ok()
    }

    pub fn fullscreen(&self) {
        unsafe {
            (self.fullscreen_fn)(self.window);
        }
    }

    /// Show or hide the save bar. `None` hides it.
    pub fn set_save_progress(&self, progress: Option<(u32, u32, String)>) {
        let Some((done, total, text)) = progress else {
            unsafe { (self.widget_hide)(self.progress) };
            return;
        };
        let fraction = if total == 0 {
            0.0
        } else {
            (done as f64 / total as f64).clamp(0.0, 1.0)
        };
        let Ok(text) = CString::new(text) else {
            return;
        };
        unsafe {
            (self.progress_set_fraction)(self.progress, fraction);
            (self.progress_set_text)(self.progress, text.as_ptr());
            (self.widget_show)(self.progress);
        }
    }

    /// Replace the bar text. Used while DualStep is being written.
    pub fn set_message(&self, text: &str) {
        let Ok(text) = CString::new(text) else {
            return;
        };
        unsafe {
            (self.label_set_text)(self.status_label, text.as_ptr());
        }
    }

    pub fn update(&self, status: &StatusSnapshot) {
        let text = bar_text(status);
        let Ok(text) = CString::new(text) else {
            return;
        };
        let play = if status.state == crate::player::state::PlayerState::Playing {
            "Pause"
        } else {
            "Play"
        };
        let Ok(play) = CString::new(play) else {
            return;
        };
        unsafe {
            (self.label_set_text)(self.status_label, text.as_ptr());
            (self.button_set_label)(self.play_button, play.as_ptr());
            if !self.dragging.load(Ordering::SeqCst) {
                if let (Some(pos), Some(dur)) = (status.position, status.duration) {
                    if dur.nseconds() > 0 {
                        let fraction =
                            (pos.nseconds() as f64 / dur.nseconds() as f64).clamp(0.0, 1.0);
                        (self.range_set_value)(self.seek, fraction);
                    }
                }
            }
        }
    }
}

impl Drop for VideoWindow {
    fn drop(&mut self) {
        if !self.window.is_null() {
            unsafe { (self.destroy)(self.window) };
            self.window = std::ptr::null_mut();
        }
    }
}

fn add_button(
    api: &GtkApi,
    bar: *mut c_void,
    label: &str,
    cmd: Command,
    tx: &mpsc::Sender<Command>,
) -> Result<*mut c_void> {
    let text = CString::new(label).map_err(|e| Error::PlayerInit(e.to_string()))?;
    unsafe {
        let button = (api.button_new)(text.as_ptr());
        (api.set_can_focus)(button, 0);
        (api.set_focus_on_click)(button, 0);
        (api.pack_start)(bar, button, 0, 0, 0);
        connect_signal(
            button,
            "clicked",
            std::mem::transmute::<
                unsafe extern "C" fn(*mut c_void, *mut c_void),
                unsafe extern "C" fn(),
            >(on_clicked),
            Box::new(ClickBinding {
                cmd,
                tx: tx.clone(),
            }),
        );
        Ok(button)
    }
}

fn connect_key(widget: *mut c_void, tx: &mpsc::Sender<Command>) {
    connect_signal(
        widget,
        "key-press-event",
        unsafe {
            std::mem::transmute::<
                unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> c_int,
                unsafe extern "C" fn(),
            >(on_key)
        },
        Box::new(tx.clone()),
    );
}

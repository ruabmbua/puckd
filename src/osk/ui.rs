//! Wayland side of the overlay: a wlr-layer-shell surface on the overlay layer that never
//! takes keyboard focus. Runs on its own thread with a calloop event loop.

use std::collections::HashMap;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::Instant;

use log::{debug, info};
use smithay_client_toolkit::reexports::client::{
    Connection, QueueHandle,
    globals::registry_queue_init,
    protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, FrameCallbackData},
    delegate_registry,
    output::{OutputHandler, OutputState},
    reexports::calloop::{EventLoop, LoopHandle, channel as cchan},
    reexports::calloop_wayland_source::WaylandSource,
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keymap, Keysym, Modifiers, RawModifiers},
        pointer::{PointerEvent, PointerEventKind, PointerHandler},
    },
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure,
        },
    },
    shm::{Shm, ShmHandler, slot::SlotPool},
};

use crate::osk::layout::Labels;
use crate::osk::model::Model;
use crate::osk::render::{self, Fonts, Geo, HEIGHT, PROMPT_H, PROMPT_W, TOAST_H, TOAST_W, WIDTH};
use crate::osk::{Cmd, Event, EventSink, Tab};
use smithay_client_toolkit::dispatch2::Dispatch2;
use smithay_client_toolkit::reexports::client::Proxy;
use smithay_client_toolkit::reexports::client::backend::{ObjectData, ObjectId};
use smithay_client_toolkit::reexports::protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1::{self, ZwlrForeignToplevelHandleV1},
    zwlr_foreign_toplevel_manager_v1::{self, ZwlrForeignToplevelManagerV1},
};

const BTN_LEFT: u32 = 0x110;

pub fn run(rx: cchan::Channel<Cmd>, sink: EventSink) {
    if let Err(e) = run_inner(rx, &sink) {
        sink.send(Event::Failed(e));
    }
    debug!("overlay thread exited");
}

/// Connect to the compositor. A systemd user service often lacks WAYLAND_DISPLAY,
/// so fall back to the first wayland socket in XDG_RUNTIME_DIR.
fn connect() -> Result<Connection, String> {
    if let Ok(c) = Connection::connect_to_env() {
        return Ok(c);
    }
    let dir = std::env::var_os("XDG_RUNTIME_DIR").ok_or("no WAYLAND_DISPLAY and no XDG_RUNTIME_DIR")?;
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .map_err(|e| e.to_string())?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("wayland-") && !n.ends_with(".lock"))
        .collect();
    names.sort();
    for n in names {
        if let Ok(s) = UnixStream::connect(std::path::Path::new(&dir).join(&n))
            && let Ok(c) = Connection::from_socket(s)
        {
            info!("overlay: connected to {n}");
            return Ok(c);
        }
    }
    Err("no Wayland compositor found".into())
}

struct App {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    shm: Shm,
    pool: SlotPool,
    layer: Option<LayerSurface>,
    pending_show: bool,
    configured: bool,
    frame_pending: bool,
    scale: i32,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    model: Model,
    fonts: Fonts,
    loop_handle: LoopHandle<'static, App>,
    /// The overlay currently holds keyboard focus (while a name is typed).
    kb_grab: bool,
    /// "Switch preset?" popup: requested text, surface, configured.
    prompt_text: Option<(String, String)>,
    prompt: Option<LayerSurface>,
    prompt_configured: bool,
    prompt_dirty: bool,
    /// Mode-switch banner: content, surface, configured, needs drawing, hide time.
    toast_data: Option<crate::osk::Toast>,
    toast: Option<LayerSurface>,
    toast_configured: bool,
    toast_dirty: bool,
    toast_until: Option<Instant>,
    /// Toplevels from wlr-foreign-toplevel: (app_id, activated), and the last focused app.
    toplevels: HashMap<ObjectId, Toplevel>,
    /// Last reported focus: (app key, title).
    focused: Option<(String, String)>,
    geo: Geo,
    sink: EventSink,
    exit: bool,
}

fn run_inner(rx: cchan::Channel<Cmd>, sink: &EventSink) -> Result<(), String> {
    let conn = connect()?;
    let (globals, queue) = registry_queue_init(&conn).map_err(|e| format!("wayland registry: {e}"))?;
    let qh = queue.handle();
    let compositor = CompositorState::bind(&globals, &qh).map_err(|e| format!("wl_compositor: {e}"))?;
    let layer_shell = LayerShell::bind(&globals, &qh).map_err(|e| format!("compositor lacks wlr-layer-shell: {e}"))?;
    let shm = Shm::bind(&globals, &qh).map_err(|e| format!("wl_shm: {e}"))?;
    let pool = SlotPool::new((WIDTH * HEIGHT * 4) as usize, &shm).map_err(|e| format!("shm pool: {e}"))?;
    let fonts = Fonts::load()?;

    // Focus tracking for per-app presets (optional: not every compositor has it).
    let toplevel_manager =
        globals.bind::<ZwlrForeignToplevelManagerV1, App, ToplevelManager>(&qh, 1..=3, ToplevelManager).ok();
    if toplevel_manager.is_none() {
        info!("overlay: no wlr-foreign-toplevel-management; per-app presets are unavailable");
    }

    let mut event_loop: EventLoop<App> = EventLoop::try_new().map_err(|e| e.to_string())?;
    WaylandSource::new(conn.clone(), queue).insert(event_loop.handle()).map_err(|e| e.to_string())?;
    event_loop
        .handle()
        .insert_source(rx, |ev, _, app: &mut App| match ev {
            cchan::Event::Msg(cmd) => app.command(cmd),
            cchan::Event::Closed => app.exit = true,
        })
        .map_err(|e| e.to_string())?;

    let mut app = App {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        compositor,
        layer_shell,
        shm,
        pool,
        layer: None,
        pending_show: false,
        configured: false,
        frame_pending: false,
        scale: 1,
        keyboard: None,
        pointer: None,
        model: Model::new(Labels::fallback()),
        fonts,
        loop_handle: event_loop.handle(),
        kb_grab: false,
        prompt_text: None,
        prompt: None,
        prompt_configured: false,
        prompt_dirty: false,
        toast_data: None,
        toast: None,
        toast_configured: false,
        toast_dirty: false,
        toast_until: None,
        toplevels: HashMap::new(),
        focused: None,
        geo: Geo::new(),
        sink: sink.clone(),
        exit: false,
    };

    while !app.exit {
        let timeout = app.toast_until.map(|t| t.saturating_duration_since(Instant::now()));
        event_loop.dispatch(timeout, &mut app).map_err(|e| format!("event loop: {e}"))?;
        if app.toast_until.is_some_and(|t| Instant::now() >= t) {
            app.toast = None;
            app.toast_data = None;
            app.toast_until = None;
        }
        if app.layer.is_none() && app.configured {
            app.configured = false;
        }
        app.maybe_draw(&qh);
        conn.flush().map_err(|e| format!("wayland: {e}"))?;
    }
    Ok(())
}

impl App {
    fn emit(&self, events: Vec<Event>) {
        for e in events {
            self.sink.send(e);
        }
    }

    fn command(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Show(tab) => {
                self.model.show(tab, Instant::now());
                self.pending_show = true;
                self.place_prompt();
            }
            Cmd::Hide => {
                let mut out = Vec::new();
                self.model.hide(&mut out);
                self.emit(out);
                self.layer = None;
                self.pending_show = false;
                self.configured = false;
                self.frame_pending = false;
                self.place_prompt();
            }
            Cmd::Input(st, raw) => {
                if self.layer.is_some() || self.pending_show {
                    let ev = self.model.input(&st, &raw, Instant::now());
                    self.emit(ev);
                }
            }
            Cmd::Status(s) => self.model.set_status(s),
            Cmd::Toast(t) => {
                self.toast_until = Some(Instant::now() + std::time::Duration::from_secs_f32(t.secs.max(0.5)));
                self.toast_data = Some(t);
                self.toast_dirty = true;
            }
            Cmd::Prompt(text) => {
                if text.is_none() {
                    self.prompt = None;
                    self.prompt_configured = false;
                }
                self.prompt_text = text;
                self.prompt_dirty = true;
            }
        }
    }
}

impl App {
    fn create_layer(&mut self, qh: &QueueHandle<Self>) {
        let surface = self.compositor.create_surface(qh);
        let layer = self.layer_shell.create_layer_surface(qh, surface, Layer::Overlay, Some("puckd-osk"), None);
        layer.set_anchor(Anchor::BOTTOM);
        layer.set_size(WIDTH, HEIGHT);
        layer.set_margin(0, 0, 24, 0);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.set_exclusive_zone(0);
        layer.commit();
        self.layer = Some(layer);
        self.kb_grab = false;
        self.configured = false;
        self.frame_pending = false;
        self.model.dirty = true;
    }

    /// Bottom margin of the popup: above the overlay while that is open.
    fn prompt_margin(&self) -> i32 {
        if self.layer.is_some() || self.pending_show { 24 + HEIGHT as i32 + 12 } else { 32 }
    }

    /// Bottom margin of the banner: above the overlay and the popup when they show.
    fn toast_margin(&self) -> i32 {
        self.prompt_margin() + if self.prompt.is_some() { PROMPT_H as i32 + 12 } else { 0 }
    }

    /// Re-place the popup and banner when the overlay opens or closes.
    fn place_prompt(&self) {
        if let Some(p) = &self.prompt {
            p.set_margin(0, 0, self.prompt_margin(), 0);
            p.commit();
        }
        if let Some(t) = &self.toast {
            t.set_margin(0, 0, self.toast_margin(), 0);
            t.commit();
        }
    }

    fn draw_toast(&mut self, qh: &QueueHandle<Self>) {
        if !self.toast_dirty {
            return;
        }
        let Some(data) = self.toast_data.clone() else {
            self.toast_dirty = false;
            return;
        };
        let Some(layer) = &self.toast else {
            let surface = self.compositor.create_surface(qh);
            let l = self.layer_shell.create_layer_surface(qh, surface, Layer::Overlay, Some("puckd-banner"), None);
            l.set_anchor(Anchor::BOTTOM);
            l.set_size(TOAST_W, TOAST_H);
            l.set_margin(0, 0, self.toast_margin(), 0);
            l.set_keyboard_interactivity(KeyboardInteractivity::None);
            l.commit();
            self.toast = Some(l);
            self.toast_configured = false;
            return;
        };
        if !self.toast_configured {
            return;
        }
        let scale = self.scale.max(1);
        let Some(pm) = render::draw_toast(&data, &mut self.fonts, scale as f32) else { return };
        let (w, h) = (pm.width() as i32, pm.height() as i32);
        let Ok((buffer, canvas)) = self.pool.create_buffer(w, h, w * 4, wl_shm::Format::Argb8888) else { return };
        for (d, s) in canvas.as_chunks_mut::<4>().0.iter_mut().zip(pm.data().as_chunks::<4>().0) {
            *d = [s[2], s[1], s[0], s[3]];
        }
        let surface = layer.wl_surface();
        surface.set_buffer_scale(scale);
        surface.damage_buffer(0, 0, w, h);
        if buffer.attach_to(surface).is_ok() {
            layer.commit();
            self.toast_dirty = false;
        }
    }

    fn draw_prompt(&mut self, qh: &QueueHandle<Self>) {
        if !self.prompt_dirty {
            return;
        }
        let Some(text) = self.prompt_text.clone() else {
            self.prompt_dirty = false;
            return;
        };
        let Some(layer) = &self.prompt else {
            let surface = self.compositor.create_surface(qh);
            let l = self.layer_shell.create_layer_surface(qh, surface, Layer::Overlay, Some("puckd-prompt"), None);
            l.set_anchor(Anchor::BOTTOM);
            l.set_size(PROMPT_W, PROMPT_H);
            l.set_margin(0, 0, self.prompt_margin(), 0);
            l.set_keyboard_interactivity(KeyboardInteractivity::None);
            l.commit();
            self.prompt = Some(l);
            self.prompt_configured = false;
            return;
        };
        if !self.prompt_configured {
            return;
        }
        let scale = self.scale.max(1);
        let Some(pm) = render::draw_prompt(&text.0, &text.1, &mut self.fonts, scale as f32) else { return };
        let (w, h) = (pm.width() as i32, pm.height() as i32);
        let Ok((buffer, canvas)) = self.pool.create_buffer(w, h, w * 4, wl_shm::Format::Argb8888) else { return };
        for (d, s) in canvas.as_chunks_mut::<4>().0.iter_mut().zip(pm.data().as_chunks::<4>().0) {
            *d = [s[2], s[1], s[0], s[3]];
        }
        let surface = layer.wl_surface();
        surface.set_buffer_scale(scale);
        surface.damage_buffer(0, 0, w, h);
        if buffer.attach_to(surface).is_ok() {
            layer.commit();
            self.prompt_dirty = false;
        }
    }

    /// A key from a real keyboard (we only have focus while a name is typed).
    fn on_key(&mut self, ev: KeyEvent) {
        use crate::osk::model::EntryInput;
        if self.model.entry.is_none() {
            return;
        }
        let input = match ev.keysym {
            Keysym::BackSpace => EntryInput::Backspace,
            Keysym::Return | Keysym::KP_Enter => EntryInput::Confirm,
            Keysym::Escape => EntryInput::Cancel,
            _ => match ev.utf8 {
                Some(t) if !t.is_empty() => EntryInput::Text(t),
                _ => return,
            },
        };
        let mut out = Vec::new();
        self.model.entry_input(input, &mut out);
        self.emit(out);
    }

    /// Take keyboard focus while a name is typed, give it back afterwards.
    fn sync_keyboard_grab(&mut self) {
        let want = self.model.entry.is_some();
        if let Some(layer) = &self.layer
            && want != self.kb_grab
        {
            layer.set_keyboard_interactivity(if want {
                KeyboardInteractivity::Exclusive
            } else {
                KeyboardInteractivity::None
            });
            layer.commit();
            self.kb_grab = want;
        }
    }

    fn maybe_draw(&mut self, qh: &QueueHandle<Self>) {
        self.sync_keyboard_grab();
        self.draw_prompt(qh);
        self.draw_toast(qh);
        if self.pending_show {
            self.pending_show = false;
            if self.layer.is_none() {
                self.create_layer(qh);
            }
        }
        if !self.configured || self.frame_pending || !self.model.dirty {
            return;
        }
        let Some(layer) = &self.layer else { return };
        let scale = self.scale.max(1);
        let Some(pm) = render::draw(&self.model, &mut self.fonts, scale as f32) else { return };
        let (w, h) = (pm.width() as i32, pm.height() as i32);
        let Ok((buffer, canvas)) = self.pool.create_buffer(w, h, w * 4, wl_shm::Format::Argb8888) else {
            return;
        };
        // tiny-skia: premultiplied RGBA; wl_shm ARGB8888 little endian: B G R A.
        for (d, s) in canvas.as_chunks_mut::<4>().0.iter_mut().zip(pm.data().as_chunks::<4>().0) {
            *d = [s[2], s[1], s[0], s[3]];
        }
        let surface = layer.wl_surface();
        surface.set_buffer_scale(scale);
        surface.damage_buffer(0, 0, w, h);
        surface.frame(qh, FrameCallbackData(surface.clone()));
        if buffer.attach_to(surface).is_ok() {
            layer.commit();
            self.frame_pending = true;
            self.model.dirty = false;
        }
    }
}

impl CompositorHandler for App {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, f: i32) {
        if f != self.scale {
            self.scale = f;
            self.model.dirty = true;
        }
    }
    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {
        self.frame_pending = false;
    }
    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl LayerShellHandler for App {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, l: &LayerSurface) {
        if self.toast.as_ref() == Some(l) {
            self.toast = None;
            self.toast_data = None;
            self.toast_until = None;
            return;
        }
        if self.prompt.as_ref() == Some(l) {
            self.prompt = None;
            self.prompt_text = None;
            return;
        }
        let mut out = Vec::new();
        self.model.hide(&mut out);
        self.emit(out);
        self.layer = None;
        self.configured = false;
        self.sink.send(Event::Closed);
    }

    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, l: &LayerSurface, _: LayerSurfaceConfigure, _: u32) {
        if self.toast.as_ref() == Some(l) {
            self.toast_configured = true;
            self.toast_dirty = true;
            return;
        }
        if self.prompt.as_ref() == Some(l) {
            self.prompt_configured = true;
            self.prompt_dirty = true;
            return;
        }
        // Fixed size; the compositor's suggestion doesn't matter.
        self.configured = true;
        self.frame_pending = false;
        self.model.dirty = true;
    }
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, cap: Capability) {
        if cap == Capability::Keyboard && self.keyboard.is_none() {
            // For the keymap (labels), and for typing names with a real keyboard: the
            // overlay only takes keyboard focus while a name is being typed.
            self.keyboard = self
                .seat_state
                .get_keyboard_with_repeat(
                    qh,
                    &seat,
                    None,
                    self.loop_handle.clone(),
                    Box::new(|app: &mut App, _, ev: KeyEvent| app.on_key(ev)),
                )
                .ok();
        }
        if cap == Capability::Pointer && self.pointer.is_none() {
            self.pointer = self.seat_state.get_pointer(qh, &seat).ok();
        }
    }
    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, cap: Capability) {
        if cap == Capability::Keyboard
            && let Some(k) = self.keyboard.take()
        {
            k.release();
        }
        if cap == Capability::Pointer
            && let Some(p) = self.pointer.take()
        {
            p.release();
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for App {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
    }
    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
    ) {
    }
    fn press_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, ev: KeyEvent) {
        self.on_key(ev);
    }
    fn repeat_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, ev: KeyEvent) {
        self.on_key(ev);
    }
    fn release_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: KeyEvent) {
    }
    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: Modifiers,
        _: RawModifiers,
        layout: u32,
    ) {
        self.model.set_group(layout);
    }
    fn update_keymap(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        keymap: Keymap<'_>,
    ) {
        match Labels::from_string(&keymap.as_string()) {
            Some(l) => {
                debug!("overlay: got the compositor keymap");
                self.model.set_labels(l);
            }
            None => debug!("overlay: could not parse the compositor keymap"),
        }
    }
}

impl PointerHandler for App {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        let Some(layer) = &self.layer else { return };
        let mut out = Vec::new();
        for e in events {
            if &e.surface != layer.wl_surface() {
                continue;
            }
            let (x, y) = (e.position.0 as f32, e.position.1 as f32);
            match e.kind {
                PointerEventKind::Press { button: BTN_LEFT, .. } => {
                    if let Some(tab) = self.geo.tab_at(x, y) {
                        self.model.set_tab(tab, &mut out);
                        continue;
                    }
                    match self.model.tab {
                        Tab::Keyboard => {
                            let (ux, uy) = self.geo.to_units(x, y);
                            let idx = self.model.layout.hit(ux, uy);
                            self.model.pointer_key(idx, &mut out);
                        }
                        Tab::Mapping
                            if matches!(self.model.map_edit, Some(crate::osk::model::MapEdit::List { .. })) =>
                        {
                            if let Some(o) = self.geo.picker_at(x, y) {
                                self.model.pointer_item(o, &mut out);
                            }
                        }
                        Tab::Settings | Tab::Mapping | Tab::Presets => {
                            let (items, focus) = self.model.list();
                            if let Some(i) = self.geo.item_at(y, focus, items.len(), self.model.tab) {
                                self.model.pointer_item(i, &mut out);
                            }
                        }
                        Tab::Input => {}
                    }
                }
                PointerEventKind::Release { button: BTN_LEFT, .. } => self.model.pointer_release(&mut out),
                _ => {}
            }
        }
        // Pointer clicks don't need haptics on the controller.
        out.retain(|e| !matches!(e, Event::Haptic { .. }));
        self.emit(out);
    }
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_registry!(App);

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

smithay_client_toolkit::delegate_dispatch2!(App);

/// wlr-foreign-toplevel: tracks which application has focus.
#[derive(Default)]
struct Toplevel {
    app_id: String,
    title: String,
    activated: bool,
}

struct ToplevelManager;
struct ToplevelHandle;

impl Dispatch2<ZwlrForeignToplevelManagerV1, App> for ToplevelManager {
    fn event(
        &self,
        app: &mut App,
        _: &ZwlrForeignToplevelManagerV1,
        event: zwlr_foreign_toplevel_manager_v1::Event,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
        if let zwlr_foreign_toplevel_manager_v1::Event::Toplevel { toplevel } = event {
            app.toplevels.insert(toplevel.id(), Toplevel::default());
        }
    }

    fn event_created_child(_opcode: u16, qh: &QueueHandle<App>) -> Arc<dyn ObjectData> {
        qh.make_data::<ZwlrForeignToplevelHandleV1, ToplevelHandle>(ToplevelHandle)
    }
}

impl Dispatch2<ZwlrForeignToplevelHandleV1, App> for ToplevelHandle {
    fn event(
        &self,
        app: &mut App,
        handle: &ZwlrForeignToplevelHandleV1,
        event: zwlr_foreign_toplevel_handle_v1::Event,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
        use zwlr_foreign_toplevel_handle_v1::Event as E;
        let id = handle.id();
        match event {
            E::AppId { app_id } => {
                app.toplevels.entry(id).or_default().app_id = app_id;
            }
            E::Title { title } => {
                app.toplevels.entry(id).or_default().title = title;
            }
            E::State { state } => {
                let activated = state
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|c| u32::from_ne_bytes(*c) == zwlr_foreign_toplevel_handle_v1::State::Activated as u32);
                app.toplevels.entry(id).or_default().activated = activated;
            }
            E::Done => {
                if let Some(t) = app.toplevels.get(&id)
                    && t.activated
                {
                    // Windows without an app_id (e.g. some Wine/XWayland games) are keyed by title.
                    let key = if !t.app_id.is_empty() {
                        t.app_id.clone()
                    } else if !t.title.is_empty() {
                        format!("title:{}", t.title)
                    } else {
                        return;
                    };
                    let seen = (key.clone(), t.title.clone());
                    if app.focused.as_ref() != Some(&seen) {
                        app.focused = Some(seen);
                        app.sink.send(Event::Focus { app: key, title: t.title.clone() });
                    }
                }
            }
            E::Closed => {
                app.toplevels.remove(&id);
                handle.destroy();
            }
            _ => {}
        }
    }
}

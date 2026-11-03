//! deskpet: a transparent, always-on-top 3D desktop pet for macOS.

mod behavior;
mod camera;
mod check;
mod cursor;
mod hit;
mod model;
mod pet;
mod pose;
mod renderer;
mod scaffold;
mod scene;
mod schedule;
mod sheet;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use glam::{Mat4, Quat, Vec2};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::platform::macos::WindowAttributesExtMacOS;
use winit::window::{Window, WindowAttributes, WindowId, WindowLevel};

use behavior::Behavior;
use camera::Camera;
use model::Model;
use pet::{Pet, Style};
use pose::Pose;
use renderer::{FrameInput, Renderer};
use schedule::{Activity, PowerProfile, Scheduler};

/// How often to check whether the cursor is over the pet while it ignores the mouse.
const HIT_POLL: Duration = Duration::from_millis(100);
/// Hit slack, as a fraction of the pet's height.
const HIT_SLACK: f32 = 0.01;

struct Options {
    pet: PathBuf,
    style: Option<String>,
    power: PowerProfile,
}

struct App {
    model: Model,
    pet: Pet,
    style: Style,
    behavior: Behavior,
    facing: Quat,
    camera: Option<Camera>,
    height: f32,
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    scheduler: Scheduler,
    started: Instant,
    last_frame: Instant,
    next_hit_poll: Instant,
    pose: Pose,
    yaw: f32,
    spinning: bool,
    cursor: PhysicalPosition<f64>,
    occluded: bool,
    accepts_mouse: bool,
    /// Whether the main thread currently runs at interactive QoS.
    interactive_qos: bool,
}

impl App {
    fn new(opts: Options) -> Result<Self, String> {
        let pet = Pet::open(&opts.pet)?;
        let style = pet.style(opts.style.as_deref())?;
        let model = model::load(&pet.model_path())?;
        let behavior = Behavior::new(&model, &pet.manifest)?;
        let facing = Quat::from_rotation_y(pet.manifest.facing.to_radians());
        let now = Instant::now();
        Ok(Self {
            pose: Pose::rest(&model),
            model,
            style,
            behavior,
            facing,
            camera: None,
            height: 1.0,
            window: None,
            renderer: None,
            scheduler: Scheduler::new(opts.power),
            started: now,
            last_frame: now,
            next_hit_poll: now,
            yaw: 0.0,
            spinning: false,
            cursor: PhysicalPosition::new(0.0, 0.0),
            occluded: false,
            accepts_mouse: true,
            interactive_qos: false,
            pet,
        })
    }

    fn placement(&self) -> Mat4 {
        scene::placement(self.yaw, self.facing, self.behavior.breath_phase)
    }

    /// The cursor relative to the window, in -1..1 with y up, if known.
    fn cursor_ndc(&self) -> Option<Vec2> {
        let window = self.window.as_ref()?;
        let (x, y) = cursor::position_in(window)?;
        let size = window.inner_size();
        Some(Vec2::new(
            (x / size.width as f64 * 2.0 - 1.0) as f32,
            (1.0 - y / size.height as f64 * 2.0) as f32,
        ))
    }

    /// Whether the cursor is over the pet as last drawn.
    fn cursor_on_pet(&self) -> bool {
        let (Some(ndc), Some(camera)) = (self.cursor_ndc(), &self.camera) else {
            return false;
        };
        if ndc.abs().max_element() > 1.0 {
            return false;
        }
        let (origin, dir) = camera.ray(ndc);
        let inv = self.placement().inverse();
        let positions = pose::skinned_positions(&self.model, &self.pose);
        hit::ray_mesh(
            inv.transform_point3(origin),
            inv.transform_vector3(dir).normalize(),
            &positions,
            &self.model.indices,
            self.height * HIT_SLACK,
        )
    }

    /// Lets clicks on the transparent area fall through to the windows below.
    fn update_click_through(&mut self) {
        let Some(window) = self.window.clone() else {
            return;
        };
        // Keep the mouse while a spin is in progress, even if the cursor slips off the pet.
        let over = self.spinning || self.cursor_on_pet();
        if over != self.accepts_mouse {
            self.accepts_mouse = over;
            let _ = window.set_cursor_hittest(over);
        }
        if over {
            self.scheduler.touch();
        }
    }

    fn draw(&mut self) {
        let now = Instant::now();
        let dt = (now - self.last_frame).as_secs_f32().min(0.25);
        self.last_frame = now;
        let dozing = self.scheduler.activity() == Activity::Dozing;
        let gaze = self.cursor_ndc();
        self.behavior.update(&self.model, dt, gaze, dozing);
        self.pose = self
            .behavior
            .pose(&self.model, Quat::from_rotation_y(self.yaw) * self.facing);
        let joints = pose::joint_matrices(&self.model, &self.pose);
        let input = FrameInput {
            model: self.placement(),
            joints: &joints,
            morphs: &self.pose.morphs,
            time: (now - self.started).as_secs_f32(),
        };
        if let Some(renderer) = &mut self.renderer {
            renderer.render(&input);
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let camera = scene::framing(&self.model, &self.style, self.facing);
        let height = self.pet.manifest.height as f64;
        let attrs = WindowAttributes::default()
            .with_title(&self.pet.manifest.name)
            .with_inner_size(LogicalSize::new(height * camera.aspect as f64, height))
            .with_transparent(true)
            .with_decorations(false)
            .with_resizable(false)
            .with_has_shadow(false)
            .with_window_level(WindowLevel::AlwaysOnTop);
        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));
        self.height = scene::model_height(&self.model);
        let render_camera = scene::framing(&self.model, &self.style, self.facing);
        self.renderer = Some(pollster::block_on(Renderer::new(
            window.clone(),
            &self.model,
            self.style.clone(),
            render_camera,
            self.height,
        )));
        self.camera = Some(camera);
        window.request_redraw();
        self.window = Some(window);
        eprintln!(
            "deskpet: {} ({:?}), {} vertices, {} joints, {} morphs, clips [{}], power {:?}",
            self.pet.manifest.name,
            self.style.kind,
            self.model.vertices.len(),
            self.model.palette.len(),
            self.model.morph_names.len(),
            self.model
                .clips
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            self.scheduler.profile,
        );
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        let Some(window) = self.window.clone() else {
            return;
        };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(NamedKey::Escape),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(r) = &mut self.renderer {
                    r.resize(size.width, size.height);
                }
            }
            WindowEvent::Occluded(occluded) => {
                self.occluded = occluded;
                if !occluded {
                    window.request_redraw();
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                self.scheduler.touch();
                match (button, state) {
                    // A left press reacts, and dragging moves the pet around the desktop.
                    (MouseButton::Left, ElementState::Pressed) => {
                        self.behavior.clicked();
                        let _ = window.drag_window();
                    }
                    // Right drag spins it.
                    (MouseButton::Right, s) => self.spinning = s == ElementState::Pressed,
                    _ => {}
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                if self.spinning {
                    self.yaw += (position.x - self.cursor.x) as f32 * 0.01;
                    self.scheduler.touch();
                    window.request_redraw();
                }
                self.cursor = position;
            }
            WindowEvent::RedrawRequested if !self.occluded => self.draw(),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none() {
            return;
        }
        if self.occluded {
            // Hidden: render nothing and sleep until the OS tells us we're visible.
            event_loop.set_control_flow(ControlFlow::Wait);
            return;
        }
        let now = Instant::now();
        if now >= self.next_hit_poll {
            self.update_click_through();
            self.next_hit_poll = now + HIT_POLL;
        }
        let interactive = self.scheduler.activity() == Activity::Active;
        if interactive != self.interactive_qos {
            set_qos(interactive);
            self.interactive_qos = interactive;
        }
        if self.scheduler.frame_due(now, self.behavior.busy())
            && let Some(window) = &self.window
        {
            window.request_redraw();
        }
        let wake = self.scheduler.next_frame().min(self.next_hit_poll);
        event_loop.set_control_flow(ControlFlow::WaitUntil(wake));
    }
}

const USAGE: &str = "\
usage:
  deskpet [run] [--pet DIR] [--style NAME] [--power eco|balanced|smooth]
      Run the pet on the desktop.
  deskpet render DIR [--out FILE] [--style NAME]... [--clip NAME]... [--columns N] [--cell PX]
      Render a contact sheet PNG: turntable, every clip, head look and blink.
  deskpet check DIR [--json] [--scale N]
      Check a pet package. Exit status 1 on errors, 0 otherwise.
  deskpet new DIR --model FILE.glb [--name NAME]
      Make a pet package from a glTF binary, with guessed settings to confirm.";

fn usage() -> ! {
    eprintln!("{USAGE}");
    std::process::exit(2);
}

fn fail(e: String) -> ! {
    eprintln!("deskpet: {e}");
    std::process::exit(1);
}

/// Splits `--flag value` pairs; returns positional arguments and flags.
fn split_args(
    args: impl Iterator<Item = String>,
    switches: &[&str],
) -> (Vec<String>, Vec<(String, String)>) {
    let mut positional = Vec::new();
    let mut flags = Vec::new();
    let mut args = args.peekable();
    while let Some(a) = args.next() {
        if a == "-h" || a == "--help" {
            println!("{USAGE}");
            std::process::exit(0);
        } else if switches.contains(&a.as_str()) {
            flags.push((a, String::new()));
        } else if a.starts_with("--") {
            let v = args.next().unwrap_or_else(|| usage());
            flags.push((a, v));
        } else {
            positional.push(a);
        }
    }
    (positional, flags)
}

/// Asks the kernel to schedule the app as background-ish work: it runs on the
/// efficiency cores and at lower clocks unless the user is interacting, which
/// is when frame time matters.
fn set_qos(interactive: bool) {
    let class = if interactive {
        libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE
    } else {
        libc::qos_class_t::QOS_CLASS_UTILITY
    };
    // SAFETY: only changes the calling thread's scheduling class.
    unsafe {
        libc::pthread_set_qos_class_self_np(class, 0);
    }
}

fn run_app(flags: Vec<(String, String)>) {
    let mut opts = Options {
        pet: PathBuf::from("pets/fox"),
        style: None,
        power: PowerProfile::Balanced,
    };
    for (k, v) in flags {
        match k.as_str() {
            "--pet" => opts.pet = PathBuf::from(v),
            "--style" => opts.style = Some(v),
            "--power" => opts.power = PowerProfile::parse(&v).unwrap_or_else(|| usage()),
            _ => usage(),
        }
    }
    let mut app = App::new(opts).unwrap_or_else(|e| fail(e));
    set_qos(false);
    let event_loop = EventLoop::new().expect("event loop");
    event_loop.run_app(&mut app).expect("run app");
}

fn render_sheet(dir: PathBuf, flags: Vec<(String, String)>) {
    let mut opts = sheet::SheetOptions {
        styles: Vec::new(),
        clips: Vec::new(),
        columns: 6,
        cell_height: 360,
    };
    let mut out = dir.join("contact.png");
    for (k, v) in flags {
        match k.as_str() {
            "--out" => out = PathBuf::from(v),
            "--style" => opts.styles.push(v),
            "--clip" => opts.clips.push(v),
            "--columns" => opts.columns = v.parse().unwrap_or_else(|_| usage()),
            "--cell" => opts.cell_height = v.parse().unwrap_or_else(|_| usage()),
            _ => usage(),
        }
    }
    let pet = Pet::open(&dir).unwrap_or_else(|e| fail(e));
    let model = model::load(&pet.model_path()).unwrap_or_else(|e| fail(e));
    let report = sheet::render(&pet, &model, &opts, &out).unwrap_or_else(|e| fail(e));
    println!(
        "{}: {}x{}, {} cells, median {:.2?} per frame",
        out.display(),
        report.width,
        report.height,
        report.cells,
        report.median_frame
    );
}

fn check_pet(dir: PathBuf, flags: Vec<(String, String)>) {
    let mut json = false;
    // Retina windows are drawn at twice their size in points.
    let mut scale = 2.0;
    for (k, v) in flags {
        match k.as_str() {
            "--json" => json = true,
            "--scale" => scale = v.parse().unwrap_or_else(|_| usage()),
            _ => usage(),
        }
    }
    let findings = check::run(&dir, scale);
    if json {
        println!("{}", findings.to_json());
    } else {
        for f in &findings.0 {
            let tag = match f.level {
                check::Level::Ok => "ok   ",
                check::Level::Warn => "warn ",
                check::Level::Error => "ERROR",
            };
            println!("{tag} {:<12} {}", f.check, f.message);
        }
    }
    if findings.worst() == check::Level::Error {
        std::process::exit(1);
    }
}

fn new_pet(dir: PathBuf, flags: Vec<(String, String)>) {
    let mut source = None;
    let mut name = None;
    for (k, v) in flags {
        match k.as_str() {
            "--model" => source = Some(PathBuf::from(v)),
            "--name" => name = Some(v),
            _ => usage(),
        }
    }
    let source = source.unwrap_or_else(|| usage());
    scaffold::create(&dir, &source, name.as_deref()).unwrap_or_else(|e| fail(e));
    println!(
        "{}: created. Next: deskpet check {0} && deskpet render {0}",
        dir.display()
    );
}

fn main() {
    let mut args = std::env::args().skip(1).peekable();
    let command = match args.peek().map(String::as_str) {
        Some("run" | "render" | "check" | "new") => args.next(),
        _ => None,
    };
    let (positional, flags) = split_args(args, &["--json"]);
    let one_dir = |p: Vec<String>| match p.as_slice() {
        [d] => PathBuf::from(d),
        _ => usage(),
    };
    match command.as_deref() {
        None | Some("run") if positional.is_empty() => run_app(flags),
        Some("render") => render_sheet(one_dir(positional), flags),
        Some("check") => check_pet(one_dir(positional), flags),
        Some("new") => new_pet(one_dir(positional), flags),
        _ => usage(),
    }
}

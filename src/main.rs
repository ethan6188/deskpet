//! deskpet: a transparent, always-on-top 3D desktop pet for macOS.

mod behavior;
mod camera;
mod cursor;
mod hit;
mod model;
mod pet;
mod pose;
mod renderer;
mod schedule;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use glam::{Mat4, Quat, Vec2, Vec3};
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
            pet,
        })
    }

    /// Bounds over the rest pose and every clip, at any spin, so the pet never
    /// walks out of its window.
    fn framing(&self) -> Camera {
        let mut lo = Vec3::splat(f32::MAX);
        let mut hi = Vec3::splat(f32::MIN);
        let mut add = |pose: &Pose| {
            for p in pose::skinned_positions(&self.model, pose) {
                lo = lo.min(p);
                hi = hi.max(p);
            }
        };
        add(&Pose::rest(&self.model));
        for clip in &self.model.clips {
            for k in 0..=12 {
                let mut pose = Pose::rest(&self.model);
                pose::sample(
                    &self.model,
                    clip,
                    clip.duration * k as f32 / 12.0,
                    &mut pose,
                );
                add(&pose);
            }
        }
        // Spinning about Y sweeps a circle; frame the circle, centered on the axis.
        let r = Vec2::new(lo.x.abs().max(hi.x.abs()), lo.z.abs().max(hi.z.abs())).length();
        let fur = self.style.fur.max(self.style.outline) * (hi.y - lo.y);
        Camera::fit(Vec3::new(-r, lo.y, -r) - fur, Vec3::new(r, hi.y, r) + fur)
    }

    fn placement(&self) -> Mat4 {
        // Breathing: a slow vertical squash-and-stretch about the feet that keeps volume.
        let b = 1.0 + 0.015 * self.behavior.breath_phase.sin();
        Mat4::from_quat(Quat::from_rotation_y(self.yaw) * self.facing)
            * Mat4::from_scale(Vec3::new(1.0 / b.sqrt(), b, 1.0 / b.sqrt()))
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
        let camera = self.framing();
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
        let (lo, hi) = self
            .model
            .vertices
            .iter()
            .fold((f32::MAX, f32::MIN), |(lo, hi), v| {
                (lo.min(v.position[1]), hi.max(v.position[1]))
            });
        self.height = hi - lo;
        let render_camera = self.framing();
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
        if self.scheduler.frame_due(now, self.behavior.busy())
            && let Some(window) = &self.window
        {
            window.request_redraw();
        }
        let wake = self.scheduler.next_frame().min(self.next_hit_poll);
        event_loop.set_control_flow(ControlFlow::WaitUntil(wake));
    }
}

fn usage() -> ! {
    eprintln!("usage: deskpet [--pet DIR] [--style NAME] [--power eco|balanced|smooth]");
    std::process::exit(2);
}

fn parse_args() -> Options {
    let mut opts = Options {
        pet: PathBuf::from("pets/fox"),
        style: None,
        power: PowerProfile::Balanced,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| usage());
        match arg.as_str() {
            "--pet" => opts.pet = PathBuf::from(value()),
            "--style" => opts.style = Some(value()),
            "--power" => opts.power = PowerProfile::parse(&value()).unwrap_or_else(|| usage()),
            _ => usage(),
        }
    }
    opts
}

fn main() {
    let app = App::new(parse_args());
    let mut app = app.unwrap_or_else(|e| {
        eprintln!("deskpet: {e}");
        std::process::exit(1);
    });
    let event_loop = EventLoop::new().expect("event loop");
    event_loop.run_app(&mut app).expect("run app");
}

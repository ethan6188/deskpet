//! deskpet: a transparent, always-on-top 3D desktop pet for macOS.

mod camera;
mod cursor;
mod mesh;
mod renderer;
mod schedule;

use std::sync::Arc;
use std::time::{Duration, Instant};

use glam::{Mat4, Vec2, Vec3};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::platform::macos::WindowAttributesExtMacOS;
use winit::window::{Window, WindowAttributes, WindowId, WindowLevel};

use mesh::Mesh;
use renderer::Renderer;
use schedule::{Activity, PowerProfile, Scheduler};

/// Logical height of the pet window; the width follows the pet's proportions.
const WINDOW_HEIGHT: f64 = 320.0;
/// How often to check whether the cursor is over the pet while it ignores the mouse.
const HIT_POLL: Duration = Duration::from_millis(100);

struct App {
    profile: PowerProfile,
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    scheduler: Scheduler,
    last_frame: Instant,
    next_hit_poll: Instant,
    /// Breathing phase in radians, advanced by frame time so slowing down never jumps.
    breath_phase: f32,
    yaw: f32,
    spinning: bool,
    cursor: PhysicalPosition<f64>,
    occluded: bool,
    accepts_mouse: bool,
}

impl App {
    fn new(profile: PowerProfile) -> Self {
        let now = Instant::now();
        Self {
            profile,
            window: None,
            renderer: None,
            scheduler: Scheduler::new(profile),
            last_frame: now,
            next_hit_poll: now,
            breath_phase: 0.0,
            yaw: 0.0,
            spinning: false,
            cursor: PhysicalPosition::new(0.0, 0.0),
            occluded: false,
            accepts_mouse: true,
        }
    }

    fn model(&self) -> Mat4 {
        // Breathing: a slow vertical squash-and-stretch that keeps volume.
        let breath = 1.0 + 0.02 * self.breath_phase.sin();
        Mat4::from_rotation_y(self.yaw)
            * Mat4::from_scale(Vec3::new(1.0 / breath.sqrt(), breath, 1.0 / breath.sqrt()))
    }

    /// Whether a point in window pixels lands on the pet rather than the transparent area.
    fn hits_pet(&self, x: f64, y: f64) -> bool {
        let (Some(window), Some(renderer)) = (&self.window, &self.renderer) else {
            return false;
        };
        let size = window.inner_size();
        if x < 0.0 || y < 0.0 || x >= size.width as f64 || y >= size.height as f64 {
            return false;
        }
        let ndc = Vec2::new(
            (x / size.width as f64 * 2.0 - 1.0) as f32,
            (1.0 - y / size.height as f64 * 2.0) as f32,
        );
        let (origin, dir) = renderer.camera.ray(ndc);
        // Test in the pet's own space, so spinning and breathing are accounted for.
        let inv = self.model().inverse();
        let (origin, dir) = (inv.transform_point3(origin), inv.transform_vector3(dir));
        renderer.hit_shapes.iter().any(|s| s.hit(origin, dir))
    }

    /// Lets clicks on the transparent area fall through to the windows below.
    fn update_click_through(&mut self) {
        let Some(window) = &self.window else { return };
        // Keep the mouse while a spin is in progress, even if the cursor slips off the pet.
        let over =
            self.spinning || cursor::position_in(window).is_some_and(|(x, y)| self.hits_pet(x, y));
        if over != self.accepts_mouse {
            self.accepts_mouse = over;
            let _ = window.set_cursor_hittest(over);
        }
        if over {
            self.scheduler.touch();
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let mesh = Mesh::placeholder_pet();
        let (lo, hi) = mesh.bounds();
        let aspect = camera::Camera::fit(lo, hi).aspect as f64;
        let attrs = WindowAttributes::default()
            .with_title("deskpet")
            .with_inner_size(LogicalSize::new(WINDOW_HEIGHT * aspect, WINDOW_HEIGHT))
            .with_transparent(true)
            .with_decorations(false)
            .with_resizable(false)
            .with_has_shadow(false)
            .with_window_level(WindowLevel::AlwaysOnTop);
        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));
        self.renderer = Some(pollster::block_on(Renderer::new(window.clone(), mesh)));
        window.request_redraw();
        self.window = Some(window);
        eprintln!("deskpet: power profile {:?}", self.profile);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        let (Some(window), Some(renderer)) = (&self.window, &mut self.renderer) else {
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
            WindowEvent::Resized(size) => renderer.resize(size.width, size.height),
            WindowEvent::Occluded(occluded) => {
                self.occluded = occluded;
                if !occluded {
                    window.request_redraw();
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                self.scheduler.touch();
                match (button, state) {
                    // Left drag moves the pet around the desktop.
                    (MouseButton::Left, ElementState::Pressed) => {
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
            WindowEvent::RedrawRequested if !self.occluded => {
                let now = Instant::now();
                let dt = (now - self.last_frame).as_secs_f32().min(0.25);
                self.last_frame = now;
                // Dozing pets breathe slower and deeper-feeling.
                let rate = match self.scheduler.activity() {
                    Activity::Dozing => 1.2,
                    _ => 2.2,
                };
                self.breath_phase = (self.breath_phase + dt * rate) % std::f32::consts::TAU;
                let model = self.model();
                if let Some(renderer) = &mut self.renderer {
                    renderer.render(model);
                }
            }
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
        if self.scheduler.frame_due(now)
            && let Some(window) = &self.window
        {
            window.request_redraw();
        }
        let wake = self.scheduler.next_frame().min(self.next_hit_poll);
        event_loop.set_control_flow(ControlFlow::WaitUntil(wake));
    }
}

fn main() {
    let mut profile = PowerProfile::Balanced;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--power" => {
                let value = args.next().unwrap_or_default();
                profile = PowerProfile::parse(&value).unwrap_or_else(|| {
                    eprintln!("deskpet: --power expects eco, balanced or smooth");
                    std::process::exit(2);
                });
            }
            other => {
                eprintln!("deskpet: unknown argument {other}");
                std::process::exit(2);
            }
        }
    }
    let event_loop = EventLoop::new().expect("event loop");
    event_loop.run_app(&mut App::new(profile)).expect("run app");
}

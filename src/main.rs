//! deskpet: a transparent, always-on-top 3D desktop pet for macOS.

mod mesh;
mod renderer;

use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowAttributes, WindowId, WindowLevel};

use renderer::Renderer;

/// Frame rate while idle. Interaction raises it to the display's vsync rate.
const IDLE_FPS: u64 = 30;
/// How long after the last interaction to stay at full frame rate.
const ACTIVE_WINDOW: Duration = Duration::from_secs(2);

struct App {
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    start: Instant,
    last_input: Instant,
    yaw: f32,
    dragging: bool,
    cursor: PhysicalPosition<f64>,
    occluded: bool,
}

impl App {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            window: None,
            renderer: None,
            start: now,
            last_input: now,
            yaw: 0.0,
            dragging: false,
            cursor: PhysicalPosition::new(0.0, 0.0),
            occluded: false,
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = WindowAttributes::default()
            .with_title("deskpet")
            .with_inner_size(LogicalSize::new(320.0, 320.0))
            .with_transparent(true)
            .with_decorations(false)
            .with_resizable(false)
            .with_window_level(WindowLevel::AlwaysOnTop);
        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));
        self.renderer = Some(pollster::block_on(Renderer::new(window.clone())));
        window.request_redraw();
        self.window = Some(window);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        let (Some(window), Some(renderer)) = (&self.window, &mut self.renderer) else {
            return;
        };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => renderer.resize(size.width, size.height),
            WindowEvent::Occluded(occluded) => self.occluded = occluded,
            WindowEvent::MouseInput { state, button, .. } => {
                self.last_input = Instant::now();
                match (button, state) {
                    // Left drag moves the pet around the desktop.
                    (MouseButton::Left, ElementState::Pressed) => {
                        let _ = window.drag_window();
                    }
                    // Right drag spins it.
                    (MouseButton::Right, s) => self.dragging = s == ElementState::Pressed,
                    _ => {}
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                if self.dragging {
                    self.yaw += (position.x - self.cursor.x) as f32 * 0.01;
                    self.last_input = Instant::now();
                }
                self.cursor = position;
            }
            WindowEvent::RedrawRequested if !self.occluded => {
                renderer.render(self.start.elapsed().as_secs_f32(), self.yaw);
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let Some(window) = &self.window else { return };
        if self.occluded {
            // Hidden: render nothing and sleep until an event arrives.
            event_loop.set_control_flow(ControlFlow::Wait);
        } else if self.last_input.elapsed() < ACTIVE_WINDOW {
            window.request_redraw();
            event_loop.set_control_flow(ControlFlow::Poll);
        } else {
            window.request_redraw();
            let next = Instant::now() + Duration::from_millis(1000 / IDLE_FPS);
            event_loop.set_control_flow(ControlFlow::WaitUntil(next));
        }
    }
}

fn main() {
    let event_loop = EventLoop::new().expect("event loop");
    event_loop.run_app(&mut App::new()).expect("run app");
}

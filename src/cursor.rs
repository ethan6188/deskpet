//! Global cursor position. A click-through window receives no mouse events, so
//! the pet has to ask the window server where the cursor is.

use core_graphics::event::CGEvent;
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use winit::window::Window;

/// The cursor position in the window's physical pixels, if it can be read.
pub fn position_in(window: &Window) -> Option<(f64, f64)> {
    let source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState).ok()?;
    // Global display coordinates in points, origin at the top-left of the main display.
    let p = CGEvent::new(source).ok()?.location();
    let origin = window.inner_position().ok()?;
    let scale = window.scale_factor();
    Some((p.x * scale - origin.x as f64, p.y * scale - origin.y as f64))
}

use popugos::window::{Event as WindowEvent, MouseButton};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiEvent {
    Down { x: i32, y: i32 },
    Move { x: i32, y: i32 },
    Leave,
    Wheel { x: i32, y: i32, delta: i32 },
    Up { x: i32, y: i32 },
    /// Secondary-button press. Widgets can use this to request a context menu.
    Context { x: i32, y: i32 },
    KeyDown { scancode: u8, ch: u8, mods: u8 },
    KeyUp { scancode: u8, ch: u8, mods: u8 },
    Resize { width: u32, height: u32 },
}

impl UiEvent {
    pub fn from_window(event: WindowEvent) -> Option<Self> {
        match event {
            WindowEvent::MouseMove { x, y } => Some(Self::Move { x, y }),
            WindowEvent::MouseLeave => Some(Self::Leave),
            WindowEvent::MouseWheel { x, y, delta } => Some(Self::Wheel { x, y, delta }),
            WindowEvent::MouseDown(event) => match event.button {
                MouseButton::Right => Some(Self::Context { x: event.x, y: event.y }),
                MouseButton::Left => Some(Self::Down { x: event.x, y: event.y }),
                _ => None,
            },
            WindowEvent::MouseUp(event) => match event.button {
                MouseButton::Left => Some(Self::Up { x: event.x, y: event.y }),
                _ => None,
            },
            WindowEvent::KeyDown(event) => Some(Self::KeyDown {
                scancode: event.scancode,
                ch: event.ch,
                mods: event.modifiers,
            }),
            WindowEvent::KeyUp(event) => Some(Self::KeyUp {
                scancode: event.scancode,
                ch: event.ch,
                mods: event.modifiers,
            }),
            WindowEvent::Resize { width, height } => Some(Self::Resize { width, height }),
            WindowEvent::Close | WindowEvent::Focused(_) => None,
        }
    }
}

use crate::Color;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    pub background: Color,
    pub panel_background: Color,
    pub panel_border: Color,
    pub text: Color,
    pub label: Color,
    pub button: Color,
    pub button_hot: Color,
    pub button_down: Color,
    pub button_border: Color,
    pub input_background: Color,
    pub input_border: Color,
    pub input_border_focus: Color,
    pub scrollbar_background: Color,
    pub scrollbar_thumb: Color,
}

pub const DARK_THEME: Theme = Theme {
    background: Color::new(0x10, 0x18, 0x20),
    panel_background: Color::new(0x18, 0x22, 0x2E),
    panel_border: Color::new(0x40, 0x50, 0x60),
    text: Color::new(0xF0, 0xF0, 0xF0),
    label: Color::new(0xC8, 0xD0, 0xD8),
    button: Color::new(0x2A, 0x3A, 0x4A),
    button_hot: Color::new(0x3A, 0x5A, 0x7A),
    button_down: Color::new(0x1A, 0x2A, 0x3A),
    button_border: Color::new(0x50, 0x60, 0x70),
    input_background: Color::new(0x18, 0x20, 0x28),
    input_border: Color::new(0x50, 0x60, 0x70),
    input_border_focus: Color::new(0x3A, 0x7C, 0xA5),
    scrollbar_background: Color::new(0x28, 0x34, 0x40),
    scrollbar_thumb: Color::new(0x70, 0x80, 0x90),
};

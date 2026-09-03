use taffy::geometry::Size;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Constraints {
    pub min_width: f32,
    pub max_width: f32,
    pub min_height: f32,
    pub max_height: f32,
}

impl Constraints {
    pub const UNBOUNDED: Self = Self {
        min_width: 0.0,
        max_width: f32::INFINITY,
        min_height: 0.0,
        max_height: f32::INFINITY,
    };

    pub const fn new(min_width: f32, max_width: f32, min_height: f32, max_height: f32) -> Self {
        Self {
            min_width,
            max_width,
            min_height,
            max_height,
        }
    }

    pub fn tight(width: f32, height: f32) -> Self {
        Self::new(width, width, height, height)
    }

    pub fn width(max_width: f32) -> Self {
        Self::new(0.0, max_width, 0.0, f32::INFINITY)
    }

    pub fn height(max_height: f32) -> Self {
        Self::new(0.0, f32::INFINITY, 0.0, max_height)
    }

    pub fn clamp(self, size: Size<f32>) -> Size<f32> {
        Size {
            width: size.width.max(self.min_width).min(self.max_width),
            height: size.height.max(self.min_height).min(self.max_height),
        }
    }

    pub fn is_bounded_width(self) -> bool {
        self.max_width.is_finite()
    }
    pub fn is_bounded_height(self) -> bool {
        self.max_height.is_finite()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }

    pub fn contains(self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w as i32 && y < self.y + self.h as i32
    }

    pub fn intersect(self, other: Self) -> Option<Self> {
        let x0 = self.x.max(other.x);
        let y0 = self.y.max(other.y);
        let x1 = (self.x + self.w as i32).min(other.x + other.w as i32);
        let y1 = (self.y + self.h as i32).min(other.y + other.h as i32);
        if x1 <= x0 || y1 <= y0 {
            None
        } else {
            Some(Self::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32))
        }
    }
}

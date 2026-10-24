pub fn ease_out_cubic(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

/// Eases out past 1 and settles back, like a soft spring.
pub fn ease_out_back(t: f32) -> f32 {
    const C1: f32 = 1.2;
    const C3: f32 = C1 + 1.0;
    1.0 + C3 * (t - 1.0).powi(3) + C1 * (t - 1.0).powi(2)
}

/// Damped spring for UI motion. Values are in logical pixels.
#[derive(Clone, Copy)]
pub struct Spring {
    pub value: f32,
    pub target: f32,
    velocity: f32,
    stiffness: f32,
    damping: f32,
}

const STIFFNESS: f32 = 320.0;
// Damping ratio ~0.73: a small overshoot, like the iOS island.
const DAMPING: f32 = 26.0;
// Fixed integration step keeps the spring stable at any frame rate.
const STEP: f32 = 1.0 / 240.0;
const EPSILON: f32 = 0.05;

impl Spring {
    pub const fn new(value: f32) -> Self {
        Self { value, target: value, velocity: 0.0, stiffness: STIFFNESS, damping: DAMPING }
    }

    /// Critically damped: glides to its target without overshooting, like
    /// a page settling after a scroll.
    pub const fn smooth(value: f32) -> Self {
        // Damping ratio 1: damping = 2 * sqrt(stiffness).
        Self { value, target: value, velocity: 0.0, stiffness: 196.0, damping: 28.0 }
    }

    pub fn update(&mut self, dt: f32) {
        let mut remaining = dt;
        while remaining > 0.0 {
            let h = remaining.min(STEP);
            let force = self.stiffness * (self.target - self.value) - self.damping * self.velocity;
            self.velocity += force * h;
            self.value += self.velocity * h;
            remaining -= h;
        }
        if self.is_settled() {
            self.value = self.target;
            self.velocity = 0.0;
        }
    }

    /// Pushes the spring without moving its target, for a bounce in place.
    pub fn kick(&mut self, velocity: f32) {
        self.velocity += velocity;
    }

    pub fn is_settled(&self) -> bool {
        (self.target - self.value).abs() < EPSILON && self.velocity.abs() < EPSILON
    }
}

//! OWNER: render agent. Colors derived from the Windows theme (light/dark, accent).
//! The host only calls `Theme::from_system()`; fields are the renderer's business.

#[derive(Clone, Debug)]
pub struct Theme {
    pub dark: bool,
}

impl Theme {
    pub fn from_system() -> Self {
        Self { dark: true }
    }
}

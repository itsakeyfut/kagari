//! Logical-pixel unit.

/// A length in logical pixels.
///
/// Physical pixels = logical × scale factor; the scale is applied at paint time
/// (see [`Px::to_physical`]), so layout and styling stay resolution-independent.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Px(pub f32);

impl Px {
    /// Convert to physical pixels for a given display scale factor.
    pub fn to_physical(self, scale: f32) -> f32 {
        self.0 * scale
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn px_to_physical_should_scale_by_factor() {
        assert_eq!(Px(10.0).to_physical(2.0), 20.0);
        assert_eq!(Px(10.0).to_physical(1.0), 10.0);
    }
}

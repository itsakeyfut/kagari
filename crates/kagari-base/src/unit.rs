//! Logical-pixel unit.

/// A length in logical pixels.
///
/// Physical pixels = logical × scale factor; the scale is applied at paint time
/// (see [`Px::to_physical`]), so layout and styling stay resolution-independent.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
// Transparent so a logical-px value (de)serializes as a bare number in theme RON
// (`16.0`, not `Px(16.0)`) — cleaner authoring.
#[cfg_attr(feature = "serde", serde(transparent))]
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

//! Authoring-level layout style (#31) and its mapping to `taffy::Style` (#33).
//!
//! `LayoutStyle` is the layout intent an element records; #33 maps it onto a `taffy::Style`. The
//! mapping (`to_taffy`) is `pub(crate)` so `taffy` stays an implementation detail — the public
//! surface is kagari-owned enums and `kagari-base` value types only. The paint-side style
//! (bg/color tokens) is the styling milestone's concern (/spec Q4).
//!
//! Grid: `Display::Grid` maps through to taffy, but explicit grid-template tracks / placement are
//! a follow-up (keeping `LayoutStyle` `Copy`).

use kagari_base::{Edges, Px, Size};

/// Which layout algorithm a container uses.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Display {
    /// Flexbox (the default for kagari containers).
    #[default]
    Flex,
    /// Block layout.
    Block,
    /// CSS Grid (template/placement is a follow-up).
    Grid,
    /// Hidden: the node and its children are not laid out.
    None,
}

/// Main-axis direction of a flex container.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum FlexDirection {
    /// Lay children out along the horizontal axis (the flex default).
    #[default]
    Row,
    /// Lay children out along the vertical axis.
    Column,
}

/// Whether flex children wrap onto multiple lines.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum FlexWrap {
    #[default]
    NoWrap,
    Wrap,
    WrapReverse,
}

/// Cross-axis alignment of children.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AlignItems {
    Start,
    End,
    Center,
    Stretch,
    Baseline,
}

/// Main-axis distribution of children.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum JustifyContent {
    Start,
    End,
    Center,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
}

/// How content that overflows the node's bounds is treated.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Overflow {
    #[default]
    Visible,
    Clip,
    Hidden,
    Scroll,
}

/// The layout intent of an element. Mapped to a `taffy::Style` by [`LayoutStyle::to_taffy`].
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct LayoutStyle {
    pub display: Display,
    pub flex_direction: FlexDirection,
    pub flex_wrap: FlexWrap,
    pub flex_grow: f32,
    pub flex_shrink: f32,
    pub flex_basis: Option<Px>,
    pub gap: Px,
    pub padding: Edges,
    pub size: Option<Size>,
    pub min_size: Option<Size>,
    pub max_size: Option<Size>,
    pub align_items: Option<AlignItems>,
    pub justify_content: Option<JustifyContent>,
    pub overflow: Overflow,
}

impl Default for LayoutStyle {
    fn default() -> Self {
        Self {
            display: Display::default(),
            flex_direction: FlexDirection::default(),
            flex_wrap: FlexWrap::default(),
            flex_grow: 0.0,
            // CSS/taffy default: flex items shrink by default.
            flex_shrink: 1.0,
            flex_basis: None,
            gap: Px(0.0),
            padding: Edges::default(),
            size: None,
            min_size: None,
            max_size: None,
            align_items: None,
            justify_content: None,
            overflow: Overflow::default(),
        }
    }
}

impl LayoutStyle {
    /// Maps this style onto a `taffy::Style`. `..Default::default()` fills the fields kagari does
    /// not yet expose (grid template, margin, border, inset, …). Takes `self` by value
    /// (`LayoutStyle` is `Copy`).
    pub(crate) fn to_taffy(self) -> taffy::Style {
        taffy::Style {
            display: map_display(self.display),
            flex_direction: map_flex_direction(self.flex_direction),
            flex_wrap: map_flex_wrap(self.flex_wrap),
            flex_grow: self.flex_grow,
            flex_shrink: self.flex_shrink,
            flex_basis: dim(self.flex_basis.map(|p| p.0)),
            gap: taffy::Size {
                width: taffy::LengthPercentage::length(self.gap.0),
                height: taffy::LengthPercentage::length(self.gap.0),
            },
            padding: taffy::Rect {
                left: taffy::LengthPercentage::length(self.padding.left),
                right: taffy::LengthPercentage::length(self.padding.right),
                top: taffy::LengthPercentage::length(self.padding.top),
                bottom: taffy::LengthPercentage::length(self.padding.bottom),
            },
            size: taffy_size(self.size),
            min_size: taffy_size(self.min_size),
            max_size: taffy_size(self.max_size),
            align_items: self.align_items.map(map_align_items),
            justify_content: self.justify_content.map(map_justify_content),
            overflow: taffy::Point {
                x: map_overflow(self.overflow),
                y: map_overflow(self.overflow),
            },
            ..taffy::Style::default()
        }
    }
}

fn dim(value: Option<f32>) -> taffy::Dimension {
    match value {
        Some(v) => taffy::Dimension::length(v),
        None => taffy::Dimension::auto(),
    }
}

fn taffy_size(size: Option<Size>) -> taffy::Size<taffy::Dimension> {
    taffy::Size {
        width: dim(size.map(|s| s.w)),
        height: dim(size.map(|s| s.h)),
    }
}

fn map_display(display: Display) -> taffy::Display {
    match display {
        Display::Flex => taffy::Display::Flex,
        Display::Block => taffy::Display::Block,
        Display::Grid => taffy::Display::Grid,
        Display::None => taffy::Display::None,
    }
}

fn map_flex_direction(direction: FlexDirection) -> taffy::FlexDirection {
    match direction {
        FlexDirection::Row => taffy::FlexDirection::Row,
        FlexDirection::Column => taffy::FlexDirection::Column,
    }
}

fn map_flex_wrap(wrap: FlexWrap) -> taffy::FlexWrap {
    match wrap {
        FlexWrap::NoWrap => taffy::FlexWrap::NoWrap,
        FlexWrap::Wrap => taffy::FlexWrap::Wrap,
        FlexWrap::WrapReverse => taffy::FlexWrap::WrapReverse,
    }
}

fn map_align_items(align: AlignItems) -> taffy::AlignItems {
    match align {
        AlignItems::Start => taffy::AlignItems::START,
        AlignItems::End => taffy::AlignItems::END,
        AlignItems::Center => taffy::AlignItems::CENTER,
        AlignItems::Stretch => taffy::AlignItems::STRETCH,
        AlignItems::Baseline => taffy::AlignItems::BASELINE,
    }
}

fn map_justify_content(justify: JustifyContent) -> taffy::JustifyContent {
    match justify {
        JustifyContent::Start => taffy::JustifyContent::START,
        JustifyContent::End => taffy::JustifyContent::END,
        JustifyContent::Center => taffy::JustifyContent::CENTER,
        JustifyContent::SpaceBetween => taffy::JustifyContent::SPACE_BETWEEN,
        JustifyContent::SpaceAround => taffy::JustifyContent::SPACE_AROUND,
        JustifyContent::SpaceEvenly => taffy::JustifyContent::SPACE_EVENLY,
    }
}

fn map_overflow(overflow: Overflow) -> taffy::Overflow {
    match overflow {
        Overflow::Visible => taffy::Overflow::Visible,
        Overflow::Clip => taffy::Overflow::Clip,
        Overflow::Hidden => taffy::Overflow::Hidden,
        Overflow::Scroll => taffy::Overflow::Scroll,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_style_should_map_display_and_flex_to_taffy() {
        let style = LayoutStyle {
            display: Display::Flex,
            flex_direction: FlexDirection::Column,
            gap: Px(8.0),
            size: Some(Size { w: 100.0, h: 50.0 }),
            justify_content: Some(JustifyContent::SpaceBetween),
            ..LayoutStyle::default()
        };
        let taffy_style = style.to_taffy();

        assert_eq!(taffy_style.display, taffy::Display::Flex);
        assert_eq!(taffy_style.flex_direction, taffy::FlexDirection::Column);
        assert_eq!(taffy_style.size.width, taffy::Dimension::length(100.0));
        assert_eq!(taffy_style.size.height, taffy::Dimension::length(50.0));
        assert_eq!(
            taffy_style.justify_content,
            Some(taffy::JustifyContent::SPACE_BETWEEN)
        );
    }

    #[test]
    fn layout_style_default_should_shrink() {
        // CSS/taffy default: flex items shrink (1.0), not 0.0 (the naive numeric default).
        assert_eq!(LayoutStyle::default().flex_shrink, 1.0);
    }
}

//! Layer-2 integration tests (#46, rules/test.md): exercise kagari-style through its **public API**
//! only (as an external consumer would), with a real theme RON fixture. The pure per-method logic is
//! unit-tested inline (resolve.rs / theme.rs / styled.rs / token.rs); these tests cover the
//! load → resolve → validate path end-to-end and tie the macro (#41) → resolution (#40) → theme (#45)
//! layers together. No GPU (the pixel golden is #144).

use kagari_base::{Color, Px};
use kagari_style::{
    ColorRole, FontSizeStep, RadiusStep, SpacingStep, Style, StyleError, Styled, Theme,
};

/// The bundled fixture: a self-contained full theme (palette + scales + all 16 roles).
const SAMPLE: &str = include_str!("fixtures/sample-theme.ron");

#[test]
fn sample_theme_ron_should_round_trip_and_validate() {
    let theme = Theme::from_ron(SAMPLE).expect("fixture parses");
    // Palette + role mapping survive the round-trip.
    assert!(theme.primitives.colors.contains_key("blue-500"));
    assert_eq!(
        theme.roles.colors.get(&ColorRole::Accent),
        Some(&"blue-500".to_string())
    );
    // Scales resolve to the authored values (bare-number RON via Px's serde transparent, RK-010).
    assert_eq!(theme.resolve_spacing(SpacingStep::S4), Px(16.0));
    assert_eq!(theme.resolve_radius(RadiusStep::Lg), Px(8.0));
    assert_eq!(theme.resolve_font_size(FontSizeStep::Base), Px(16.0));
    // Every ColorRole is mapped, so the fixture is a complete theme.
    theme.validate().expect("fixture maps every role");
}

#[test]
fn sample_theme_accent_should_resolve_srgb_to_linear() {
    // Accent → blue-500 (#3b82f6). sRGB (0.2314, 0.5098, 0.9647) decodes to ~(0.044, 0.223, 0.921).
    let c = Theme::from_ron(SAMPLE)
        .unwrap()
        .resolve_color(ColorRole::Accent);
    let close = |a: f32, b: f32| (a - b).abs() < 0.01;
    assert!(close(c.r, 0.044), "r {}", c.r);
    assert!(close(c.g, 0.223), "g {}", c.g);
    assert!(close(c.b, 0.921), "b {}", c.b);
    assert_eq!(c.a, 1.0);
}

#[test]
fn theme_swap_should_change_resolved_color() {
    // The style-layer "swap" is resolving the same role against a different Theme value (the reactive
    // signal-driven swap is tested in kagari-core, #43). Surface is white in light, slate-900 in dark.
    assert_ne!(
        Theme::light().resolve_color(ColorRole::Surface),
        Theme::dark().resolve_color(ColorRole::Surface)
    );
}

#[test]
fn invalid_hex_theme_ron_should_error() {
    let bad = r##"( primitives: ( colors: { "x": "#zz" } ), roles: ( colors: {} ) )"##;
    assert!(matches!(
        Theme::from_ron(bad),
        Err(StyleError::ThemeParse(_))
    ));
}

#[test]
fn incomplete_theme_should_fail_validation() {
    // Parses fine, but no roles are mapped → validate reports the first unmapped role.
    let theme =
        Theme::from_ron(r##"( primitives: ( colors: {} ), roles: ( colors: {} ) )"##).unwrap();
    assert!(matches!(theme.validate(), Err(StyleError::UnknownRole(_))));
}

#[test]
fn styled_tokens_should_resolve_through_theme() {
    // A minimal consumer of the public `Styled` trait: record tokens, then resolve them against a
    // theme — exercising macro output (#41) → resolution (#40) → built-in theme (#45) end-to-end.
    #[derive(Default)]
    struct Probe {
        style: Style,
    }
    impl Styled for Probe {
        fn style_mut(&mut self) -> &mut Style {
            &mut self.style
        }
    }

    let probe = Probe::default().gap_2().bg(ColorRole::Accent);
    assert_eq!(probe.style.layout.gap, Some(SpacingStep::S2));
    assert_eq!(probe.style.paint.bg, Some(ColorRole::Accent));

    // The recorded tokens resolve against the built-in light theme to its concrete values.
    let theme = Theme::light();
    assert_eq!(
        theme.resolve_spacing(probe.style.layout.gap.unwrap()),
        Px(8.0) // Tailwind gap-2 = 8px
    );
    // The recorded role resolves to a real, opaque palette color — not the unmapped sentinel.
    let bg = theme.resolve_color(probe.style.paint.bg.unwrap());
    assert_eq!(
        bg.a, 1.0,
        "the recorded role resolves to an opaque themed color"
    );
    assert_ne!(
        bg,
        Color::from_srgb([1.0, 0.0, 1.0, 1.0]),
        "resolved to a real palette color, not the unmapped magenta sentinel"
    );
}

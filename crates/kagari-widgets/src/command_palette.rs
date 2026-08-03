//! The [`CommandPalette`] widget (#232): a modal, fuzzy-search command overlay (VS Code / Unreal style).
//!
//! `command_palette(open, commands)` is a centered modal on the portal substrate (#219: backdrop + dismiss on
//! outside/Esc + focus-trap) holding an auto-focused `text_input` filter (#72) and a **fuzzy-ranked** result
//! list (a `scroll_view`-capped column, rebuilt per filter like [`Combobox`]; result-list virtualization (#86)
//! is a follow-up). Typing filters + ranks; Up/Down move the selection; Enter runs the selected command's
//! `action`; Esc / an outside press dismisses. Open state is **controlled** via `RwSignal<bool>` (D2), so the
//! host binds it to a shortcut (e.g. Ctrl+P).
//!
//! Structure mirrors [`Combobox`]: the field keeps focus so typing works, and Arrow/Enter/Esc **bubble** out of
//! the editor leaf (#312) to the panel's `on_key_down`, which drives navigation/run. Matched characters are
//! highlighted (color-only — `text` has no weight); the selected row is `Accent`/`AccentFg`, and matched-char
//! emphasis (`Accent`) shows on the non-selected rows (RK-030-safe). The capped result list scrolls to keep the
//! keyboard selection in view.
//!
//! [`Combobox`]: crate::Combobox

use std::rc::Rc;

use kagari_base::{Point, Px, SharedString, Size};
use kagari_core::element::{dyn_if, dyn_list};
use kagari_core::reactive::prelude::*;
use kagari_core::reactive::{RwSignal, create_effect, rx};
use kagari_core::{AnyElement, IntoElement, KeyCode, ScrollHandle, div, overlay, text};
use kagari_style::{ColorRole, Styled};

use crate::control::{ControlSize, label_px};
use crate::scroll_view;
use crate::text_input;

/// A command's action, run on Enter / click. `Rc` (single-threaded UI): shared so the panel can rebuild each
/// open/filter while the callback stays callable (mirrors the Menu `OnSelect`).
type Action = Rc<dyn Fn()>;

/// One entry in a [`CommandPalette`]: a display `name`, optional search-only `keywords` (aliases that also match
/// but are not shown), and an `action` run when the command is chosen.
pub struct Command {
    name: SharedString,
    keywords: Vec<SharedString>,
    action: Action,
}

/// Creates a command with a display `name` and an `action` run when it is chosen. Add search aliases with
/// [`.keyword`](Command::keyword).
pub fn command(name: impl Into<SharedString>, action: impl Fn() + 'static) -> Command {
    Command {
        name: name.into(),
        keywords: Vec::new(),
        action: Rc::new(action),
    }
}

impl Command {
    /// Adds a search-only alias: the command also matches queries against this keyword (e.g. "write" for a
    /// "Save" command), but only the `name` is displayed (and highlighted).
    pub fn keyword(mut self, keyword: impl Into<SharedString>) -> Self {
        self.keywords.push(keyword.into());
        self
    }
}

/// One ranked result: the source command index, the matched char positions in its `name` (for highlighting;
/// empty when it matched only via a keyword), and the fuzzy score.
struct Match {
    cmd: usize,
    positions: Vec<usize>,
    score: i32,
}

/// A subsequence fuzzy match of `query` in `haystack` (ASCII case-insensitive). Returns `None` if `query` is not
/// a subsequence of `haystack`; otherwise `(score, positions)` where a higher score is a better match — matched
/// characters that are **contiguous** or on a **word boundary** (string start, after a separator, or a camelCase
/// hump) score higher — and `positions` are the matched **char** indices in `haystack` (for highlighting). An
/// empty query trivially matches with score `0` and no positions.
fn fuzzy_score(haystack: &str, query: &str) -> Option<(i32, Vec<usize>)> {
    if query.is_empty() {
        return Some((0, Vec::new()));
    }
    let hay: Vec<char> = haystack.chars().collect();
    let q: Vec<char> = query.chars().collect();
    let mut qi = 0usize;
    let mut positions = Vec::with_capacity(q.len());
    let mut score = 0i32;
    let mut prev_match: Option<usize> = None;
    for (i, &hc) in hay.iter().enumerate() {
        if qi >= q.len() {
            break;
        }
        if hc.eq_ignore_ascii_case(&q[qi]) {
            let mut s = 1;
            // Contiguous with the previous match (adjacent chars) — the strongest signal.
            if prev_match == Some(i.wrapping_sub(1)) {
                s += 5;
            }
            // Word boundary: string start, after a separator, or a lowercase→uppercase (camelCase) hump.
            let boundary = i == 0
                || hay
                    .get(i.wrapping_sub(1))
                    .is_some_and(|&p| matches!(p, ' ' | '_' | '-' | '/' | '.'))
                || (hc.is_uppercase()
                    && hay
                        .get(i.wrapping_sub(1))
                        .is_some_and(|&p| p.is_lowercase()));
            if boundary {
                s += 10;
            }
            score += s;
            positions.push(i);
            prev_match = Some(i);
            qi += 1;
        }
    }
    (qi == q.len()).then_some((score, positions))
}

/// Ranks `commands` against `query`: keeps every command whose `name` or any `keyword` fuzzy-matches, scored by
/// the best of the two (a keyword-only match is penalised by 1 so name matches rank first, and carries no
/// highlight positions), sorted by score descending. A stable sort preserves the input order for ties (and for
/// the empty query, which matches everything with score 0).
fn rank(commands: &[Command], query: &str) -> Vec<Match> {
    let mut matches: Vec<Match> = commands
        .iter()
        .enumerate()
        .filter_map(|(i, cmd)| {
            let name_m = fuzzy_score(&cmd.name, query);
            let kw_best = cmd
                .keywords
                .iter()
                .filter_map(|k| fuzzy_score(k, query))
                .map(|(s, _)| s)
                .max();
            match (name_m, kw_best) {
                (Some((ns, positions)), kw) => Some(Match {
                    cmd: i,
                    positions,
                    score: kw.map_or(ns, |k| ns.max(k)),
                }),
                (None, Some(ks)) => Some(Match {
                    cmd: i,
                    positions: Vec::new(),
                    score: ks - 1,
                }),
                (None, None) => None,
            }
        })
        .collect();
    // Stable sort by score descending — preserves the input order for ties (and the empty query).
    matches.sort_by_key(|m| std::cmp::Reverse(m.score));
    matches
}

/// Splits `name` into consecutive `(matched, text)` runs given sorted matched char `positions` — the unit of
/// highlighting (each run becomes one `text` span). No positions → a single unmatched run of the whole name.
fn split_runs(name: &str, positions: &[usize]) -> Vec<(bool, String)> {
    let mut runs: Vec<(bool, String)> = Vec::new();
    let mut pi = 0usize;
    for (i, c) in name.chars().enumerate() {
        while pi < positions.len() && positions[pi] < i {
            pi += 1;
        }
        let matched = pi < positions.len() && positions[pi] == i;
        match runs.last_mut() {
            Some((m, s)) if *m == matched => s.push(c),
            _ => runs.push((matched, c.to_string())),
        }
    }
    runs
}

/// The row height (logical px) for `size` — matches the Table scale so palettes line up with data views.
fn row_height(size: ControlSize) -> f32 {
    match size {
        ControlSize::Sm => 24.0,
        ControlSize::Md => 28.0,
        ControlSize::Lg => 34.0,
    }
}

/// The palette's fixed content width (logical px).
const PANEL_W: f32 = 480.0;
/// The maximum result-list height before it scrolls (logical px).
const MAX_LIST_H: f32 = 320.0;

/// Builds result row `i` for match `m` (fixed `PANEL_W × row_h`, like a Table body row): the command name with
/// matched chars highlighted, tinted `Accent` when it is the selected (`highlight`) row; hover selects it, click
/// runs it.
fn build_result_row(
    i: usize,
    m: &Match,
    commands: &[Command],
    highlight: RwSignal<usize>,
    open: RwSignal<bool>,
    font: Px,
    row_h: f32,
) -> AnyElement {
    let cmd = &commands[m.cmd];
    let action = cmd.action.clone();

    // The label: a flex row of run-spans, each colored by (its match state × whether this row is selected).
    let mut label = div().flex().items_center().px_3();
    for (matched, run) in split_runs(&cmd.name, &m.positions) {
        label = label.child(
            text(SharedString::from(run))
                .size(font)
                .color_role(rx(move || {
                    if highlight.get() == i {
                        ColorRole::AccentFg
                    } else if matched {
                        ColorRole::Accent
                    } else {
                        ColorRole::Text
                    }
                })),
        );
    }

    div()
        .flex()
        .items_center()
        .size(Size {
            w: PANEL_W,
            h: row_h,
        })
        .bg(rx(move || {
            if highlight.get() == i {
                ColorRole::Accent
            } else {
                ColorRole::SurfaceRaised
            }
        }))
        .on_mouse_enter(move |_, _| highlight.set(i))
        .on_click(move |_, _| {
            action();
            open.set(false);
        })
        .child(label)
        .into_element()
}

/// A modal fuzzy-search command palette (#232). Build with [`command_palette`]; chain [`.size`](Self::size).
/// Open state is **controlled** via `RwSignal<bool>` (D2). Returns `impl IntoElement`; `Styled` is not on its
/// surface.
pub struct CommandPalette {
    open: RwSignal<bool>,
    commands: Vec<Command>,
    size: ControlSize,
}

/// Creates a command palette over `commands`, shown while `open` is `true` (#232).
pub fn command_palette(open: RwSignal<bool>, commands: Vec<Command>) -> CommandPalette {
    CommandPalette {
        open,
        commands,
        size: ControlSize::Md,
    }
}

impl CommandPalette {
    /// Sets the control size (D3) — the field padding, font size, and result row height.
    pub fn size(mut self, size: ControlSize) -> Self {
        self.size = size;
        self
    }
}

impl IntoElement for CommandPalette {
    fn into_element(self) -> AnyElement {
        let CommandPalette {
            open,
            commands,
            size,
        } = self;
        let commands = Rc::new(commands);
        let font = label_px(size);
        let row_h = row_height(size);

        // Created once, so they survive the per-open panel rebuilds. `filter` = the query, `highlight` = the
        // selected index **into the filtered results**, `scroll` = the result list's offset (built here in the
        // component body so it captures the window frame clock for smooth scroll, RK-033).
        let filter = RwSignal::new(String::new());
        let highlight = RwSignal::new(0usize);
        let scroll = ScrollHandle::new();

        // Opening resets to a clean state: selection to the top (effect C then scrolls to top) and any stale
        // query cleared (guarded equal-write so it doesn't loop; mirrors Combobox's effect). Subscribes `open`
        // only (reads `filter` untracked), so the `highlight`/`filter` writes here can't re-fire it. Resetting
        // `highlight` on open — not only on a filter change — covers reopening after arrowing with an empty query.
        create_effect(move || {
            if open.get() {
                highlight.set(0);
                if filter.with_untracked(|f| !f.is_empty()) {
                    filter.set(String::new());
                }
            }
        });
        // A filter change resets the selection to the top result.
        create_effect(move || {
            let _ = filter.get();
            highlight.set(0);
        });
        // Keep the selected row visible as the keyboard moves it (subscribes `highlight` only — the current
        // offset is read untracked so this never loops on its own `scroll_to`).
        {
            let scroll = scroll.clone();
            create_effect(move || {
                let hl = highlight.get();
                let row_top = hl as f32 * row_h;
                let row_bot = row_top + row_h;
                let cur = scroll.offset_untracked().y;
                let next = if row_top < cur {
                    row_top
                } else if row_bot > cur + MAX_LIST_H {
                    row_bot - MAX_LIST_H
                } else {
                    cur
                };
                if (next - cur).abs() > 0.5 {
                    // Instant: a ScrollView's standalone handle spring is not self-ticked (matches its wheel path).
                    scroll.jump_to(Point::new(0.0, next.max(0.0)));
                }
            });
        }

        // The panel is rebuilt each open (dyn_if gated on the same `open` as the overlay, #219). Focus enters
        // the auto-focused field; Arrow/Enter/Esc bubble from the editor leaf to the panel's `on_key_down`.
        let panel_cmds = commands.clone();
        let panel = move || {
            let nav_cmds = panel_cmds.clone();
            let list_cmds = panel_cmds.clone();
            let list_scroll = scroll.clone();

            // The filtered result list, rebuilt whenever the filter changes (single-item rebuild — the
            // Combobox/Dropdown idiom): a flex column of rows inside a `scroll_view` capped at `MAX_LIST_H`
            // (shrinking to the content when shorter). Virtualization (#86) is a follow-up — the core
            // virtualized list reuses rows by index key, which does not refresh across a changing filtered count.
            let list = dyn_list(
                move || vec![filter.get()],
                |q: &String| q.clone(),
                move |_q| {
                    let results = rank(&list_cmds, &filter.get_untracked());
                    let list_h = (results.len() as f32 * row_h).min(MAX_LIST_H);
                    let mut col = div().flex_col().min_w(PANEL_W);
                    for (idx, m) in results.iter().enumerate() {
                        col = col.child(build_result_row(
                            idx, m, &list_cmds, highlight, open, font, row_h,
                        ));
                    }
                    scroll_view(col)
                        .size(Size {
                            w: PANEL_W,
                            h: list_h,
                        })
                        .horizontal(false)
                        .handle(list_scroll.clone())
                },
            );

            div()
                .flex_col()
                .min_w(PANEL_W)
                .gap_1()
                .p_2()
                .rounded_md()
                .bg(ColorRole::SurfaceRaised)
                .border_w_1()
                .border_color(Some(ColorRole::Border))
                .shadow_lg()
                .on_key_down(move |kev, _cx| {
                    if kev.repeat {
                        return;
                    }
                    // Only Arrow/Enter/Esc bubble here (the editor consumes typed chars, #312); rank the
                    // filtered results lazily in the arms that actually need them, not on every bubbled key.
                    match kev.code {
                        KeyCode::ArrowDown => {
                            let n = rank(&nav_cmds, &filter.get_untracked()).len();
                            if n > 0 {
                                highlight.set((highlight.get_untracked() + 1).min(n - 1));
                            }
                        }
                        KeyCode::ArrowUp => {
                            highlight.set(highlight.get_untracked().saturating_sub(1));
                        }
                        KeyCode::Enter | KeyCode::NumpadEnter => {
                            let results = rank(&nav_cmds, &filter.get_untracked());
                            if let Some(m) = results.get(highlight.get_untracked()) {
                                let action = nav_cmds[m.cmd].action.clone();
                                action();
                                open.set(false);
                            }
                        }
                        KeyCode::Escape => open.set(false),
                        _ => {}
                    }
                })
                .child(
                    text_input(filter)
                        .placeholder("Type a command…")
                        .size(size)
                        .autofocus(true),
                )
                .child(list)
        };

        overlay(dyn_if(move || open.get(), panel))
            .center()
            .open(open)
            .backdrop(true)
            .dismiss_on_outside(true)
            .trap_focus(true)
            .into_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::sync::Arc as StdArc;

    use kagari_base::{NodeId, Point, Size as BSize};
    use kagari_core::arena::Arena;
    use kagari_core::damage::DamageState;
    use kagari_core::event::FocusRegistry;
    use kagari_core::overlay::{OverlayLayers, OverlayRegistry};
    use kagari_core::paint::render_tree;
    use kagari_core::reactive::{Owner, provide_context};
    use kagari_core::{
        DispatchState, HitTest, KeyEvent, Modifiers, MouseButton, MouseEvent, MouseKind,
        dispatch_key, dispatch_mouse,
    };
    use kagari_layout::LayoutTree;
    use kagari_render::Scene;
    use kagari_style::Theme;
    use kagari_text::{FontDb, TextSystem};

    const VIEWPORT: BSize = BSize { w: 640.0, h: 480.0 };

    // ---- pure helpers ----

    #[test]
    fn fuzzy_score_should_rank_subsequence() {
        // A contiguous / word-boundary match outscores a scattered one.
        let contig = fuzzy_score("Save", "sa").expect("subsequence");
        let scattered = fuzzy_score("Save", "se").expect("subsequence");
        assert!(
            contig.0 > scattered.0,
            "contiguous 'sa' ({}) outranks scattered 'se' ({})",
            contig.0,
            scattered.0
        );
        // A non-subsequence (missing char, or wrong order) does not match.
        assert!(fuzzy_score("Save", "xyz").is_none(), "missing chars → None");
        assert!(fuzzy_score("abc", "cb").is_none(), "out-of-order → None");
        // Empty query trivially matches.
        assert_eq!(fuzzy_score("Save", ""), Some((0, Vec::new())));
    }

    #[test]
    fn fuzzy_should_return_match_positions() {
        // "of" in "Open File" matches O@0 and F@5 (used to highlight those chars).
        let (_, positions) = fuzzy_score("Open File", "of").expect("subsequence");
        assert_eq!(positions, vec![0, 5]);
    }

    #[test]
    fn rank_should_match_via_keyword() {
        let cmds = vec![
            command("Save", || {}).keyword("write"),
            command("Quit", || {}),
        ];
        // A query matching only the keyword still includes the command (no highlight positions).
        let by_kw = rank(&cmds, "write");
        assert_eq!(by_kw.len(), 1, "'write' matches Save via its keyword");
        assert_eq!(by_kw[0].cmd, 0);
        assert!(
            by_kw[0].positions.is_empty(),
            "a keyword-only match carries no name-highlight positions"
        );
        // A name match highlights the name.
        let by_name = rank(&cmds, "sav");
        assert_eq!(by_name.len(), 1);
        assert_eq!(by_name[0].positions, vec![0, 1, 2]);
    }

    #[test]
    fn split_runs_should_split_matched_runs() {
        assert_eq!(
            split_runs("Save", &[0, 2]),
            vec![
                (true, "S".to_string()),
                (false, "a".to_string()),
                (true, "v".to_string()),
                (false, "e".to_string()),
            ]
        );
        // No positions → a single unmatched run of the whole name.
        assert_eq!(split_runs("Save", &[]), vec![(false, "Save".to_string())]);
    }

    // ---- headless widget harness (mirrors dropdown.rs) ----

    struct Harness {
        root: AnyElement,
        arena: Arena,
        layout: LayoutTree,
        text: TextSystem,
        hit: HitTest,
        reg: OverlayRegistry,
        focus: FocusRegistry,
        layers: OverlayLayers,
        damage: StdArc<DamageState>,
        theme: Theme,
        root_id: NodeId,
    }

    fn harness(build: impl FnOnce() -> AnyElement) -> Harness {
        let layers = OverlayLayers::new();
        provide_context(layers.clone());
        let focus = FocusRegistry::new();
        provide_context(focus.clone());
        let root = build();
        let mut h = Harness {
            root,
            arena: Arena::new(),
            layout: LayoutTree::new(),
            text: TextSystem::new(FontDb::new()),
            hit: HitTest::new(),
            reg: OverlayRegistry::new(),
            focus,
            layers,
            damage: StdArc::new(DamageState::default()),
            theme: Theme::light(),
            root_id: NodeId::from_raw(0),
        };
        h.root_id = frame(&mut h);
        h
    }

    /// Dispatches a left click (down+up) at window point `c`.
    fn click_at(h: &mut Harness, c: Point) {
        let mut dispatch = DispatchState::default();
        for kind in [
            MouseKind::Down(MouseButton::Left),
            MouseKind::Up(MouseButton::Left),
        ] {
            let ev = MouseEvent::new(kind, c, Modifiers::default());
            dispatch_mouse(&mut h.root, &h.arena, &h.hit, &ev, &mut dispatch);
        }
    }

    fn frame(h: &mut Harness) -> NodeId {
        let mut scene = Scene::new();
        render_tree(
            &mut h.root,
            &mut h.arena,
            &mut h.layout,
            &mut h.text,
            None,
            Some(&mut h.hit),
            Some(&mut h.reg),
            Some(&mut h.focus),
            None,
            None,
            &mut scene,
            VIEWPORT,
            &h.damage,
            &h.theme,
        )
        .unwrap()
    }

    /// Whether the palette panel is mounted (root=overlay → dyn_if has a child).
    fn panel_mounted(h: &Harness) -> bool {
        let dyn_if = h.arena.get(h.root_id).unwrap().children[0];
        !h.arena.get(dyn_if).unwrap().children.is_empty()
    }

    /// The result-row node ids: overlay → dyn_if → panel → child[1] dyn_list → scroll_view wheel-div → scroll →
    /// column → rows.
    fn result_rows(h: &Harness) -> Vec<NodeId> {
        let dyn_if = h.arena.get(h.root_id).unwrap().children[0];
        let panel = h.arena.get(dyn_if).unwrap().children[0];
        let list = h.arena.get(panel).unwrap().children[1];
        let wheel = h.arena.get(list).unwrap().children[0];
        let scroll = h.arena.get(wheel).unwrap().children[0];
        let col = h.arena.get(scroll).unwrap().children[0];
        h.arena.get(col).unwrap().children.clone()
    }

    fn press(h: &mut Harness, code: KeyCode) {
        let kev = KeyEvent::new(code, Modifiers::default(), true, false);
        dispatch_key(&mut h.root, &h.arena, h.focus.focused_node(), &kev);
    }

    /// Types one character into the auto-focused editor.
    fn type_char(h: &mut Harness, code: KeyCode, ch: &str) {
        let mut kev = KeyEvent::new(code, Modifiers::default(), true, false);
        kev.text = Some(SharedString::from(ch.to_string()));
        dispatch_key(&mut h.root, &h.arena, h.focus.focused_node(), &kev);
    }

    fn cmds() -> Vec<Command> {
        vec![
            command("Save File", || {}),
            command("Save As", || {}),
            command("Open File", || {}),
            command("Close", || {}),
        ]
    }

    #[test]
    fn command_palette_filter_should_rank() {
        let owner = Owner::new();
        owner.set();
        let open = RwSignal::new(false);
        let mut h = harness(|| command_palette(open, cmds()).into_element());

        open.set(true);
        frame(&mut h); // mount the panel (the field auto-focuses)
        assert_eq!(
            result_rows(&h).len(),
            4,
            "all commands show for the empty query"
        );

        // "save" is a subsequence of "Save File" + "Save As" only.
        type_char(&mut h, KeyCode::KeyS, "s");
        type_char(&mut h, KeyCode::KeyA, "a");
        type_char(&mut h, KeyCode::KeyV, "v");
        type_char(&mut h, KeyCode::KeyE, "e");
        frame(&mut h);
        assert_eq!(
            result_rows(&h).len(),
            2,
            "typing 'save' narrows to the two Save commands"
        );
        drop(owner);
    }

    #[test]
    fn command_palette_enter_should_run() {
        let owner = Owner::new();
        owner.set();
        let open = RwSignal::new(false);
        let ran = Rc::new(Cell::new(0u32));
        let r = ran.clone();
        let build = move || {
            command_palette(
                open,
                vec![
                    command("Open File", move || r.set(1)),
                    command("Close", || {}),
                ],
            )
            .into_element()
        };
        let mut h = harness(build);

        open.set(true);
        frame(&mut h);
        // Narrow to "Open File" only, then Enter runs it (Enter bubbles from the editor to the panel).
        for (c, s) in [
            (KeyCode::KeyO, "o"),
            (KeyCode::KeyP, "p"),
            (KeyCode::KeyE, "e"),
        ] {
            type_char(&mut h, c, s);
        }
        frame(&mut h);
        assert_eq!(result_rows(&h).len(), 1, "'ope' narrows to Open File");
        press(&mut h, KeyCode::Enter);
        assert_eq!(ran.get(), 1, "Enter runs the selected command's action");
        assert!(
            !open.get_untracked(),
            "running a command closes the palette"
        );
        drop(owner);
    }

    #[test]
    fn command_palette_esc_should_dismiss() {
        let owner = Owner::new();
        owner.set();
        let open = RwSignal::new(false);
        let mut h = harness(|| command_palette(open, cmds()).into_element());

        open.set(true);
        frame(&mut h);
        assert!(panel_mounted(&h), "the palette is open");
        press(&mut h, KeyCode::Escape);
        frame(&mut h); // the dyn_if unmounts on the next frame
        assert!(!open.get_untracked(), "Esc clears the open signal");
        assert!(
            !panel_mounted(&h),
            "Esc dismisses the palette (panel unmounts)"
        );
        drop(owner);
    }

    #[test]
    fn command_palette_should_highlight_matched_chars() {
        let owner = Owner::new();
        owner.set();
        let open = RwSignal::new(false);
        let mut h = harness(|| command_palette(open, cmds()).into_element());

        open.set(true);
        frame(&mut h);
        // "of" matches "Open File" at O@0 and F@5 → the label splits into 4 runs: [O][pen ][F][ile].
        type_char(&mut h, KeyCode::KeyO, "o");
        type_char(&mut h, KeyCode::KeyF, "f");
        frame(&mut h);
        let rows = result_rows(&h);
        assert_eq!(rows.len(), 1, "'of' narrows to Open File");
        let label = h.arena.get(rows[0]).unwrap().children[0];
        assert_eq!(
            h.arena.get(label).unwrap().children.len(),
            4,
            "the matched name splits into 4 highlight run-spans"
        );
        drop(owner);
    }

    #[test]
    fn command_palette_arrow_should_move_selection_and_run() {
        let owner = Owner::new();
        owner.set();
        let open = RwSignal::new(false);
        let ran = Rc::new(Cell::new(0u32));
        let r = ran.clone();
        let build = move || {
            command_palette(
                open,
                vec![
                    command("Save File", || {}),
                    command("Save As", move || r.set(2)),
                ],
            )
            .into_element()
        };
        let mut h = harness(build);

        open.set(true);
        frame(&mut h);
        assert_eq!(result_rows(&h).len(), 2, "both commands show");
        // ArrowDown moves the selection 0→1; a second ArrowDown clamps at the last (1). Enter runs it.
        press(&mut h, KeyCode::ArrowDown);
        press(&mut h, KeyCode::ArrowDown);
        press(&mut h, KeyCode::Enter);
        assert_eq!(
            ran.get(),
            2,
            "ArrowDown moves + clamps the selection, Enter runs the moved item"
        );
        drop(owner);
    }

    #[test]
    fn command_palette_arrow_up_should_clamp_at_top() {
        let owner = Owner::new();
        owner.set();
        let open = RwSignal::new(false);
        let ran = Rc::new(Cell::new(0u32));
        let r = ran.clone();
        let build = move || {
            command_palette(
                open,
                vec![command("First", move || r.set(1)), command("Second", || {})],
            )
            .into_element()
        };
        let mut h = harness(build);

        open.set(true);
        frame(&mut h);
        // ArrowUp at the top clamps at 0 (saturating_sub); Enter runs the first command.
        press(&mut h, KeyCode::ArrowUp);
        press(&mut h, KeyCode::Enter);
        assert_eq!(
            ran.get(),
            1,
            "ArrowUp clamps at the top; Enter runs the first"
        );
        drop(owner);
    }

    #[test]
    fn command_palette_outside_press_should_dismiss() {
        let owner = Owner::new();
        owner.set();
        let open = RwSignal::new(false);
        let mut h = harness(|| command_palette(open, cmds()).into_element());

        open.set(true);
        frame(&mut h);
        assert!(
            !h.layers.is_empty(),
            "an open palette registers a dismiss-on-outside layer (#219)"
        );
        assert!(
            h.layers.dismiss_topmost(),
            "the layer is dismissed by an outside press"
        );
        frame(&mut h);
        assert!(
            !panel_mounted(&h),
            "an outside press dismisses the palette (panel unmounts)"
        );
        drop(owner);
    }

    #[test]
    fn command_palette_click_should_run() {
        let owner = Owner::new();
        owner.set();
        let open = RwSignal::new(false);
        let ran = Rc::new(Cell::new(0u32));
        let r = ran.clone();
        let build = move || {
            command_palette(
                open,
                vec![
                    command("Open File", move || r.set(1)),
                    command("Close", || {}),
                ],
            )
            .into_element()
        };
        let mut h = harness(build);

        open.set(true);
        frame(&mut h);
        type_char(&mut h, KeyCode::KeyO, "o");
        type_char(&mut h, KeyCode::KeyP, "p");
        frame(&mut h);
        let rows = result_rows(&h);
        assert_eq!(rows.len(), 1, "'op' narrows to Open File");
        // The panel is a centered overlay: hit-test at the row's painted (centered) position.
        let dyn_if = h.arena.get(h.root_id).unwrap().children[0];
        let panel = h.arena.get(dyn_if).unwrap().children[0];
        click_centered_row(&mut h, panel, rows[0]);
        assert_eq!(ran.get(), 1, "clicking a result row runs its action");
        assert!(!open.get_untracked(), "and closes the palette");
        drop(owner);
    }

    #[test]
    fn command_palette_keyword_should_filter_and_run() {
        let owner = Owner::new();
        owner.set();
        let open = RwSignal::new(false);
        let ran = Rc::new(Cell::new(0u32));
        let r = ran.clone();
        let build = move || {
            command_palette(
                open,
                vec![
                    command("Save", move || r.set(1)).keyword("write"),
                    command("Quit", || {}),
                ],
            )
            .into_element()
        };
        let mut h = harness(build);

        open.set(true);
        frame(&mut h);
        // "write" matches "Save" only via its keyword → 1 result; Enter runs it end-to-end.
        for (c, s) in [
            (KeyCode::KeyW, "w"),
            (KeyCode::KeyR, "r"),
            (KeyCode::KeyI, "i"),
            (KeyCode::KeyT, "t"),
        ] {
            type_char(&mut h, c, s);
        }
        frame(&mut h);
        assert_eq!(
            result_rows(&h).len(),
            1,
            "'writ' matches Save via its keyword"
        );
        press(&mut h, KeyCode::Enter);
        assert_eq!(ran.get(), 1, "Enter runs the keyword-matched command");
        drop(owner);
    }

    /// Clicks a result row inside the centered overlay: the panel lays out at the origin but paints centered in
    /// the viewport, so the hit-test point is the row's panel-relative offset translated by the centered origin.
    fn click_centered_row(h: &mut Harness, panel: NodeId, row: NodeId) {
        let p = h.layout.absolute_layout(panel);
        let r = h.layout.absolute_layout(row);
        let cx = (VIEWPORT.w - p.size.w).max(0.0) / 2.0;
        let cy = (VIEWPORT.h - p.size.h).max(0.0) / 2.0;
        click_at(
            h,
            Point::new(
                cx + (r.origin.x - p.origin.x) + r.size.w / 2.0,
                cy + (r.origin.y - p.origin.y) + r.size.h / 2.0,
            ),
        );
    }
}

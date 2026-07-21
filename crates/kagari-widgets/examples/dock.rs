//! Dock example (#239) — a manual visual check of the [`dock`](kagari_widgets::dock) widget. Opens a real
//! window via the App shell, so it is a **manual/local** check (not a CI test).
//!
//! Run with: `cargo run -p kagari-widgets --example dock`
//!
//! A workspace of four panels in a nested split + a tab group. Try:
//! - **Resize**: drag a splitter divider (or focus it with Tab and arrow-key).
//! - **Tabs**: click the tabs in the tabbed region.
//! - **Drag-to-dock**: drag a panel's title bar over another region — drop in the CENTER to tabify, or near
//!   an EDGE to split. The dropped-on region tints while a compatible drag hovers.
//! - **Close**: the `x` header action removes the panel (the layout collapses/rebuilds).
//! - **Persistence**: the layout is saved to an in-memory [`PersistenceService`] on every change (the same
//!   `set`/`get` an app would point at a RON file, #68).

use kagari_base::{Axis, Px, Size};
use kagari_core::reactive::prelude::*;
use kagari_core::reactive::{RwSignal, create_effect};
use kagari_core::{
    App, DockChild, DockNode, DockTree, PanelId, PersistenceService, WindowOptions, div, text,
};
use kagari_style::{ColorRole, Styled};
use kagari_widgets::{button, dock, panel};

/// The default workspace: `Split[ Leaf(1) | Split-v[ Leaf(2), Tabs(3, 4) ] ]`.
fn default_layout() -> DockTree {
    DockTree::new(DockNode::Split {
        axis: Axis::Horizontal,
        children: vec![
            DockChild {
                node: DockNode::Leaf(PanelId::from_raw(1)),
                weight: 1.0,
            },
            DockChild {
                node: DockNode::Split {
                    axis: Axis::Vertical,
                    children: vec![
                        DockChild {
                            node: DockNode::Leaf(PanelId::from_raw(2)),
                            weight: 2.0,
                        },
                        DockChild {
                            node: DockNode::Tabs {
                                panels: vec![PanelId::from_raw(3), PanelId::from_raw(4)],
                                active: 0,
                            },
                            weight: 1.0,
                        },
                    ],
                },
                weight: 3.0,
            },
        ],
    })
}

fn main() -> Result<(), kagari_core::AppError> {
    let mut app = App::new()?;
    app.open_window(
        WindowOptions::default().title("kagari — dock (drag a title bar to re-dock)"),
        || {
            // App-owned persistence: restore the saved layout at startup (or the default), and save on
            // every change. An in-memory service here; an app points `PersistenceService::load()` at a file.
            let persistence = PersistenceService::in_memory();
            let restored = persistence
                .get::<DockTree>("dock_layout")
                .ok()
                .flatten()
                .filter(|t| !t.is_empty())
                .unwrap_or_else(default_layout);
            let tree = RwSignal::new(restored);
            {
                let persistence = persistence.clone();
                create_effect(move || {
                    let _ = persistence.set("dock_layout", &tree.get());
                });
            }

            // The dock fills the window (its outer container grows into this sized flex slot).
            div()
                .flex()
                .size(Size { w: 960.0, h: 620.0 })
                .bg(ColorRole::Bg)
                .child(dock(tree, move |id: PanelId| {
                    panel(format!("Panel {}", id.raw()), move || {
                        div()
                            .flex_col()
                            .gap_2()
                            .child(
                                text(format!("Content of panel {}", id.raw()))
                                    .color_role(ColorRole::Text)
                                    .size(Px(14.0)),
                            )
                            .child(
                                text("drag my title bar to re-dock")
                                    .color_role(ColorRole::TextMuted)
                                    .size(Px(12.0)),
                            )
                    })
                    .actions([button("x").ghost().on_click(move || {
                        tree.update(|t| t.remove(id));
                    })])
                }))
        },
    )?;
    app.run()
}

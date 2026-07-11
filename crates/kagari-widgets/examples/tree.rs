//! Tree example (#223) — a manual visual check of the [`tree`](kagari_widgets::tree) widget. Opens a real
//! window via the App shell, so it is a **manual/local** check (like the `table` / `dropdown` examples), not
//! a CI test.
//!
//! Run with: `cargo run -p kagari-widgets --example tree`
//!
//! A small scene-outliner-style tree: click a disclosure caret (▸/▾) to expand/collapse, click a row to
//! select it (the selected row tints Accent), and use the arrow keys to navigate (Down/Up move the
//! selection, Right/Left expand/collapse or descend/ascend). The body virtualizes.

use std::collections::HashSet;

use kagari_base::Px;
use kagari_core::reactive::RwSignal;
use kagari_core::{App, WindowOptions, div, text};
use kagari_style::{ColorRole, Styled};
use kagari_widgets::{TreeNode, TreeSelection, tree};

fn node(key: u32, name: &str) -> TreeNode<u32, String> {
    TreeNode::leaf(key, name.to_string())
}
fn group(key: u32, name: &str, children: Vec<TreeNode<u32, String>>) -> TreeNode<u32, String> {
    TreeNode::branch(key, name.to_string(), children)
}

fn scene() -> Vec<TreeNode<u32, String>> {
    vec![
        group(
            0,
            "Scene",
            vec![
                group(
                    1,
                    "Characters",
                    vec![node(2, "Player"), node(3, "Enemy"), node(4, "NPC")],
                ),
                group(5, "Environment", vec![node(6, "Terrain"), node(7, "Sky")]),
                node(8, "Camera"),
            ],
        ),
        group(9, "Lighting", vec![node(10, "Sun"), node(11, "Ambient")]),
    ]
}

fn main() -> Result<(), kagari_core::AppError> {
    let mut app = App::new()?;
    app.open_window(
        WindowOptions::default().title("kagari — tree (outliner)"),
        || {
            // Start with the top-level groups expanded.
            let expanded = RwSignal::new(HashSet::from([0u32, 9u32]));
            let selection = RwSignal::new(TreeSelection::none());
            div().p_4().bg(ColorRole::Surface).child(
                tree(scene())
                    .node(|name: &String| {
                        text(name.clone())
                            .color_role(ColorRole::Text)
                            .size(Px(15.0))
                    })
                    .expanded(expanded)
                    .selection(selection)
                    .height(360.0),
            )
        },
    )?;
    app.run()
}

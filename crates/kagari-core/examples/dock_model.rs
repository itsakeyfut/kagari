//! Dock substrate (#222) — a **headless** demonstration of the dock model (there is no window: the model
//! is pure data + tree ops; the visual dockable-panel UI is the Dock widget #239). Run with:
//!
//! `cargo run -p kagari-core --example dock_model`
//!
//! It builds a workspace by docking panels (centre = tabify, edge = split), prints the tree at each step,
//! shows drop-zone classification, and round-trips the layout through RON (the persistence format).

use kagari_base::{Point, Rect};
use kagari_core::{DockNode, DockTarget, DockTree, DropZone, PanelId, drop_zone};

/// A readable one-panel-per-line indented rendering of the tree.
fn print_tree(tree: &DockTree) {
    fn walk(node: &DockNode, depth: usize) {
        let pad = "  ".repeat(depth);
        match node {
            DockNode::Leaf(id) => println!("{pad}Leaf(panel {})", id.raw()),
            DockNode::Tabs { panels, active } => {
                let names: Vec<String> = panels.iter().map(|p| p.raw().to_string()).collect();
                println!("{pad}Tabs[{}] active={active}", names.join(", "));
            }
            DockNode::Split { axis, children } => {
                println!("{pad}Split({axis:?})");
                for c in children {
                    println!("{pad}  ├ weight {}", c.weight);
                    walk(&c.node, depth + 2);
                }
            }
            other => println!("{pad}{other:?}"),
        }
    }
    match tree.root() {
        Some(node) => walk(node, 0),
        None => println!("(empty workspace)"),
    }
    println!(
        "  panels present: {:?}",
        tree.panels().iter().map(|p| p.raw()).collect::<Vec<_>>()
    );
}

fn main() {
    let p = PanelId::from_raw;

    println!("=== 1. Seed an empty workspace, then build a layout ===\n");
    let mut tree = DockTree::empty();
    println!("start: empty? {}", tree.is_empty());

    // Seed the first panel (any drop on an empty workspace seeds it).
    tree.dock(DockTarget::Root, DropZone::Center, p(1));
    println!("\nafter docking panel 1 into the empty workspace:");
    print_tree(&tree);

    // Tabify panel 2 into panel 1's group (centre drop).
    tree.dock(DockTarget::Panel(p(1)), DropZone::Center, p(2));
    println!("\nafter docking panel 2 onto panel 1's CENTRE (tabify):");
    print_tree(&tree);

    // Split panel 3 to the left of the tab group (edge drop → splits the whole group).
    tree.dock(DockTarget::Panel(p(1)), DropZone::Left, p(3));
    println!("\nafter docking panel 3 on the LEFT edge (split the group):");
    print_tree(&tree);

    // Split panel 4 below panel 3.
    tree.dock(DockTarget::Panel(p(3)), DropZone::Bottom, p(4));
    println!("\nafter docking panel 4 BELOW panel 3 (split):");
    print_tree(&tree);

    println!("\n=== 2. Drop-zone classification (a 100x100 region) ===\n");
    let r = Rect::from_xywh(0.0, 0.0, 100.0, 100.0);
    for (x, y) in [
        (50.0, 50.0),
        (5.0, 50.0),
        (95.0, 50.0),
        (50.0, 5.0),
        (50.0, 95.0),
        (5.0, 5.0),
    ] {
        println!(
            "  point ({x:>4}, {y:>4}) -> {:?}",
            drop_zone(r, Point::new(x, y))
        );
    }

    println!("\n=== 3. Persistence round-trip (RON — the on-disk format) ===\n");
    let ron_text = ron::ser::to_string_pretty(&tree, ron::ser::PrettyConfig::default())
        .expect("serialize dock tree");
    println!("{ron_text}");
    let restored: DockTree = ron::from_str(&ron_text).expect("deserialize dock tree");
    println!("round-trips unchanged: {}", restored == tree);

    println!("\n=== 4. Remove panels — regions collapse ===\n");
    tree.remove(p(4));
    println!("after removing panel 4:");
    print_tree(&tree);
    tree.remove(p(3));
    println!("\nafter removing panel 3 (its split collapses):");
    print_tree(&tree);
    tree.remove(p(1));
    tree.remove(p(2));
    println!("\nafter removing panels 1 and 2 (workspace empties):");
    print_tree(&tree);

    println!("\n=== 5. Robustness — a malformed layout normalizes on load ===\n");
    let malformed = "Some(Split(axis: Horizontal, children: [\
        (node: Tabs(panels: [], active: 9), weight: 0.0), \
        (node: Tabs(panels: [1, 1], active: 5), weight: 1.0)]))";
    println!("raw (hand-edited) RON: {malformed}");
    let loaded: DockTree = ron::from_str(malformed).expect("deserialize");
    println!("normalized on load:");
    print_tree(&loaded);
}

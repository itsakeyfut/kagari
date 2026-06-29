//! Layout errors (#33).

use kagari_base::NodeId;

/// An error from a layout operation.
#[derive(Debug, thiserror::Error)]
pub enum LayoutError {
    /// The node has no entry in the layout tree (never inserted, or removed).
    #[error("layout node not found: {0:?}")]
    NodeNotFound(NodeId),
    /// taffy reported an error during layout (stringified — taffy's error type is not leaked).
    #[error("taffy layout error: {0}")]
    Taffy(String),
}

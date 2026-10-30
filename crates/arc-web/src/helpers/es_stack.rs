//! Compatibility facade for the Arc-owned headless stack.
pub use arc_stack::es_stack::{
    apply_snapshot_policy, build, build_stores, build_with_driver, EsStack, EsStores, SharedStack,
};

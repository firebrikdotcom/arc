//! Headless assembly of Arc event-sourced stacks.
use arc_core::projection::Projector;
pub mod config;
pub mod es_stack;
pub use es_stack::SharedStack;

/// A read-model projector plus the view (table) it maintains. Applications
/// register these against their aggregate so the framework can drive
/// synchronous, read-after-write projections in single-process mode.
pub struct ProjectorReg {
    pub projector: Box<dyn Projector>,
    pub view: String,
}

impl ProjectorReg {
    pub fn new(projector: impl Projector + 'static, view: impl Into<String>) -> Self {
        Self {
            projector: Box::new(projector),
            view: view.into(),
        }
    }
}

use std::sync::Arc;

use super::*;
use crate::builtin::{glob::GlobTool, report_status::ReportStatusTool};

#[test]
fn definitions_are_cached_and_invalidated_by_register() {
    let mut reg = ToolRegistry::new().with(Arc::new(GlobTool));
    let a = reg.definitions_arc();
    let b = reg.definitions_arc();
    assert!(Arc::ptr_eq(&a, &b));
    assert_eq!(a.len(), 1);

    reg.register(Arc::new(ReportStatusTool));
    let c = reg.definitions_arc();
    assert!(!Arc::ptr_eq(&a, &c));
    assert_eq!(c.len(), 2);
    assert_eq!(reg.definitions().len(), 2);
}

#[test]
fn without_does_not_reuse_stale_definitions() {
    let reg = ToolRegistry::new()
        .with(Arc::new(GlobTool))
        .with(Arc::new(ReportStatusTool));
    let _ = reg.definitions_arc();
    assert_eq!(reg.without("glob").definitions_arc().len(), 1);
}

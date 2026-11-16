//! Every ecosystem purge supports. Adding one is a new crate under
//! `ecosystems/` plus one line here.

use purge_ecosystem::Ecosystem;

/// Instances of all supported ecosystems, in detection order.
pub fn all() -> Vec<Box<dyn Ecosystem>> {
    vec![
        Box::new(purge_rust::Rust::new()),
        Box::new(purge_js::JavaScript::new()),
    ]
}

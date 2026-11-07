//! Integration tests for the `snapsrc` element.

use gst::prelude::*;
use gst_base::prelude::*;

fn init() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        gst::init().unwrap();
        gstsnapsrc::plugin_register_static().expect("register snapsrc plugin");
    });
}

#[test]
fn element_has_default_properties() {
    init();
    let src = gst::ElementFactory::make("snapsrc").build().unwrap();
    assert_eq!(src.property::<String>("host"), "127.0.0.1");
    assert_eq!(src.property::<u32>("port"), 1704);
    assert!(src.downcast_ref::<gst_base::BaseSrc>().unwrap().is_live());
}

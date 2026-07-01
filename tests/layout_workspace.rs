//! Verifies the workspace exposes the layout sub-crate API expected by docparse.

use docparse_layout::{LayoutModel, LayoutOptions};

/// Ensures the default layout options select PP-DocLayoutV3.
#[test]
fn default_layout_options_select_pp_doclayout_v3() {
    assert_eq!(LayoutOptions::default().model, LayoutModel::PpDocLayoutV3);
}

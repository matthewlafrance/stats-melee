//! One module per screen in the main panel.
//!
//! Each declares its own `impl StatsMeleeApp` block, so a page's methods
//! sit together in one file while the app state they read stays private to
//! `app`.

pub(super) mod library;
pub(super) mod settings;
pub(super) mod summary;

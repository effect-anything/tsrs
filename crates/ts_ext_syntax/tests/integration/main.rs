mod cli_snapshots;
mod lsp_expansion;
mod lsp_macro_module;
mod lsp_trust;
mod options_parity;
mod sweetener_map;
mod typegpu;
mod unit_tests;
mod watch_macro_module;

use tsrs_lsp as _;

pub(crate) static E2E_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

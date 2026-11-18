//! Extension hooks for `tsrs_lsp` (`--runExternalCode` default and `custom/textDocument/showExpansion`).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tsrs_core::context::Context;
use tsrs_ls::LanguageService;
use tsrs_lsproto::{self as lsproto, Error};

use crate::server::Server;

static DEFAULT_RUN_EXTERNAL_CODE: AtomicBool = AtomicBool::new(false);

/// Sets the process-wide default for `runExternalCode` when a client sends no
/// `initializationOptions.runExternalCode` (an explicit client option still wins).
pub fn set_default_run_external_code(enabled: bool) {
    // Relaxed: independent process-wide default flag with no dependent state.
    DEFAULT_RUN_EXTERNAL_CODE.store(enabled, Ordering::Relaxed);
}

pub(crate) fn default_run_external_code() -> bool {
    // Relaxed: reads the independent process-wide default flag.
    DEFAULT_RUN_EXTERNAL_CODE.load(Ordering::Relaxed)
}

impl Server {
    #[expect(
        clippy::unused_self,
        reason = "Registered LSP handlers share a Server receiver so the dispatcher can call them uniformly."
    )]
    pub(crate) fn handle_show_expansion(
        self: &Arc<Self>,
        _ctx: &Context,
        ls: &Arc<LanguageService>,
        params: &lsproto::TextDocumentPositionParams,
    ) -> Result<lsproto::CustomTextDocumentShowExpansionResponse, Error> {
        tsrs_ls::ext_ls::show_expansion(ls, params)
    }
}

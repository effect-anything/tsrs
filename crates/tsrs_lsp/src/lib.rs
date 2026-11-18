// Go internal/lsp: the language server (docs/LSP.md).

pub mod dynamic_queue;
pub mod ext_lsp;
mod logger;
mod lsconsts;
pub mod lspwatcher;
pub mod progress;
pub mod server;
pub mod stack_sanitizer;
pub mod workerpool;

pub use ext_lsp::set_default_run_external_code;
pub use server::{
    NpmInstallFunc, Reader, Server, ServerOptions, SpawnFunc, Writer, new_server, to_reader,
    to_writer,
};

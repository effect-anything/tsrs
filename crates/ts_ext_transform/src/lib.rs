//! Source-to-source transforms and precheck/post-emit rewrites for `tsrs`.

pub mod align;
pub mod precheck;
pub mod tsover;
pub mod typegpu;

pub use precheck::MAPPER_NAME;

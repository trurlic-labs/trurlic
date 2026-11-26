//! The `apply` tool: its arguments declared once for the schema and the
//! parse, and a handler that hands the batch to the store's one write
//! ([`tool`]), each op's fields declared in [`ops`]. Every rule a batch is
//! held to is the store's; every repair the tree shows, which the store
//! cannot read, is read before the write lock is taken and handed to the
//! store as a map ([`repairs`]); the likely duplicates of what it wrote are
//! looked for in the graph it left ([`duplicates`]). Its result is
//! declared in [`result`].

mod duplicates;
mod ops;
mod repairs;
mod result;
mod tool;

pub(crate) use duplicates::Duplicates;
pub(crate) use result::Answer;
pub(crate) use tool::APPLY;

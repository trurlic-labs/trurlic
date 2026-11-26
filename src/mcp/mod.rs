//! The MCP transport: one session over stdio, each request answered in
//! turn, above the tool layer it calls.
//!
//! The server holds the directory it was started in and no graph. Every
//! tool call hands that directory to the tool, which finds the store from
//! there and loads it, so a call answers from the files on disk, and a
//! store created or migrated while the server runs is served from the next
//! call. Without one, `initialize` and `tools/list` still answer, and
//! every tool call is refused naming the command that fixes it.
//!
//! The session is [`server`], a `tools/call` request [`call`], what a
//! request is answered with [`reply`]; the wire is [`protocol`], the
//! client on its other end [`peer`], a tool's listing and result envelope
//! [`listing`] and [`envelope`].

mod call;
mod envelope;
mod listing;
mod peer;
mod protocol;
mod reply;
mod server;

#[cfg(feature = "bench")]
pub use server::Server;
pub(crate) use server::run_server;

#[cfg(test)]
mod tests {
    use std::path::Path;

    /// The transport sits below the commands that serve it: its non-test
    /// code names in the crate only itself, the tool layer, the budget,
    /// the error, the console and the failpoints, however a path is
    /// spelled (`crate::purity` states what the check cannot see).
    #[test]
    fn the_transport_names_no_command() {
        let named = ["mcp", "tools", "budget", "Result", "console", "failpoint"];
        let source = crate::purity::Crate::read(Path::new(env!("CARGO_MANIFEST_DIR")));
        assert_eq!(source.places_beyond("mcp", &named), Vec::<String>::new());
    }
}

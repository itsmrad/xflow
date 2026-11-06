#[path = "../cli/mod.rs"]
mod cli;

fn main() -> std::process::ExitCode {
    // SAFETY: first statement of the process; no thread has used SQLite yet.
    // Meeting reads open SQLite in this process. The opener must not do this.
    unsafe { xflow_app::store::skip_bulk_page_cache() };
    cli::main()
}

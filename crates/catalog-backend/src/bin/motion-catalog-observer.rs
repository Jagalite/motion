//! Standalone packaging/test entrypoint; the shipped server/operator embed the
//! same audited closed dispatcher, rather than trusting a sibling executable.
fn main() {
    match motion_catalog_backend::observer::run_closed(
        &std::env::args().skip(1).collect::<Vec<_>>(),
    ) {
        Ok(value) => println!("{value}"),
        Err(error) => {
            eprintln!("motion-catalog-observer: {error}");
            std::process::exit(2);
        }
    }
}

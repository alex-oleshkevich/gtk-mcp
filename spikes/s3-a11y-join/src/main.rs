//! Spike S3 (plan 3.2 / 10): can the in-process GTK accessible tree (public
//! getters only) be joined to the AT-SPI export seen from outside?
//!
//! `s3 app <gtk.json> <stop-file>`   fixture-like GTK app; dumps its accessible tree, then waits for <stop-file>
//! `s3 client <pid> <gtk.json> <out.json>`  walks the app's AT-SPI tree (zbus, a11y bus) and measures the join
//!
//! Driven by `run.sh` (private session + a11y bus, nested niri off the working output).

mod app;
mod client;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("app") => app::run(&args[2], &args[3]),
        Some("client") => client::run(args[2].parse().unwrap(), &args[3], &args[4]),
        _ => eprintln!("usage: s3 app <gtk.json> <stop> | s3 client <pid> <gtk.json> <out.json>"),
    }
}

//! Prints the layout this version writes for a settings file, every key at its
//! default: `cargo run -q -p tangul-data --example layout -- settings|database`.
//! `make release` keeps it as the release's fixture (crates/data/migrations).

fn main() {
    let layout = match std::env::args().nth(1).as_deref() {
        Some("settings") => tangul_data::settings::defaults(),
        Some("database") => tangul_data::database_settings::defaults(),
        _ => {
            eprintln!("usage: layout settings|database");
            std::process::exit(2);
        }
    };
    // Its output is the file: stdout on purpose.
    let text = tangul_data::upgrade::pretty(&layout);
    std::io::Write::write_all(&mut std::io::stdout(), text.as_bytes()).expect("stdout");
}

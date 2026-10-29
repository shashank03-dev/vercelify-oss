//! Print the ffmpeg arguments the cockpit uses for a `SenderConfig` (JSON in
//! the first argument) as a JSON array, so the display contract test runs the
//! real sender command line rather than a copy of it.
//!
//!   cargo run -q -p vercelify-core --example sender_args -- '{"url":"udp://…",…}'

fn main() {
    let raw = std::env::args().nth(1).unwrap_or_default();
    let config: vercelify_core::stream::SenderConfig = match serde_json::from_str(&raw) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("bad SenderConfig JSON: {e}");
            std::process::exit(2);
        }
    };
    match vercelify_core::stream::sender_args(&config) {
        Ok(args) => println!("{}", serde_json::to_string(&args).expect("strings serialize")),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

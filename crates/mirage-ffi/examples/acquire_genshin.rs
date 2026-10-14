//! Read-only publisher metadata probe; writes only the explicitly chosen new
//! output directory. Does not mount, launch, install or change game content.
fn main() {
    let mut args = std::env::args().skip(1);
    let destination = args.next().expect("new output directory");
    let language = args.next().unwrap_or_else(|| "en-us".into());
    match mirage_ffi::sophon::publisher::acquire(std::path::Path::new(&destination), &language) {
        Ok(generation) => println!("{}", serde_json::to_string_pretty(&generation).unwrap()),
        Err(error) => {
            eprintln!("publisher acquisition failed: {error:?}");
            std::process::exit(1);
        }
    }
}

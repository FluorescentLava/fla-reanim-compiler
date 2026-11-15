fn main() {
    if let Err(error) = fla_reanim::workflow::cli(std::env::args().skip(1)) {
        eprintln!(
            "{}",
            fla_reanim::workflow::display_message(&format!("{error:#}"))
        );
        std::process::exit(1);
    }
}

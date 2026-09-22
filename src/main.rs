use std::process::ExitCode;

use tunnel_deck::error::ExitStatus;

fn main() -> ExitCode {
    match tunnel_deck::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("tdeck: {error}");
            ExitCode::from(ExitStatus::from(&error) as u8)
        }
    }
}

mod args;
mod convert;
mod progress;
mod run;
mod signal;

use clap::Parser;

pub fn main() -> i32 {
    let cli = match args::Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            let _ = err.print();
            return if err.use_stderr() { 1 } else { 0 };
        }
    };
    match run::run(cli) {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("error: {err}");
            for hint in run::hints(&err) {
                eprintln!("  {hint}");
            }
            err.exit_code()
        }
    }
}

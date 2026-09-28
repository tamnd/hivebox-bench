//! The `hivebox-bench` command.

#![forbid(unsafe_code)]

use std::process::ExitCode;

use hivebox_bench::suite::{self, SUITES};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("list") => {
            println!("{:<20} {:<8} {:<4} target", "suite", "scope", "from");
            for s in SUITES {
                let scope = format!("{:?}", s.scope).to_lowercase();
                println!("{:<20} {:<8} {:<4} {}", s.name, scope, s.milestone, s.target);
            }
            ExitCode::SUCCESS
        }
        Some("run") => match args.get(1).and_then(|n| suite::find(n)) {
            Some(s) => {
                eprintln!(
                    "{} measures {}, and it needs a hivebox cluster at {} or later, which does not exist yet",
                    s.name, s.measures, s.milestone
                );
                ExitCode::FAILURE
            }
            None => {
                eprintln!("no such suite, try `hivebox-bench list`");
                ExitCode::FAILURE
            }
        },
        Some("--version" | "-V") => {
            println!("hivebox-bench {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        _ => {
            println!("hivebox-bench <command>");
            println!();
            println!(
                "  list         every suite, where it runs, and the target it is judged against"
            );
            println!("  run <suite>  run one suite against the cluster in HIVEBOX_ENDPOINT");
            ExitCode::SUCCESS
        }
    }
}

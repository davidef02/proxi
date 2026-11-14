use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "proxi",
    version,
    about = "Proxi",
    disable_help_subcommand = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Data folder: where, help
    Data {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Lessons: init, append, list
    Lesson {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Run the local server
    Serve(proxi::server::ServeArgs),
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args_os()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();

    let code = match args.get(1).map(String::as_str) {
        Some("data") => proxi::data_root::run_cli(&args[2..]),
        Some("lesson") => proxi::lesson::cli::run_cli(&args[2..]),
        _ => {
            let cli = Cli::parse_from(&args);
            match cli.command {
                Command::Data { args } => proxi::data_root::run_cli(&args),
                Command::Lesson { args } => proxi::lesson::cli::run_cli(&args),
                Command::Serve(args) => proxi::server::serve_cli(args),
            }
        }
    };

    std::process::ExitCode::from(code as u8)
}

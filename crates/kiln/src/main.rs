mod cli;
mod helper;
mod server;
mod setup;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use kiln_core::config::FILE_NAME;

#[derive(Parser)]
#[command(name = "kiln", version, about = "Self-hosted CI for private GitHub repositories")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    #[command(about = "Check a .kiln.toml for errors")]
    Validate {
        #[arg(default_value = FILE_NAME)]
        file: PathBuf,
        #[arg(long, help = "Repository name used for the default image name")]
        repo: Option<String>,
        #[arg(long, help = "Print diagnostics as JSON")]
        json: bool,
    },
    #[command(about = "Show what Kiln would run for an event, and the version it would release")]
    Plan {
        #[arg(long, default_value = "push", value_parser = cli::parse_event, help = "push, pr or tag:<name>")]
        event: cli::Event,
        #[arg(long = "published", help = "A tag already in the registry; repeat for each tag")]
        published: Vec<String>,
        #[arg(long, default_value = FILE_NAME)]
        file: PathBuf,
        #[arg(long)]
        repo: Option<String>,
    },
    #[command(about = "Print the JSON Schema for .kiln.toml")]
    Schema,
    #[command(about = "Create the Kiln GitHub App and write its credentials as a Kubernetes Secret")]
    Setup {
        #[arg(long, help = "The https address Kiln will be reachable at, like https://kiln.example.com")]
        url: String,
        #[arg(long, help = "Create the app in this organization instead of your personal account")]
        org: Option<String>,
        #[arg(long, help = "App name shown on GitHub; must be unique on GitHub")]
        name: Option<String>,
        #[arg(long, default_value = "kiln-secret.yaml", help = "Where to write the Secret with the app's credentials")]
        out: PathBuf,
        #[arg(long, default_value_t = 0, help = "Local port for the setup page; 0 picks a free one")]
        port: u16,
        #[arg(long, help = "Print the setup address instead of opening a browser")]
        no_browser: bool,
        #[arg(long, default_value = "https://github.com", hide = true)]
        github: String,
        #[arg(long, default_value = "https://api.github.com", hide = true)]
        api: String,
    },
    #[command(about = "Run the Kiln server")]
    Serve,
    #[command(about = "Try Kiln locally with simulated repositories, builds and logs")]
    Demo {
        #[arg(long, default_value_t = 8080)]
        port: u16,
    },
    #[command(hide = true, subcommand)]
    Helper(helper::Helper),
}

#[tokio::main]
async fn main() -> ExitCode {
    let result = match Cli::parse().command {
        Commands::Validate { file, repo, json } => cli::validate(&file, repo, json),
        Commands::Plan { event, published, file, repo } => cli::plan(&file, repo, &event, &published),
        Commands::Schema => cli::schema(),
        Commands::Setup { url, org, name, out, port, no_browser, github, api } => {
            setup::run(setup::Options { url, org, name, out, port, browser: !no_browser, github, api }).await
        }
        Commands::Serve => server::serve().await.map(|()| ExitCode::SUCCESS),
        Commands::Demo { port } => server::demo(port).await.map(|()| ExitCode::SUCCESS),
        Commands::Helper(command) => helper::run(command).await,
    };
    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::from(2)
        }
    }
}

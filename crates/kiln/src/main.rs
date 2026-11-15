mod cli;
mod helper;
mod server;

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
    #[command(about = "Run the Kiln server")]
    Serve,
    #[command(about = "List recent builds; run inside the Kiln server pod")]
    Builds {
        #[arg(long, help = "owner/name")]
        repo: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: i64,
    },
    #[command(about = "Print a build's logs; run inside the Kiln server pod")]
    Logs {
        #[arg(help = "owner/name")]
        repo: String,
        #[arg(help = "Build number")]
        number: i64,
        #[arg(long, help = "Only this task, by name or container")]
        task: Option<String>,
        #[arg(long, short, help = "Keep printing until the build finishes")]
        follow: bool,
    },
    #[command(about = "Try Kiln locally with simulated repositories, builds and logs")]
    Demo {
        #[arg(long, default_value_t = 8080)]
        port: u16,
        #[arg(long, default_value = "127.0.0.1", help = "Address to listen on; use 0.0.0.0 inside a container")]
        bind: std::net::IpAddr,
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
        Commands::Serve => server::serve().await.map(|()| ExitCode::SUCCESS),
        Commands::Builds { repo, limit } => server::inspect::builds(repo, limit),
        Commands::Logs { repo, number, task, follow } => server::inspect::logs(repo, number, task, follow).await,
        Commands::Demo { port, bind } => server::demo(bind, port).await.map(|()| ExitCode::SUCCESS),
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

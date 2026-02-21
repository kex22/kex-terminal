use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "kex", about = "A modern terminal multiplexer")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Manage the kex server
    Server {
        #[command(subcommand)]
        action: ServerAction,
    },
    /// Manage terminal instances
    Terminal {
        #[command(subcommand)]
        action: TerminalAction,
    },
}

#[derive(Subcommand)]
enum ServerAction {
    /// Start the server daemon
    Start,
    /// Stop the server daemon
    Stop,
}

#[derive(Subcommand)]
enum TerminalAction {
    /// Create a new terminal
    Create {
        /// Optional name for the terminal
        #[arg(long)]
        name: Option<String>,
        /// Don't attach after creating
        #[arg(long)]
        detach: bool,
    },
    /// List all terminals
    Ls,
    /// Kill a terminal
    Kill {
        /// Terminal ID or name
        id: String,
    },
    /// Attach to a terminal
    Attach {
        /// Terminal ID or name
        id: String,
    },
}

fn main() {
    let cli = Cli::parse();

    // Daemonize before tokio runtime is created
    if matches!(
        cli.command,
        Command::Server {
            action: ServerAction::Start
        }
    ) && let Err(e) = kex::server::daemon::daemonize()
    {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }

    let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
    if let Err(e) = rt.block_on(run(cli)) {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> kex::error::Result<()> {
    use kex::error::KexError;
    use kex::ipc::client::IpcClient;
    use kex::ipc::message::{Request, Response};

    match cli.command {
        Command::Server { action } => match action {
            ServerAction::Start => kex::server::Server::start().await,
            ServerAction::Stop => {
                let mut client = IpcClient::connect().await?;
                match client.send(Request::ServerStop).await? {
                    Response::Ok => {
                        println!("server stopped");
                        Ok(())
                    }
                    Response::Error { message } => Err(KexError::Server(message)),
                    _ => Ok(()),
                }
            }
        },
        Command::Terminal { action } => match action {
            TerminalAction::Create { name, detach } => {
                let label = name.clone();
                let mut client = IpcClient::connect().await?;
                match client.send(Request::TerminalCreate { name }).await? {
                    Response::TerminalCreated { id } => {
                        if detach {
                            println!("{id}");
                            Ok(())
                        } else {
                            let label = label.unwrap_or_else(|| id.clone());
                            let mut client = IpcClient::connect().await?;
                            match client.send(Request::TerminalAttach { id }).await? {
                                Response::Ok => {
                                    kex::terminal::attach::attach(client.into_stream(), &label).await
                                }
                                Response::Error { message } => Err(KexError::Server(message)),
                                _ => Ok(()),
                            }
                        }
                    }
                    Response::Error { message } => Err(KexError::Server(message)),
                    _ => Ok(()),
                }
            }
            TerminalAction::Ls => {
                let mut client = IpcClient::connect().await?;
                if let Response::TerminalList { terminals } =
                    client.send(Request::TerminalList).await?
                {
                    if terminals.is_empty() {
                        println!("no terminals");
                    } else {
                        println!("{:<10} {:<15} CREATED", "ID", "NAME");
                        for t in terminals {
                            println!(
                                "{:<10} {:<15} {}",
                                t.id,
                                t.name.as_deref().unwrap_or("-"),
                                t.created_at
                            );
                        }
                    }
                }
                Ok(())
            }
            TerminalAction::Kill { id } => {
                let mut client = IpcClient::connect().await?;
                match client.send(Request::TerminalKill { id }).await? {
                    Response::Ok => Ok(()),
                    Response::Error { message } => Err(KexError::Server(message)),
                    _ => Ok(()),
                }
            }
            TerminalAction::Attach { id } => {
                let mut client = IpcClient::connect().await?;
                match client.send(Request::TerminalAttach { id: id.clone() }).await? {
                    Response::Ok => {
                        kex::terminal::attach::attach(client.into_stream(), &id).await
                    }
                    Response::Error { message } => Err(KexError::Server(message)),
                    _ => Ok(()),
                }
            }
        },
    }
}

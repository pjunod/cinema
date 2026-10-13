//! Independent support hosting. This process has no Cinema LAN/control authority.
mod handoff;
mod limits;
mod operator;
mod store;
#[cfg(test)]
mod tests;
mod transport;
use clap::{Parser, Subcommand};
use std::{net::SocketAddr, path::PathBuf};
use store::{Database, Fault};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    state_dir: PathBuf,
    #[arg(long)]
    key_file: PathBuf,
    #[arg(long)]
    limits_file: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    InitKey,
    RevokeOperator {
        #[arg(long)]
        name: String,
    },
    Serve {
        #[arg(long)]
        support_origin: String,
        #[arg(long)]
        operator_origin: String,
        #[arg(long, default_value = "127.0.0.1:8087")]
        ingress: SocketAddr,
        #[arg(long, default_value = "127.0.0.1:8088")]
        operator: SocketAddr,
        #[arg(long)]
        development_local: bool,
        #[arg(long)]
        trust_loopback_ingress: bool,
    },
    Publisher {
        #[arg(long)]
        id: String,
        #[arg(long)]
        credential_out: PathBuf,
        #[arg(long)]
        origin: Vec<String>,
    },
    RevokePublisher {
        #[arg(long)]
        id: String,
    },
    Operator {
        #[arg(long)]
        name: String,
        #[arg(long)]
        token_out: PathBuf,
    },
    Reconcile,
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    if matches!(args.command, Command::InitKey) {
        let mut key = [0u8; 32];
        getrandom::getrandom(&mut key).map_err(|_| error(Fault::Unavailable))?;
        store::atomic_secret(&args.key_file, &key).map_err(error)?;
        println!("Storage key written to the selected protected file.");
        return Ok(());
    }
    let mut db = Database::open(&args.state_dir, &args.key_file)
        .map_err(|e| anyhow::anyhow!("support storage unavailable: {e:?}"))?;
    db.set_limits(limits::Limits::load(args.limits_file.as_deref()).map_err(error)?);
    match args.command {
        Command::InitKey => unreachable!("handled before database open"),
        Command::RevokeOperator { name } => {
            db.fence("operator", &name).map_err(error)?;
            db.with(|c| {
                c.execute(
                    "UPDATE operators SET active=0,key=NULL WHERE name=?1",
                    [name],
                )?;
                Ok(())
            })
            .map_err(error)?;
            println!("Operator and its sessions revoked.");
        }
        Command::Serve {
            support_origin,
            operator_origin,
            ingress,
            operator,
            development_local,
            trust_loopback_ingress,
        } => {
            if !ingress.ip().is_loopback() || !operator.ip().is_loopback() || ingress == operator {
                anyhow::bail!("listeners must be distinct loopback endpoints behind their separate TLS ingress");
            }
            transport::serve(
                db,
                support_origin,
                operator_origin,
                ingress,
                operator,
                development_local,
                trust_loopback_ingress,
            )
            .await?;
        }
        Command::Publisher {
            id,
            credential_out,
            origin,
        } => {
            db.enroll_publisher(&id, &origin, &credential_out)
                .map_err(error)?;
            println!("Publisher enrolled; credential written to the selected operator file.");
        }
        Command::RevokePublisher { id } => {
            db.fence("publisher", &id).map_err(error)?;
            db.with(|c| {
                c.execute("UPDATE publishers SET active=0 WHERE id=?1", [id])?;
                Ok(())
            })
            .map_err(error)?;
            println!("Publisher revoked, including all child capabilities.");
        }
        Command::Operator { name, token_out } => {
            operator::bootstrap(&db, &name, &token_out).map_err(error)?;
            println!("One-use operator enrollment token written to the selected file.");
        }
        Command::Reconcile => {
            db.reconcile().map_err(error)?;
            println!("Deletion journal, expiry and storage reclamation reconciled.");
        }
    }
    Ok(())
}
fn error(e: Fault) -> anyhow::Error {
    anyhow::anyhow!("support operation refused: {e:?}")
}

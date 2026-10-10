#[allow(dead_code)]
mod process;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let wall_ms: u64 = args.next().expect("wall-ms").parse().expect("wall integer");
    let cancel_ms: u64 = args.next().expect("cancel-ms; zero disables").parse().expect("cancel integer");
    let program = args.next().expect("program");
    let arguments: Vec<String> = args.collect();
    let token = CancellationToken::new();
    if cancel_ms != 0 {
        let t = token.clone();
        tokio::spawn(async move {tokio::time::sleep(Duration::from_millis(cancel_ms)).await;t.cancel();});
    }
    let result = process::bounded::cancellable(token, process::bounded::output(
        program, &arguments, Duration::from_millis(wall_ms), 65537,
        process::ChildWork::background("non-serving DV M2 synthetic helper"),
    )).await;
    match result {
        Ok(output) => {
            if output.stdout.len()>65536 || output.stderr.len()>65536 {eprintln!("owned-runner: observed capture cap");std::process::exit(65);}
            use std::io::Write;
            std::io::stdout().write_all(&output.stdout).expect("stdout");
            std::io::stderr().write_all(&output.stderr).expect("stderr");
            std::process::exit(output.status.code().unwrap_or(66));
        }
        Err(error) => {eprintln!("owned-runner: {:?}: {}",error.kind(),error);std::process::exit(70);}
    }
}

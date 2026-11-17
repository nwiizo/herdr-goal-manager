//! herdr-goal-manager CLI entrypoint.
mod accounting;
mod admission;
mod binding;
mod checkout;
mod cli;
mod config;
mod contract;
mod cross_check;
mod events;
mod factory;
mod fingerprint;
mod handoff;
mod herdr;
mod herdr_cli;
mod herdr_events;
mod manager;
mod manager_goal;
mod model;
mod monitor;
mod native_goal;
mod packet;
mod prepared;
mod process;
mod queue;
mod roles;
mod runner;
mod scope;
mod signals;
mod store;
mod supervision;
mod view;
mod wake_await;
mod watch;
mod watch_view;

use clap::Parser;

fn main() -> std::process::ExitCode {
    let cli = cli::Cli::parse();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("Cannot start runtime: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let result = runtime.block_on(async {
        tokio::select! {
            result = cli.execute() => match result {
                Ok(()) => std::process::ExitCode::SUCCESS,
                Err(error) => { eprintln!("Error: {error:#}"); std::process::ExitCode::FAILURE }
            },
            result = tokio::signal::ctrl_c() => {
                if let Err(error) = result { eprintln!("Cannot listen for interruption: {error}"); }
                eprintln!("Interrupted. Partial work is preserved; inspect the goal before resume.");
                std::process::ExitCode::from(130)
            }
        }
    });
    // A panel may be blocked in stdin. Bound shutdown instead of waiting forever
    // for that blocking read, while allowing subprocess drop/reaping to complete.
    runtime.shutdown_timeout(std::time::Duration::from_secs(2));
    result
}

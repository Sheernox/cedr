//! CLI output for the shared Route starter generator.
use std::path::Path;
use std::process::ExitCode;

use cedr_route::{Result, config, starter};
use clap::Args as ClapArgs;
use serde_json::json;

#[derive(ClapArgs)]
pub struct Args {
    /// Use these harnesses instead of detecting installed agents (repeatable)
    #[arg(long, value_name = "H")]
    harness: Vec<String>,
    /// Agent and model that keep work when no candidate is eligible
    #[arg(long, value_name = "H/MODEL")]
    orchestrator: Option<String>,
    /// Print the generated routes without writing
    #[arg(long)]
    dry_run: bool,
    #[arg(long, value_enum, default_value_t = crate::Format::Text)]
    format: crate::Format,
}

pub fn run(args: Args, path: Option<&Path>) -> Result<ExitCode> {
    let dest = path.map_or_else(config::user_path, Path::to_path_buf);
    if !args.dry_run {
        starter::ensure_new(&dest)?;
    }
    let harnesses = if args.harness.is_empty() {
        starter::detect()?
    } else {
        args.harness
    };
    let starter = starter::prepare(dest, &harnesses, args.orchestrator.as_deref())?;
    if !args.dry_run {
        starter.write()?;
    }
    match args.format {
        crate::Format::Json => {
            if args.dry_run {
                eprint!("{}", starter.text);
            }
            println!(
                "{}",
                json!({
                    "path": starter.path.to_string_lossy(),
                    "written": !args.dry_run,
                    "harnesses": starter.harnesses,
                    "orchestrator": starter.orchestrator,
                    "routes": starter.routes,
                })
            );
        }
        crate::Format::Text if args.dry_run => print!("{}", starter.text),
        crate::Format::Text => {
            println!(
                "Wrote {} for {}.",
                starter.path.display(),
                starter.harnesses.join(", ")
            );
            if path.is_some() {
                println!("Next: cedr route show review.code --config PATH (use the file above)");
            } else {
                println!("Next: cedr route show review.code");
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

//! Plan local setup, then apply only the steps the user confirms.
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use cedr_route::{config, starter};
use serde::Serialize;

use crate::doctor::{self, McpRegistration};
use crate::{Format, monitor};

#[derive(clap::Args)]
pub struct Args {
    /// Confirm every proposed change
    #[arg(long)]
    yes: bool,
    /// Show the plan without changing anything
    #[arg(long)]
    dry_run: bool,
    #[arg(long, value_enum, default_value_t = Format::Text)]
    format: Format,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Status {
    Planned,
    Applied,
    Skipped,
    Failed,
    Already,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Applied => "applied",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
            Self::Already => "already",
        }
    }
}

#[derive(Serialize)]
struct Step {
    id: String,
    description: String,
    command: Option<Vec<&'static str>>,
    status: Status,
    error: Option<String>,
    #[serde(skip)]
    action: Action,
}

enum Action {
    Routes(Box<starter::Starter>),
    Register(&'static str),
    Schedule,
    None,
}

impl Step {
    fn new(id: impl Into<String>, description: impl Into<String>, status: Status) -> Self {
        Self {
            id: id.into(),
            description: description.into(),
            command: None,
            status,
            error: None,
            action: Action::None,
        }
    }

    fn fail(&mut self, error: impl ToString) {
        self.status = Status::Failed;
        self.error = Some(error.to_string());
    }
}

fn routes_step(selected: Option<PathBuf>, harnesses: &[String]) -> Step {
    if let Some(path) = selected {
        let mut step = Step::new(
            "route.init",
            format!("Keep existing routes at {}", path.display()),
            Status::Already,
        );
        if !path.is_file() {
            step.description = format!("Check selected routes at {}", path.display());
            step.fail(format!(
                "{} is unavailable; check CEDR_ROUTES",
                path.display()
            ));
        }
        return step;
    }
    let path = config::user_path();
    let mut step = Step::new("route.init", "Generate starter routes", Status::Planned);
    if harnesses.is_empty() {
        step.description = "Starter routes require a supported agent".into();
        step.status = Status::Skipped;
    } else {
        step.description = format!(
            "Write starter routes for {} to {}",
            harnesses.join(", "),
            path.display()
        );
        match starter::prepare(path, harnesses, None) {
            Ok(starter) => step.action = Action::Routes(Box::new(starter)),
            Err(e) => step.fail(e),
        }
    }
    step
}

fn mcp_step(agent: &'static str, registration: McpRegistration) -> Step {
    let mut step = Step::new(
        format!("mcp.{agent}"),
        format!("Register cedr MCP with {agent}"),
        Status::Planned,
    );
    match registration {
        McpRegistration::Absent => {
            step.description = format!("{agent} CLI is unavailable");
            step.status = Status::Skipped;
        }
        McpRegistration::Registered => {
            step.description = format!("Keep existing cedr MCP registration with {agent}");
            step.status = Status::Already;
        }
        McpRegistration::Missing => {
            step.command = Some(doctor::mcp_fix(agent).to_vec());
            step.action = Action::Register(agent);
        }
        McpRegistration::Unreadable => {
            step.fail(format!(
                "{agent} mcp list failed; check the existing registration"
            ));
        }
    }
    step
}

fn schedule_step(existing: Result<bool, impl ToString>) -> Step {
    match existing {
        Ok(true) => Step::new(
            "monitor.schedule",
            "Keep existing Monitor timer",
            Status::Already,
        ),
        Ok(false) => Step {
            command: Some(vec!["cedr", "monitor", "schedule"]),
            action: Action::Schedule,
            ..Step::new(
                "monitor.schedule",
                "Install Monitor's push timer for use with an organization",
                Status::Planned,
            )
        },
        Err(e) => Step::new(
            "monitor.schedule",
            format!("Monitor timer unavailable ({})", e.to_string()),
            Status::Skipped,
        ),
    }
}

fn apply(step: &mut Step) -> Result<Status, String> {
    match &step.action {
        Action::Routes(starter) => {
            starter.write().map_err(|e| e.to_string())?;
        }
        Action::Register(agent) => {
            let command = doctor::mcp_fix(agent);
            match doctor::mcp_registration(agent) {
                McpRegistration::Registered => return Ok(Status::Already),
                McpRegistration::Missing => {}
                _ => {
                    return Err(format!(
                        "{agent} mcp list failed; check the existing registration"
                    ));
                }
            }
            let result =
                doctor::run_mcp(agent, &command[1..]).map_err(|e| format!("{agent}: {e}"))?;
            if !result.succeeded() {
                return Err(format!(
                    "{} failed; run it directly for details",
                    command.join(" ")
                ));
            }
        }
        Action::Schedule => {
            if monitor::schedule_exists().map_err(|e| e.to_string())? {
                return Ok(Status::Already);
            }
            monitor::schedule_new().map_err(|e| e.to_string())?;
        }
        Action::None => unreachable!("only planned steps are applied"),
    }
    Ok(Status::Applied)
}

fn print_step(out: &mut dyn Write, step: &Step) -> io::Result<()> {
    writeln!(
        out,
        "{} {}: {}",
        step.status.as_str(),
        step.id,
        step.description
    )?;
    if let Some(command) = &step.command {
        writeln!(out, "  {}", command.join(" "))?;
    }
    if let Some(error) = &step.error {
        writeln!(out, "  {error}")?;
    }
    Ok(())
}

fn confirm(out: &mut dyn Write, description: &str) -> io::Result<bool> {
    write!(out, "{description}? [y/N] ")?;
    out.flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

pub fn run(args: Args) -> Result<ExitCode, String> {
    let root = std::env::current_dir().map_err(|e| format!("init: {e}"))?;
    let mut out: Box<dyn Write> = match args.format {
        Format::Text => Box::new(io::stdout().lock()),
        Format::Json => Box::new(io::stderr().lock()),
    };
    let result =
        setup(&root, args.yes, args.dry_run, &mut out).map_err(|e| format!("init: {e}"))?;
    if matches!(args.format, Format::Json) {
        println!("{}", serde_json::json!({"steps": result.0}));
    }
    Ok(crate::exit(result.1))
}

fn setup(
    root: &Path,
    yes: bool,
    preview: bool,
    out: &mut dyn Write,
) -> io::Result<(Vec<Step>, bool)> {
    let non_interactive = !io::stdin().is_terminal() && !yes && !preview;
    let preview = preview || non_interactive;
    if non_interactive {
        writeln!(out, "No terminal and no --yes; showing a dry run.")?;
    }
    let harnesses = starter::detect().map_err(io::Error::other)?;
    let selected = config::locate(None, root).ok().map(|located| located.path);
    let mut steps = vec![routes_step(selected, &harnesses)];
    for &agent in doctor::MCP_AGENTS {
        steps.push(mcp_step(agent, doctor::mcp_registration(agent)));
    }
    steps.push(schedule_step(monitor::schedule_exists()));
    for step in &steps {
        print_step(out, step)?;
    }
    out.flush()?;
    if !preview {
        for step in steps.iter_mut().filter(|s| s.status == Status::Planned) {
            if yes || confirm(out, &step.description)? {
                match apply(step) {
                    Ok(status) => step.status = status,
                    Err(e) => step.fail(e),
                }
            } else {
                step.status = Status::Skipped;
            }
            print_step(out, step)?;
        }
        writeln!(out, "\nDoctor: {}", doctor::summary(&doctor::checks(root)))?;
        writeln!(
            out,
            "Monitor: cedr monitor\nGuard lint: cedr guard lint\nSync: cedr sync init\nRoute: cedr route show review.code\nConnect an organization: https://cedr.dev/docs"
        )?;
    }
    let failed = !preview && steps.iter().any(|step| step.status == Status::Failed);
    Ok((steps, failed))
}

//! `fpk machine …` and `--machine`: saving SSH machines, and running a command
//! on one.

use std::{
    ffi::OsString,
    io::{self, BufRead as _, IsTerminal as _, Write as _},
    process::ExitCode,
};

use crate::{
    client::{
        machines::{self, Machine},
        remote::{self, Reach, Wanted},
        ssh::{Prompt, REMOTE_FPK, Ssh, quote},
    },
    config::Config,
    home::Home,
};

/// Probes the host, installs what this client needs there if the user agrees,
/// makes sure its server runs, and saves it. Declined or failed, nothing is
/// saved. On a label already saved, probes again and offers to update.
pub(super) async fn add(home: &Home, label: &str, target: &str, yes: bool) -> Result<(), String> {
    if let Some(reason) = machines::invalid_label(label) {
        return Err(format!("cannot name a machine `{label}`: {reason}"));
    }
    let config = Config::load(home)?;
    let mut saved = machines::load(home)?;
    if let Some(existing) = saved.iter().find(|machine| machine.label == label)
        && existing.ssh != target
    {
        return Err(format!(
            "`{label}` is already saved for {}; remove it first to point it elsewhere",
            existing.ssh
        ));
    }
    let wanted = Wanted::for_this_build()?;
    let ssh =
        Ssh::new(home, &config, target, Prompt::Allowed).map_err(|error| error.to_string())?;

    println!("probing {target}");
    let probe = match remote::probe(&ssh).await {
        Reach::Probed(probe) => probe,
        Reach::NeedsLogin(why) | Reach::Unreachable(why) => {
            return Err(format!("cannot reach {target}: {why}"));
        }
    };
    if let Some(why) = wanted.mismatch(&probe) {
        let question = format!(
            "{target}: {why}. Install {} into ~/{}/hosts/<its machine ID>/bin/fpk there?",
            wanted.describe(),
            crate::home::DIRECTORY_NAME
        );
        if !(yes || confirm(&question)?) {
            return Err(format!(
                "nothing was installed, and `{label}` was not saved"
            ));
        }
        remote::install(&ssh, &wanted).await?;
        // A server already running is still the old build: replacing it
        // cancels its tasks, so that is asked separately, as herdr asks.
        if probe.server_running {
            let question = format!(
                "{target}'s server is still the old build. Restart it? Its tasks are cancelled."
            );
            if yes || confirm(&question)? {
                let stop = ssh
                    .command(&format!("{REMOTE_FPK} server stop"))
                    .status()
                    .await
                    .map_err(|error| format!("cannot run ssh: {error}"))?;
                if !stop.success() {
                    return Err(format!("stopping the old server on {target} failed"));
                }
            }
        }
        match remote::probe(&ssh).await {
            Reach::Probed(after) if wanted.mismatch(&after).is_none() => {}
            Reach::Probed(after) => {
                return Err(format!(
                    "{target} still has {} after installing",
                    after.describe()
                ));
            }
            Reach::NeedsLogin(why) | Reach::Unreachable(why) => {
                return Err(format!("cannot reach {target} after installing: {why}"));
            }
        }
    } else {
        println!("{target} has {}", probe.describe());
    }
    remote::start_server(&ssh).await?;

    if saved.iter().any(|machine| machine.label == label) {
        println!("`{label}` is up to date");
    } else {
        saved.push(Machine::new(label, target));
        machines::save(home, &saved)?;
        println!("saved `{label}`; the terminal UI lists it");
    }
    Ok(())
}

pub(super) fn list(home: &Home) -> Result<(), String> {
    let saved = machines::load(home)?;
    if saved.is_empty() {
        println!("no machines saved; add one with `fpk machine add <label> <ssh-target>`");
        return Ok(());
    }
    let width = saved
        .iter()
        .map(|machine| machine.label.len())
        .max()
        .unwrap_or(0);
    for machine in saved {
        let off = if machine.enabled { "" } else { "  (disabled)" };
        println!("{:<width$}  {}{off}", machine.label, machine.ssh);
    }
    Ok(())
}

pub(super) fn remove(home: &Home, label: &str) -> Result<(), String> {
    let mut saved = machines::load(home)?;
    let before = saved.len();
    saved.retain(|machine| machine.label != label);
    if saved.len() == before {
        return Err(format!("no machine is saved as `{label}`"));
    }
    machines::save(home, &saved)?;
    println!("forgot `{label}`; its server and tasks keep running");
    Ok(())
}

/// Opens the shared connection interactively, so a password or a
/// keyboard-interactive login can be answered here; background reconnects
/// ride it afterwards without asking.
pub(super) async fn login(home: &Home, label: &str) -> Result<(), String> {
    let ssh = ssh_for(home, label, Prompt::Allowed)?;
    let status = ssh
        .command("true")
        .status()
        .await
        .map_err(|error| format!("cannot run ssh: {error}"))?;
    if status.success() {
        println!("signed in to `{label}`");
        Ok(())
    } else {
        Err(format!("signing in to `{label}` failed ({status})"))
    }
}

/// Runs `fpk <args>` on a saved machine, passing its output through and
/// exiting as it exits.
pub(super) async fn run_remote(
    home: &Home,
    label: &str,
    args: &[OsString],
) -> Result<ExitCode, String> {
    let ssh = ssh_for(home, label, Prompt::Allowed)?;
    let quoted: Vec<String> = args
        .iter()
        .map(|arg| quote(&arg.to_string_lossy()))
        .collect();
    let status = ssh
        .command(&format!("{REMOTE_FPK} {}", quoted.join(" ")))
        .status()
        .await
        .map_err(|error| format!("cannot run ssh: {error}"))?;
    Ok(status
        .code()
        .and_then(|code| u8::try_from(code).ok())
        .map_or(ExitCode::FAILURE, ExitCode::from))
}

fn ssh_for(home: &Home, label: &str, prompt: Prompt) -> Result<Ssh, String> {
    let config = Config::load(home)?;
    let machine = machines::load(home)?
        .into_iter()
        .find(|machine| machine.label == label)
        .ok_or_else(|| format!("no machine is saved as `{label}`; see `fpk machine list`"))?;
    Ssh::new(home, &config, &machine.ssh, prompt).map_err(|error| error.to_string())
}

/// Asks a yes-or-no question on the terminal. Without one, the answer is no:
/// nothing is installed on a host by a script that did not say `--yes`.
fn confirm(question: &str) -> Result<bool, String> {
    if !io::stdin().is_terminal() {
        println!("{question} (no terminal to ask on; pass --yes to agree)");
        return Ok(false);
    }
    print!("{question} [y/N] ");
    io::stdout().flush().map_err(|error| error.to_string())?;
    let mut answer = String::new();
    io::stdin()
        .lock()
        .read_line(&mut answer)
        .map_err(|error| error.to_string())?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes"))
}

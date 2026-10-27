use anstream::println;
use anyhow::Result;

use super::Session;
use crate::cli::GlobalOpts;
use crate::config::{agent_names, profile_names};
use crate::style::{DIM, ERROR, LABEL, OK};

pub fn agents(opts: &GlobalOpts) -> Result<()> {
    let session = Session::open(opts)?;
    let default = session.resolve().ok().map(|r| r.agent.name);
    for name in agent_names(&session.discovered, session.platform) {
        let is_default = default.as_ref() == Some(&name);
        let marker = if is_default { "*" } else { " " };
        let name_style = if is_default {
            LABEL
        } else {
            anstyle::Style::new()
        };
        match session.resolve_agent(Some(&name)) {
            Ok(r) => {
                let profile = r.nono.profile.as_deref().unwrap_or("-");
                println!(
                    "{OK}{marker}{OK:#} {name_style}{name:<20}{name_style:#} {:<16} \
                     {DIM}profile:{DIM:#} {profile}",
                    r.agent.command
                );
            }
            Err(e) => println!(
                "{OK}{marker}{OK:#} {name_style}{name:<20}{name_style:#} {ERROR}error:{ERROR:#} {e:#}"
            ),
        }
    }
    let profiles = profile_names(&session.discovered, session.platform);
    if !profiles.is_empty() {
        let profiles: Vec<_> = profiles.into_iter().collect();
        println!("\n{LABEL}profiles (-p):{LABEL:#} {}", profiles.join(", "));
    }
    Ok(())
}

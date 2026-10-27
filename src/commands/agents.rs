use anyhow::Result;

use super::Session;
use crate::cli::GlobalOpts;
use crate::config::{agent_names, profile_names};

pub fn agents(opts: &GlobalOpts) -> Result<()> {
    let session = Session::open(opts)?;
    let default = session.resolve().ok().map(|r| r.agent.name);
    for name in agent_names(&session.discovered, session.platform) {
        let marker = if default.as_ref() == Some(&name) {
            "*"
        } else {
            " "
        };
        match session.resolve_agent(Some(&name)) {
            Ok(r) => {
                let profile = r.nono.profile.as_deref().unwrap_or("-");
                println!(
                    "{marker} {name:<20} {:<16} profile: {profile}",
                    r.agent.command
                );
            }
            Err(e) => println!("{marker} {name:<20} error: {e:#}"),
        }
    }
    let profiles = profile_names(&session.discovered, session.platform);
    if !profiles.is_empty() {
        let profiles: Vec<_> = profiles.into_iter().collect();
        println!("\nprofiles (-p): {}", profiles.join(", "));
    }
    Ok(())
}

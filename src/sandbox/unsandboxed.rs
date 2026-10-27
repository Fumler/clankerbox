use anyhow::{Result, bail};

use super::{Launch, Request, Sandbox};

/// Runs the agent directly. Policy grants are ignored; only the environment applies.
pub struct Unsandboxed;

impl Sandbox for Unsandboxed {
    fn build(&self, request: &Request<'_>) -> Result<Launch> {
        if !request.passthrough.is_empty() {
            bail!("sandbox arguments were given, but the sandbox is disabled");
        }
        Ok(Launch {
            program: request.program.into(),
            args: request.args.clone(),
            env: request.policy.env.clone(),
            cwd: request.cwd.to_path_buf(),
            files: Vec::new(),
        })
    }
}

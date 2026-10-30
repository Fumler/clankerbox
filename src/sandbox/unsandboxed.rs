use std::ffi::OsString;

use anyhow::{Result, bail};

use super::{Launch, Request, Sandbox};

/// Runs the agent directly. Policy grants are ignored; only the environment applies.
pub struct Unsandboxed;

impl Sandbox for Unsandboxed {
    fn build(&self, request: &Request<'_>) -> Result<Launch> {
        if !request.run.sandbox.is_empty() {
            bail!("sandbox arguments were given, but the sandbox is disabled");
        }
        Ok(Launch {
            program: request.program.into(),
            args: request
                .config_args
                .iter()
                .map(OsString::from)
                .chain(request.run.agent_args.iter().cloned())
                .collect(),
            env: request.policy.env.clone(),
            env_remove: request.policy.env_remove.clone(),
            cwd: request.cwd.to_path_buf(),
            files: Vec::new(),
        })
    }

    fn base_access(&self) -> Vec<String> {
        vec!["no sandbox: everything you can access".to_owned()]
    }
}

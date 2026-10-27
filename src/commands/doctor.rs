use std::process::{Command, ExitCode};

use anyhow::Result;

use super::Session;
use crate::cli::GlobalOpts;
use crate::config::model::BackendKind;
use crate::config::trust::TrustStore;
use crate::context;

#[derive(Default)]
struct Report {
    passed: usize,
    failed: usize,
}

impl Report {
    fn ok(&mut self, message: impl std::fmt::Display) {
        self.passed += 1;
        println!("✓ {message}");
    }
    fn fail(&mut self, message: impl std::fmt::Display) {
        self.failed += 1;
        println!("✗ {message}");
    }
    fn finish(&self) -> ExitCode {
        println!("\n{} passed, {} failed", self.passed, self.failed);
        if self.failed == 0 {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        }
    }
    fn check(&mut self, ok: bool, message: impl std::fmt::Display) {
        if ok {
            self.ok(message);
        } else {
            self.fail(message);
        }
    }
}

pub fn doctor(opts: &GlobalOpts) -> Result<ExitCode> {
    let mut report = Report::default();

    let session = match Session::open(opts) {
        Ok(session) => session,
        Err(e) => {
            report.fail(format!("config: {e:#}"));
            return Ok(report.finish());
        }
    };
    let store = TrustStore::load(&session.dirs.state)?;
    for layer in &session.discovered.layers {
        let trusted = store.is_trusted(layer);
        let note = if trusted {
            ""
        } else {
            " (untrusted: run `cb trust`)"
        };
        report.check(trusted, format!("config {layer}{note}"));
    }

    let resolved = match session.resolve() {
        Ok(r) => r,
        Err(e) => {
            report.fail(format!("resolve: {e:#}"));
            return Ok(report.finish());
        }
    };
    for warning in &resolved.warnings {
        report.fail(warning);
    }
    let agent = &resolved.agent;
    report.check(
        context::which(&agent.command).is_some(),
        format!(
            "agent `{}`: command `{}` on $PATH",
            agent.name, agent.command
        ),
    );

    if resolved.backend == BackendKind::Nono {
        match Command::new("nono").arg("--version").output() {
            Ok(out) if out.status.success() => {
                report.ok(String::from_utf8_lossy(&out.stdout).trim());
                if let Some(profile) = &resolved.nono.profile {
                    check_profile(&mut report, profile);
                }
            }
            _ => report.fail("nono not found on $PATH (see https://nono.sh)"),
        }
    }
    Ok(report.finish())
}

fn check_profile(report: &mut Report, profile: &str) {
    let found = Command::new("nono")
        .args(["profile", "show", profile])
        .output()
        .is_ok_and(|out| out.status.success());
    if found {
        report.ok(format!("nono profile `{profile}`"));
    } else {
        report.fail(format!(
            "nono profile `{profile}` not found or invalid; check `nono profile show {profile}`, \
             or install it (`nono search`, `nono pull <pack>`)"
        ));
    }
}

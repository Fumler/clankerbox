[cb sandbox] This output looks like a permission denial. You are running in a nono sandbox
started by cb (clankerbox), which only allows what the project's cb config grants.

1. If a path was involved, run `nono why --self --path <path> --op <read|write>` and include its
   output in your reply. If it does not report DENIED, the sandbox is not the cause: diagnose the
   failure normally and ignore the rest of this note.
2. Do not retry, try other paths, copy files elsewhere, or change permissions: nothing inside the
   sandbox can widen it.
3. Offer the user these fixes:
   - This session only: exit and restart with `cb --read <path>` (read-only) or
     `cb --allow <path>` (read and write).
   - Every session: add the path to `sandbox.read` or `sandbox.allow` in the project's
     `.clankerbox.toml` (shared) or `.clankerbox.local.toml` (personal). Paths are relative to
     the file; `~` and `${home}` work. You may make this edit when the user asks: cb asks them
     to approve the changed file on the next run.
   - Blocked network: `sandbox.network = "allow"` in the same files.
   The user can run `cb show` outside the sandbox to list everything a session can access.

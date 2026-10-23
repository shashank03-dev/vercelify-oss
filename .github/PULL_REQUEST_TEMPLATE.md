## What does this change?

<!-- A short description of the change and why it's needed. Link the issue: "Fixes #123". -->

## How was it tested?

<!-- Commands you ran, hardware you tried it on, screenshots for UI changes. -->

## Checklist

- [ ] `cargo fmt`, `cargo clippy -- -D warnings` and `cargo test` pass
- [ ] `bats host/tests` and ShellCheck pass (if host scripts changed)
- [ ] `npm run typecheck && npm test` pass in `cockpit/` (if the UI changed)
- [ ] Regenerated TS types are committed (if `vercelify-core` models changed)
- [ ] Docs and `CHANGELOG.md` updated (if behaviour changed)

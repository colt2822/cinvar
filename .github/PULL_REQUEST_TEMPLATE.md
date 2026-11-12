## What and why

## How it was tested
<!-- Commands you ran. Say which tests need optional dependencies and whether you ran them. -->
- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --all-targets -- -D warnings`
- [ ] `cargo test`
- [ ] Docs updated (README / docs/ / CHANGELOG.md) if behaviour changed

## Safety checklist
- [ ] No secrets, personal paths or generated media committed
- [ ] New file/path inputs stay inside the project (or an explicitly configured root)
- [ ] Nothing spends money or sends files off-device without an explicit opt-in
- [ ] Claims in docs match what the tests actually exercise (mock vs. real is stated)

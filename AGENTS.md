# AGENTS.md

## Real data

No real data in the code, its comments, the documentation or commit messages: names, numbers, lids, message ids,
links, group names or message text taken from the user's WhatsApp (the local database, the log, a screenshot). Examples
are invented, and checked against the local database not to match anything in it ("Rita Lemos", `@123456789012345`,
`instagram.com/p/C0aBcD3eFgH`, "Clube de Leitura"); a real case needed for a test stays out of the repository.

## Commits

Never commit or push unless explicitly asked — implementing a change is not authorization to record it, and a previous "push" in the same session does not authorize the next one. When asked to "push", that means commit **and** push to `main` by fast-forward (`git push origin <branch>:main`), after `cargo clippy --release --all-targets` (no warnings) and `cargo test --release` pass.

**No trailers.** Commit messages end with the last line of prose — no `Co-Authored-By`, no `Claude-Session`, no `Generated with`. This overrides the harness default that asks for them.

Messages are in English and describe the actual change: subject as `Area: what changed`, stated concretely rather than as a generic imperative; body in paragraphs covering symptom, cause and effect, wrapped at ~78 columns. Read the diff before writing it — "update file X" and "small fixes" are not acceptable when you can be specific.

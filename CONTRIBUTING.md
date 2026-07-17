# Contributing

Contributions are welcome — bug reports, ideas, and pull requests all help.

## AI-assisted contributions

AI-assisted contributions are welcome too. But **please look over the work a
little first** before opening a PR — don't just paste whatever a model produced.
Concretely, that means:

- **Build it**: `cargo build` and `cargo test` should pass, with no new warnings.
- **Run it**: actually launch the app and exercise the thing you changed. A lot
  of this is UI and audio behaviour that tests don't cover, so click through it.
- **Read the diff**: make sure it does what you think, doesn't drag in stray
  changes, and matches the surrounding style (naming, comment density, the
  deferred-`Action` pattern in `app.rs`, etc.).
- **Say what you did**: in the PR, describe the change and how you checked it
  works. "The model wrote it and it compiles" is not enough.

The bar is the same whether a human or a model wrote the code: it should be
correct, tested by hand, and something you understand well enough to explain.

## Pull requests

- Keep PRs focused — one logical change per PR is easiest to review.
- Match the existing formatting (`cargo fmt`) and keep clippy quiet where
  reasonable.
- If you're changing behaviour, note it in the PR description.

## License

By contributing, you agree that your contributions will be dual-licensed under
the [MIT](LICENSE-MIT) and [Apache-2.0](LICENSE-APACHE) licenses, the same terms
as the rest of the project.

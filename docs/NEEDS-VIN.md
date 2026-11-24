# Things only Vin can do

These are the last steps to ship 1.0 (see [ROADMAP.md](ROADMAP.md)). Each
needs the repository owner; none needs a key or a paid service.

1. **Merge the pull request** into `main`.
2. **Publish the playground.** In **Settings → Pages**, set **Source** to
   **GitHub Actions**. Then in the **Actions** tab choose **Publish the
   playground** and press **Run workflow**. The site appears at
   https://vincentwesley.github.io/little-sql/, the link at the top of the
   README.
3. **Release 1.0.0.** On `main`, run:

   ```bash
   git tag v1.0.0
   ```

   ```bash
   git push origin v1.0.0
   ```

   The **Release** workflow then builds the shell for Linux, macOS and
   Windows and the playground, and publishes them as a GitHub release with
   the notes from CHANGELOG.md. The README's Download link points there.
4. **Optional:** set the repository description (for example "A
   SQLite-compatible database engine written from scratch in Rust, with a
   browser playground"), add topics such as `rust`, `sqlite`, `database`,
   `webassembly`, and pin it on your profile.

Any static host also works for the playground: serve the contents of
`playground/dist/` (largest file: the 18 MB demo database).

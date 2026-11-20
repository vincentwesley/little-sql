# Things only Vin can do

Nothing here blocks the project; each item is optional.

## Optional: publish the playground

The playground is a plain static site; finishing the project did not depend
on hosting it. To put it online with GitHub Pages (free, no keys):

1. Merge the pull request into `main`.
2. In the repository's **Settings → Pages**, set **Source** to
   **GitHub Actions**.
3. Add a workflow that runs `scripts/build-playground.sh` and uploads
   `playground/dist/` with `actions/upload-pages-artifact`, then
   `actions/deploy-pages` (the CI job already builds the site; the deploy
   step needs the Pages permission that only the repository owner can
   grant).

Any other static host works the same way: serve the contents of
`playground/dist/` (largest file: the 18 MB demo database).

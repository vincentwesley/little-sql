# Things only Vin can do

Nothing here blocks the project; each item is optional.

## Optional: publish the playground

The playground is a plain static site; finishing the project did not depend
on hosting it. To put it online with GitHub Pages (free, no keys):

1. Merge the pull request into `main`.
2. In the repository's **Settings → Pages**, set **Source** to
   **GitHub Actions** (only the repository owner can do this).
3. In the **Actions** tab, choose **Publish the playground** and press
   **Run workflow**. It builds `playground/dist/` and deploys it; the page
   address appears in the run's summary.

The workflow (`.github/workflows/pages.yml`) only runs when started by
hand, so it cannot fail on its own before Pages is enabled.

Any other static host works the same way: serve the contents of
`playground/dist/` (largest file: the 18 MB demo database).

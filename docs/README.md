# doko docs

The documentation site at <https://wheregmis.github.io/doko/>, built with [Astro Starlight](https://starlight.astro.build).

```bash
cd docs
npm install
npm run dev      # http://localhost:4321/doko/
npm run build    # static site in dist/
```

Pages live in `src/content/docs/`. Much of the guide content mirrors the repository README, so update both when a feature changes. Pushes to `master` that touch `docs/` deploy the site through `.github/workflows/docs.yml`.

Astro needs Node 22.23 or newer; CI builds with Node 24.

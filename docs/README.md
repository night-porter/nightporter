# Night Porter docs

Live at https://docs.night-porter.com. VitePress, Markdown in this folder.

```sh
npm install
npm run dev      # http://localhost:5173
npm run build    # → .vitepress/dist
```

## Deploying to Cloudflare Pages

Connect the `night-porter/nightporter` repo with framework preset **VitePress**, root directory `docs`,
build command `npm run build`, output directory `.vitepress/dist`. Set the build watch paths to `docs/*`
so Rust-only commits don't rebuild the docs. `.node-version` pins Node 24, the same as the main site.
`public/_headers` sets the security headers.

`package.json` overrides `vite` to the latest 6.x: VitePress 1.6.4 ships Vite 5, whose dev server
has open advisories. Drop the override once VitePress 2 is stable. Custom domain: `docs.night-porter.com`.

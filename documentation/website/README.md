<!-- SPDX-FileCopyrightText: © 2026 Vahid Mohammadi -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# vedaos.org

The website of Veda, at [www.vedaos.org](https://www.vedaos.org). One static
page, with no build step and no dependencies: what is in this folder is what
is published.

| File | What it is |
|------|------------|
| `index.html` | The page, with its CSS inline, its structured data and every word of its content (search engines and screen readers get it all without running a script) |
| `assets/site.js` | The motion: the hero's sky, the agent's figure, the simulated conversation, the scroll effects |
| `assets/fonts/` | Inter (400 and 600) as WOFF2, from `assets/fonts/` of the repository (SIL Open Font License, `OFL.txt`) |
| `assets/img/` | Screenshots (640, 960 and 1280 pixels wide), the social image `og.jpg` and the app icons |
| `404.html` | The page for addresses that do not exist |
| `robots.txt`, `sitemap.xml`, `site.webmanifest`, favicons | For search engines, browsers and home screens |
| `CNAME` | The domain, for GitHub Pages |

## Veda's visual language

The animations are Veda's own, not look-alikes:

* **The hero's sky** is the *Aurora* wallpaper
  (`tools/assetgen/src/wallpapers.rs`), with its blooms and its three silky
  ribbons (their cores, filaments, halos and curtains of rays), rewritten
  as a WebGL shader, so that the ribbons flow. It is drawn below the screen's
  resolution (it is all soft light), and only while it is in view.
* **The ring and its light** are the boot splash and the startup sequence
  (`libraries/splash`, `services/compositor/src/startup.rs`): the same
  proportions, the same falloff of the light, its breath every 2.6 seconds
  and its glint going round every 2 seconds, drawn in CSS so that they are
  there before any script runs. Scrolling, the ring flies into the logo in
  the header.
* **The agent's figure** in the demo and in "Add a key. Watch it wake." is a
  port of `applications/shell/src/presence.rs` and `agent.rs`: OS1's ring
  from *Her*, breathing while it listens, trembling and glowing while it
  speaks, a light running round it while it thinks, and the film's coil (a
  ribbon wound three times around a spinning loop) that turns to face you and
  becomes the ring as the agent wakes.

Colours, type (Inter) and icons (the paths of `libraries/ui/src/icons.rs`)
are the system's too.

## Performance and accessibility

* No framework, no external requests: the HTML (with its CSS) and the script
  are about 30 KB compressed; the fonts (230 KB) are preloaded, and the text
  shows at once in a fallback with the same metrics; images are responsive
  and lazy below the fold; sections below the fold skip rendering until they
  come near (`content-visibility`).
* Every animation stops when it is off screen or the tab is hidden, and the
  sky lowers its resolution if frames come slowly.
* With `prefers-reduced-motion`, nothing moves: the figure is shown at rest
  and the scroll-driven parts are laid out as plain sections.
* Without JavaScript the page is complete and readable.

## Previewing

Any static file server will do, from this folder, for example:

```bash
python -m http.server 8000
```

(Fonts do not load from `file://`.)

## Publishing

Nothing publishes this folder automatically: upload it as it is to the static
host that serves www.vedaos.org (any will do: Cloudflare Pages, GitHub Pages,
Netlify, an S3 bucket). `CNAME` gives the domain to GitHub Pages; the other
hosts ignore it.

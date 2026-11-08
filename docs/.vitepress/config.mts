import { defineConfig, type HeadConfig } from 'vitepress'
import { readFileSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'

const REPO = 'https://github.com/night-porter/nightporter'
const SITE = 'https://docs.night-porter.com'
// The main site's social card; the docs share the brand.
const OG_IMAGE = 'https://night-porter.com/og.png'
const HEART =
  '<svg viewBox="0 0 27 26" width="14" height="13" role="img" aria-label="love" style="display:inline-block;vertical-align:-1px"><path d="M13 23 11.4 21.5C5.3 16 1.5 12.5 1.5 8.2 1.5 4.8 4.1 2.2 7.5 2.2c1.9 0 4.3.9 5.5 2.4 1.2-1.5 3.6-2.4 5.5-2.4 3.4 0 6 2.6 6 6 0 4.3-3.8 7.8-9.9 13.3z" fill="#ff3b6b" stroke="#000" stroke-width="1.6" stroke-linejoin="round"/></svg>'

// Sidebar order, reused for llms.txt so agents read the docs in the same order people do.
const SIDEBAR = [
  {
    text: 'Guide',
    items: [
      { text: 'Getting started', link: '/getting-started' },
      { text: 'Daily use', link: '/daily-use' },
      { text: 'herdr, Rex and scripts', link: '/herdr' },
      { text: 'Troubleshooting', link: '/troubleshooting' },
    ],
  },
  {
    text: 'Configuration',
    items: [
      { text: 'Reference', link: '/configuration' },
      { text: 'Advanced recipes', link: '/advanced' },
      { text: 'Writing packs', link: '/writing-packs' },
    ],
  },
  {
    text: 'Project',
    items: [
      { text: 'Threat model', link: '/threat-model' },
      { text: 'Reporting issues', link: '/reporting-issues' },
      { text: 'Contributing', link: '/contributing' },
    ],
  },
]

const frontmatter = (md: string) => md.match(/^---\n([\s\S]*?)\n---\n/)
const describe = (md: string) => frontmatter(md)?.[1].match(/^description: "?(.*?)"?$/m)?.[1] ?? ''
const body = (md: string, dir: string) =>
  md
    .replace(frontmatter(md)?.[0] ?? '', '')
    // `file#region` takes only that region, like VitePress does. ponytail: `{lines}` selectors aren't used, so not supported.
    .replace(/<!--@include: ([^#{]+?)(?:#([\w-]+))?-->/g, (_, f, region) => {
      const text = readFileSync(join(dir, f.trim()), 'utf8')
      if (!region) return text
      const start = text.indexOf(`<!-- #region ${region} -->`)
      if (start < 0) throw new Error(`llms: no region ${region} in ${f}`)
      const end = text.indexOf(`<!-- #endregion ${region} -->`, start)
      return text.slice(start + `<!-- #region ${region} -->`.length, end < 0 ? undefined : end).trim()
    })
    .trim()

// llms.txt (index) and llms-full.txt (every page) for AI agents: https://llmstxt.org
function writeLlms(srcDir: string, outDir: string) {
  const pages = SIDEBAR.map((g) => ({ ...g, items: g.items.map((i) => ({ ...i, md: readFileSync(join(srcDir, `${i.link.slice(1)}.md`), 'utf8') })) }))
  const index = [
    '# Night Porter docs',
    '',
    '> Night Porter handles your PRs while you’re away: a daemon and terminal UI that watch GitHub, GitLab and Forgejo/Gitea for review comments, failing CI and merge conflicts on your PRs and MRs, then run Claude Code, OpenCode or Codex on your local clone, within the rules you set. It never merges, force-pushes, closes or approves.',
    '',
    `Every page as one file: ${SITE}/llms-full.txt. Website: https://night-porter.com. Source: ${REPO}.`,
    ...pages.flatMap((g) => ['', `## ${g.text}`, '', ...g.items.map((i) => `- [${i.text}](${SITE}${i.link}): ${describe(i.md)}`)]),
    '',
  ].join('\n')
  const full = pages.flatMap((g) => g.items.map((i) => `<!-- ${SITE}${i.link} -->\n\n${body(i.md, srcDir)}`)).join('\n\n---\n\n')
  writeFileSync(join(outDir, 'llms.txt'), index)
  writeFileSync(join(outDir, 'llms-full.txt'), `${full}\n`)
}

export default defineConfig({
  title: 'Night Porter',
  description: 'Docs for Night Porter: handles your PRs while you’re away.',
  cleanUrls: true,
  srcExclude: ['README.md'],
  lastUpdated: true,
  sitemap: { hostname: SITE },
  buildEnd: (config) => writeLlms(config.srcDir, config.outDir),
  // Per-page social card and canonical URL; the site-wide tags are in `head` below.
  transformHead({ pageData, siteData }): HeadConfig[] {
    const path = pageData.relativePath.replace(/(index)?\.md$/, '')
    const url = `${SITE}/${path}`
    const title = pageData.frontmatter.layout === 'home' ? `${siteData.title} docs` : `${pageData.title} | ${siteData.title}`
    const description = pageData.description || siteData.description
    return [
      ['link', { rel: 'canonical', href: url }],
      ['meta', { property: 'og:url', content: url }],
      ['meta', { property: 'og:title', content: title }],
      ['meta', { property: 'og:description', content: description }],
      ['meta', { name: 'twitter:title', content: title }],
      ['meta', { name: 'twitter:description', content: description }],
    ]
  },
  head: [
    ['meta', { name: 'theme-color', content: '#07080d' }],
    ['meta', { property: 'og:type', content: 'website' }],
    ['meta', { property: 'og:site_name', content: 'Night Porter docs' }],
    ['meta', { property: 'og:image', content: OG_IMAGE }],
    ['meta', { property: 'og:image:width', content: '1200' }],
    ['meta', { property: 'og:image:height', content: '630' }],
    ['meta', { property: 'og:image:alt', content: 'A comic night porter with a cart of pull requests outside the neon Hotel Pull Requests.' }],
    ['meta', { name: 'twitter:card', content: 'summary_large_image' }],
    ['meta', { name: 'twitter:site', content: '@NightPorterDev' }],
    ['meta', { name: 'twitter:image', content: OG_IMAGE }],
    ['link', { rel: 'alternate', type: 'text/plain', title: 'llms.txt', href: '/llms.txt' }],
    ['link', { rel: 'icon', href: '/favicon.ico', sizes: '48x48' }],
    ['link', { rel: 'icon', href: '/favicon.svg', type: 'image/svg+xml' }],
    ['link', { rel: 'apple-touch-icon', href: '/apple-touch-icon.png' }],
  ],
  themeConfig: {
    // the wordmark already says "Night Porter"
    logo: { light: '/wordmark-light.svg', dark: '/wordmark-dark.svg', alt: 'Night Porter' },
    siteTitle: false,
    nav: [
      { text: 'Getting started', link: '/getting-started' },
      { text: 'Configuration', link: '/configuration' },
      { text: 'Website', link: 'https://night-porter.com' },
    ],
    sidebar: SIDEBAR,
    socialLinks: [
      { icon: 'github', link: REPO },
      { icon: 'x', link: 'https://x.com/NightPorterDev' },
      { icon: 'bluesky', link: 'https://bsky.app/profile/did:plc:zojgcz6b634zmhrkl7hjlozs' },
      { icon: 'mastodon', link: 'https://mastodon.social/@nightporter' },
    ],
    editLink: { pattern: `${REPO}/edit/main/docs/:path`, text: 'Edit this page on GitHub' },
    search: { provider: 'local' },
    footer: {
      message: `Built with ${HEART} by <a href="https://danlemon.com">Dan Lemon</a>`,
      copyright:
        'MIT or Apache-2.0 · <a href="https://night-porter.com">night-porter.com</a> · <a href="https://night-porter.com/support">Support</a>',
    },
  },
})

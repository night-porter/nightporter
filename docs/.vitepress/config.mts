import { defineConfig } from 'vitepress'

const REPO = 'https://github.com/night-porter/nightporter'

export default defineConfig({
  title: 'Night Porter',
  description: 'Docs for Night Porter: handles your PRs while you’re away.',
  cleanUrls: true,
  srcExclude: ['README.md'],
  lastUpdated: true,
  head: [
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
    sidebar: [
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
    ],
    socialLinks: [
      { icon: 'github', link: REPO },
      { icon: 'x', link: 'https://x.com/NightPorterDev' },
      { icon: 'bluesky', link: 'https://bsky.app/profile/did:plc:zojgcz6b634zmhrkl7hjlozs' },
      { icon: 'mastodon', link: 'https://mastodon.social/@nightporter' },
    ],
    editLink: { pattern: `${REPO}/edit/main/docs/:path`, text: 'Edit this page on GitHub' },
    search: { provider: 'local' },
    footer: {
      message:
        'MIT or Apache-2.0 · <a href="https://night-porter.com">night-porter.com</a> · <a href="mailto:hi@night-porter.com">hi@night-porter.com</a>',
    },
  },
})

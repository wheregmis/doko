// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

// Deployed to GitHub Pages as a project site: https://wheregmis.github.io/doko/
export default defineConfig({
	site: 'https://wheregmis.github.io',
	base: '/doko',
	integrations: [
		starlight({
			title: 'doko',
			description: 'A batteries-included Rust framework for Cloudflare Workers.',
			logo: { src: './src/assets/doko.svg' },
			favicon: '/favicon.svg',
			social: [{ icon: 'github', label: 'GitHub', href: 'https://github.com/wheregmis/doko' }],
			editLink: { baseUrl: 'https://github.com/wheregmis/doko/edit/master/docs/' },
			sidebar: [
				{
					label: 'Getting started',
					items: ['getting-started/installation', 'getting-started/first-app'],
				},
				{
					label: 'Guides',
					items: [
						'guides/configuration',
						'guides/handlers',
						'guides/generators',
						'guides/storage',
						'guides/cache',
						'guides/jobs-and-cron',
						'guides/durable-objects',
						'guides/auth',
						'guides/email',
						'guides/postgres',
						'guides/workers-ai',
						'guides/analytics-engine',
						'guides/testing',
					],
				},
				{
					label: 'Reference',
					items: ['reference/cli', 'reference/doko-toml', 'reference/api'],
				},
			],
		}),
	],
});

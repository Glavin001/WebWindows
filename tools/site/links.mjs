// Prints one-click links to the page's sample programs (runtime/web/samples.json)
// on a deployed site, as Markdown: each opens the page and runs the program
// on Wine with its suggested arguments.
//
//   node tools/site/links.mjs https://…/runtime/web/
import { readFileSync } from 'node:fs';

const page = new URL(process.argv[2]);
const samples = JSON.parse(readFileSync(new URL('../../runtime/web/samples.json', import.meta.url), 'utf8'));
const groups = new Map();
for (const s of samples) {
  const q = new URLSearchParams({ exe: new URL(`../../${s.path}`, page).pathname, wine: '1' });
  if (s.args) q.set('args', s.args);
  const [name, ...what] = s.label.split(': ');
  if (!groups.has(s.group)) groups.set(s.group, []);
  groups.get(s.group).push(`[${name}](${page.href}?${q})${what.length ? ` ${what.join(': ')}` : ''}`);
}
const lines = [`**[Open the page](${page.href})**, or run a test program in one click:`, ''];
for (const [group, links] of groups) lines.push(`- **${group}**: ${links.join(' · ')}`);
console.log(lines.join('\n'));

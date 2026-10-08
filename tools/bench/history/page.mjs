// Builds the speed log page from run.mjs's results, with the data inlined:
//
//   node tools/bench/history/page.mjs suite.json [results.json abl.json] [-o page.html]
//
// suite.json is a full `suite.mjs --json` run of the last checkpoint (the
// reference times); results and ablations default to $HIST_DIR's, CPU
// profiles come from $HIST_DIR/prof (profile.sh).
import { existsSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const H = process.env.HIST_DIR ?? resolve(here, '../../../target/history');
const read = (f) => (existsSync(f) ? JSON.parse(readFileSync(f, 'utf8')) : {});
const argv = process.argv.slice(2);
const o = argv.indexOf('-o');
const outFile = o >= 0 ? argv.splice(o, 2)[1] : join(H, 'page.html');
const [suiteFile, histFile = join(H, 'results.json'), ablFile = join(H, 'abl.json')] = argv;
if (!suiteFile) throw new Error('usage: page.mjs suite.json [results.json abl.json] [-o page.html]');

const suite = read(suiteFile);
const hist = read(histFile);
const abl = read(ablFile);

// Reference tiers from the full suite run at HEAD.
const tiers = {};
for (const r of suite.results ?? []) tiers[`${r.workload}/${r.bench}`] = { ...r.times, sums: r.sums };
const benches = Object.keys(tiers).filter((k) => k !== 'apibench/registry');
const ref = Object.fromEntries(benches.map((k) => [k, k.startsWith('apibench/') ? tiers[k].wine : tiers[k].native]));
const refSum = Object.fromEntries(benches.map((k) => [k, tiers[k].sums?.native ?? tiers[k].sums?.wine]));

const cps = readFileSync(join(here, 'checkpoints.txt'), 'utf8').trim().split('\n').map((l) => {
  const [commit, label, ...rest] = l.split(' ');
  return { commit, label, title: rest.join(' ') };
});

const checkpoints = cps.map((cp) => {
  const r = hist[cp.label];
  if (!r) return { ...cp, missing: true };
  const speed = {};
  for (const k of benches) {
    const t = r.benches?.[k];
    // A wrong checksum is a failed run, not a time.
    const ok = t && (!refSum[k] || r.sums?.[k] === refSum[k]);
    speed[k] = ok ? ref[k] / t : null;
  }
  // Older runtimes name cache files by hash only: count them all (Wine's
  // DLLs plus the four programs, the same set at every checkpoint).
  const codeBytes = Object.values(r.sizes ?? {}).reduce((a, b) => a + b, 0);
  return { ...cp, speed, times: r.benches, coldStart: r.coldStart, warmStart: r.warmStart, codeBytes, errors: r.errors };
});

const ablations = Object.entries(abl).map(([label, r]) => ({ label, title: r.title, times: r.benches, sums: r.sums, errors: r.errors }));

// Profiles: category shares per workload.
const prof = {};
const pd = join(H, 'prof');
if (existsSync(pd)) {
  for (const f of readdirSync(pd)) {
    const text = readFileSync(join(pd, f), 'utf8');
    const cats = {};
    for (const m of text.matchAll(/^\s*([0-9.]+)%\s+\[([^\]]+)\]/gm)) cats[m[2]] = Number(m[1]);
    const total = Number(text.match(/, ([0-9.]+) s sampled/)?.[1]);
    const top = [...text.matchAll(/^\s*([0-9.]+)%\s+([^\[\s].*?)\s*$/gm)].slice(0, 6).map((m) => [Number(m[1]), m[2].replace(/\s+/g, ' ')]);
    prof[f.replace(/\.txt$/, '')] = { cats, total, top };
  }
}

const data = { date: suite.date, node: suite.node, benches, tiers, ref, checkpoints, ablations, prof };
const page = readFileSync(join(here, 'page.template.html'), 'utf8').replace('/*DATA*/null', JSON.stringify(data));
writeFileSync(outFile, page);
console.log(`${outFile}: ${checkpoints.filter((c) => !c.missing).length} checkpoints, ${ablations.length} ablations, ${Object.keys(prof).length} profiles`);

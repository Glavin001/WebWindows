// Reading the output of Wine's Direct3D 9 rendering tests (visual.c, built
// with its test functions numbered by tests/web/d3d9-visual.mjs): failures
// per function. Shared by that test (headless Chromium, native Wine) and the
// page's GPU self-test, which runs the same program on the viewer's GPU.

/**
 * Results of one run of functions a to b-1 (argv "visual a-b") from its
 * output: { name: { status, failures, failed: [first failure lines] } },
 * and `next`, the runs ([a, b] pairs) still to do: the functions after one
 * that crashed or hung, and those whose trace line is not in the output of
 * a run that finished (a long output can lose its start), alone.
 * `exited`: the program exited rather than being stopped for taking too long.
 */
export function parseVisual(text, a, b, names, exited) {
  const results = {};
  const fresh = () => ({ status: 'done', failures: 0, failed: [] });
  // A run of one function is credited with its failures even when the
  // output lost its trace line.
  let current = b - a === 1 ? names[a] : null;
  if (current) results[current] = fresh();
  for (const line of text.split('\n')) {
    const f = /wwt function (\d+) (\w+)/.exec(line);
    if (f) {
      current = f[2];
      results[current] = fresh();
    } else if (current && /Test failed:/.test(line)) {
      const r = results[current];
      r.failures++;
      if (r.failed.length < 8) r.failed.push(line.trim().slice(0, 300));
    }
  }
  // Every function ran when the summary is there (a run stopped for time
  // after it was only slow to exit); otherwise the one it was in hung or
  // crashed, and the rest of the run comes next.
  const finished = /visual: (\d+) tests executed .*?(\d+) failures?\b/.test(text);
  const stuck = !finished && current ? names.indexOf(current) : -1;
  if (stuck >= 0) results[current].status = exited ? 'crash' : 'timeout';
  const next = [];
  if (stuck >= 0 && stuck + 1 < b) next.push([stuck + 1, b]);
  for (let i = a; i < (stuck >= 0 ? stuck : b); i++) {
    if (results[names[i]]) continue;
    results[names[i]] = { status: 'not run' };
    if (b - a > 1 && (finished || stuck >= 0)) next.push([i, i + 1]);
  }
  for (let i = stuck + 1; stuck >= 0 && i < b; i++) results[names[i]] ??= { status: 'not run' };
  return { results, next };
}

/** Totals of a results object: failures, and functions that crashed, hung or did not run. */
export function totals(results) {
  const all = Object.values(results);
  return {
    functions: all.length,
    failures: all.reduce((n, r) => n + (r.failures ?? 0), 0),
    broken: all.filter((r) => r.status !== 'done').length,
  };
}

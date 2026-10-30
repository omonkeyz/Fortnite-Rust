import { spawn, execSync } from 'node:child_process';
import { mkdtempSync, openSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import net from 'node:net';

const here = dirname(fileURLToPath(import.meta.url));
const args = process.argv.slice(2);
const port = Number((args.find((a) => a.startsWith('--port=')) || '--port=8798').split('=')[1]);
const replace = args.includes('--replace');
const stop = args.includes('--stop');
const base = `http://127.0.0.1:${port}`;
const warmupArg = args.find((a) => a.startsWith('--warmup-ms='));
const vars = warmupArg ? ` --var WARMUP_MS:${Number(warmupArg.split('=')[1]) || 0}` : '';

const listening = () => new Promise((res) => {
  const s = net.connect({ host: '127.0.0.1', port }, () => { s.destroy(); res(true); });
  s.on('error', () => res(false));
  s.setTimeout(1500, () => { s.destroy(); res(false); });
});
const healthy = (ms = 3000) => fetch(base + '/health', { signal: AbortSignal.timeout(ms) }).then((r) => r.ok).catch(() => false);
const owners = () => {
  if (process.platform !== 'win32') {
    try { return execSync(`lsof -t -iTCP:${port} -sTCP:LISTEN`, { encoding: 'utf8' }).split(/\s+/).filter(Boolean); } catch (_) { return []; }
  }
  const out = execSync('netstat -ano -p tcp', { encoding: 'utf8' });
  return [...new Set(out.split(/\r?\n/).filter((l) => /LISTENING/.test(l) && new RegExp(`:${port}\\s`).test(l)).map((l) => l.trim().split(/\s+/).pop()))];
};
const parentOf = (pid) => {
  if (process.platform !== 'win32') return null;
  try { return execSync(`powershell -NoProfile -Command "(Get-CimInstance Win32_Process -Filter 'ProcessId=${pid}').ParentProcessId"`, { encoding: 'utf8' }).trim() || null; } catch (_) { return null; }
};
const killTree = (pid) => {
  try {
    if (process.platform === 'win32') execSync(`taskkill /PID ${pid} /T /F`, { stdio: 'ignore' });
    else process.kill(Number(pid), 'SIGKILL');
  } catch (_) {}
};
const clearPort = () => {
  for (const pid of owners()) {
    const parent = parentOf(pid);
    if (parent) killTree(parent);
    killTree(pid);
    console.log(`[relay] stopped the process on port ${port} (pid ${pid}${parent ? ', parent ' + parent : ''})`);
  }
};

if (stop) { clearPort(); process.exit(0); }

if (await listening()) {
  const ok = await healthy();
  const pids = owners();
  if (ok && pids.length === 1 && !replace) { console.log(`[relay] already running and healthy on ${base} (pid ${pids[0]})`); process.exit(0); }
  if (!replace) {
    console.error(`[relay] port ${port} is taken by ${pids.length} listener(s) (${pids.join(', ')}), health ${ok ? 'ok' : 'NOT answering'}.`);
    console.error('[relay] Windows lets a second wrangler bind the same port, so a new relay would be shadowed by the old one. Run with --replace to stop them first.');
    process.exit(1);
  }
  clearPort();
  for (let i = 0; i < 20 && (await listening()); i++) await new Promise((r) => setTimeout(r, 250));
  if (await listening()) { console.error(`[relay] port ${port} is still taken`); process.exit(1); }
}

const state = mkdtempSync(join(tmpdir(), 'fn-mp-state-'));
const logPath = join(tmpdir(), `fn-mp-relay-${port}.log`);
const log = openSync(logPath, 'w');
const child = spawn(`npx --no-install wrangler dev --port ${port} --ip 127.0.0.1 --local --persist-to "${state}"${vars}`, {
  cwd: here, detached: true, stdio: ['ignore', log, log], windowsHide: true, shell: true,
});
child.unref();
writeFileSync(join(tmpdir(), `fn-mp-relay-${port}.pid`), String(child.pid));
for (let i = 0; i < 90; i++) {
  if (await healthy(1500)) {
    const pids = owners();
    if (pids.length !== 1) { console.error(`[relay] up, but ${pids.length} listeners on port ${port}: ${pids.join(', ')}`); process.exit(1); }
    console.log(`[relay] ready on ws://127.0.0.1:${port} (log ${logPath}, state ${state})`);
    process.exit(0);
  }
  await new Promise((r) => setTimeout(r, 1000));
}
console.error(`[relay] did not answer /health within 90 s; see ${logPath}`);
process.exit(1);
